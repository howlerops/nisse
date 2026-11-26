//! The promotion gate report: for one strategy version, each gate as `pass`, `fail` or
//! `not_evaluated`, with the figures behind it. `not_evaluated` is never a pass.
//!
//! Loading ([`load_inputs`]) and judging ([`build_report`]) are separate, so the judging is
//! pure and tested without a database. Rules: `docs/phases/phase-5.md`, "Gate report".

use std::collections::BTreeMap;

use nisse_core::nautilus::UnixNanos;
use nisse_core::{EvalScore, StrategyVersionId, SubjectType};
use nisse_store::StoreError;
use nisse_store::tables::scoring::{self, TrialWithResult};
use nisse_store::tables::{eval_score, trial_registry};
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

use crate::EvalError;
use crate::gates::Gates;
use crate::stats::bootstrap::{self, BootstrapConfig};
use crate::stats::{cv, dsr, moments, pbo, tracking};

/// The `eval_version` of the scores a report writes.
pub const EVAL_VERSION: &str = "gate-v1";

/// The verdict on one gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Pass,
    Fail,
    NotEvaluated,
}

impl GateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotEvaluated => "not_evaluated",
        }
    }

    /// The score row value: 1 pass, 0 fail, -1 not evaluated.
    const fn score(self) -> f64 {
        match self {
            Self::Pass => 1.0,
            Self::Fail => 0.0,
            Self::NotEvaluated => -1.0,
        }
    }
}

/// One gate's verdict and the figures behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct GateResult {
    pub name: &'static str,
    pub status: GateStatus,
    /// The measured figure, when there is one.
    pub value: Option<f64>,
    /// What it is compared with.
    pub threshold: Option<f64>,
    /// One plain sentence: why it passed, failed or could not be evaluated.
    pub note: String,
}

/// One backtest trial as the report uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialSeries {
    pub trial_id: i64,
    pub strategy_version_id: StrategyVersionId,
    pub holdout: bool,
    /// The daily returns, when the stored result has a complete series (every element a number
    /// and `returns_complete` true).
    pub returns: Option<Vec<f64>>,
    pub data_snapshot_id: String,
    pub requested_start: u64,
    pub requested_end: u64,
    /// The stored summary, passed through to the report.
    pub summary: Value,
}

impl TrialSeries {
    /// Reads a stored trial. A result without a field (older rows) gives `returns: None`, an
    /// empty snapshot id and zeros: such a trial can still be counted but not used in
    /// statistics.
    #[must_use]
    pub fn from_stored(trial: &TrialWithResult) -> Self {
        let result = &trial.result;
        Self {
            trial_id: trial.trial_id,
            strategy_version_id: trial.strategy_version_id,
            holdout: trial.holdout,
            returns: parse_returns(result),
            data_snapshot_id: result
                .get("data_snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            requested_start: result
                .get("requested_start")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            requested_end: result
                .get("requested_end")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary: result.get("summary").cloned().unwrap_or(Value::Null),
        }
    }
}

/// The returns array when the result says it is complete and every element is a number.
fn parse_returns(result: &Value) -> Option<Vec<f64>> {
    if result.get("returns_complete").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    result
        .get("returns")?
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect()
}

/// Everything the report is computed from.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportInputs {
    pub version: StrategyVersionId,
    pub gates: Gates,
    /// Lifetime trial count (`trial_registry`, every kind).
    pub lifetime_trials: u64,
    /// Holdout trials registered, all versions.
    pub holdout_looks: u64,
    /// Every backtest trial with a stored result, in trial id order.
    pub trials: Vec<TrialSeries>,
    /// `(failed, scored)` from `replay_failures`.
    pub replay: (u64, u64),
    pub risk_breaches: u64,
    /// Live and backtest daily returns over the same sessions, when live trading exists.
    pub live: Option<(Vec<f64>, Vec<f64>)>,
}

/// The report for one strategy version.
#[derive(Debug, Clone, PartialEq)]
pub struct GateReport {
    pub version: StrategyVersionId,
    /// The trial the statistical gates were computed on.
    pub candidate_trial_id: Option<i64>,
    pub gates: Vec<GateResult>,
    /// Figures that inform but do not gate.
    pub info: Value,
}

impl GateReport {
    /// Every gate is `Pass`. `NotEvaluated` is never a pass.
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.gates.iter().all(|g| g.status == GateStatus::Pass)
    }

    /// No gate is `Fail` (some may be `NotEvaluated`).
    #[must_use]
    pub fn none_failed(&self) -> bool {
        self.gates.iter().all(|g| g.status != GateStatus::Fail)
    }

    /// The report as JSON. Non-finite numbers are null.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "strategy_version_id": self.version.0,
            "candidate_trial_id": self.candidate_trial_id,
            "all_pass": self.all_pass(),
            "gates": self.gates.iter().map(gate_json).collect::<Vec<Value>>(),
            "info": self.info.clone(),
        })
    }
}

fn gate_json(gate: &GateResult) -> Value {
    json!({
        "name": gate.name,
        "status": gate.status.as_str(),
        "value": finite(gate.value),
        "threshold": finite(gate.threshold),
        "note": gate.note,
    })
}

/// A finite number as JSON, or null.
fn finite(value: Option<f64>) -> Value {
    value
        .filter(|v| v.is_finite())
        .map_or(Value::Null, Value::from)
}

/// Judges every gate. Pure and deterministic: the bootstrap is seeded from
/// `gates.bootstrap_seed`.
///
/// The candidate is the newest (highest trial id) non-holdout trial of `inputs.version`,
/// whether or not it has returns. The PBO matrix is the candidate's returns for its own
/// version plus, for every other version, its newest non-holdout trial with returns over the
/// candidate's snapshot, range and length; series are in ascending strategy version id.
#[must_use]
pub fn build_report(inputs: &ReportInputs) -> GateReport {
    let candidate = candidate(inputs);
    let gates = vec![
        trial_budget(inputs),
        deflated_sharpe(inputs, candidate),
        pbo_gate(inputs, candidate),
        holdout_gate(inputs),
        replay_gate(inputs.replay),
        risk_gate(inputs.risk_breaches),
        tracking_gate(inputs),
    ];
    GateReport {
        version: inputs.version,
        candidate_trial_id: candidate.map(|t| t.trial_id),
        gates,
        info: info(inputs, candidate),
    }
}

/// Loads the inputs for `version` from the store (no live returns yet: `live` is `None`).
pub async fn load_inputs<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    gates: Gates,
) -> Result<ReportInputs, EvalError> {
    let lifetime = trial_registry::lifetime_count(client).await?;
    let holdouts = trial_registry::holdout_count(client).await?;
    let trials = scoring::backtest_results(client)
        .await?
        .iter()
        .map(TrialSeries::from_stored)
        .collect();
    let (failed, scored) = scoring::replay_failures(client, version).await?;
    let breaches = scoring::risk_breaches(client, version).await?;
    Ok(ReportInputs {
        version,
        gates,
        lifetime_trials: count(lifetime)?,
        holdout_looks: count(holdouts)?,
        trials,
        replay: (count(failed)?, count(scored)?),
        risk_breaches: count(breaches)?,
        live: None,
    })
}

/// A database count, which can never be negative: a negative one is a data error.
fn count(value: i64) -> Result<u64, EvalError> {
    u64::try_from(value)
        .map_err(|_| EvalError::Store(StoreError::Data(format!("negative count {value}"))))
}

/// Writes the report as `eval_score` rows in one statement: `gate_<name>_status` (1 pass,
/// 0 fail, -1 not evaluated) for every gate, and `gate_<name>` for every gate with a value.
pub async fn store_report<C: GenericClient>(
    client: &C,
    report: &GateReport,
    now: UnixNanos,
) -> Result<(), EvalError> {
    let subject = report.version.0;
    let mut rows = Vec::with_capacity(report.gates.len() * 2);
    let row = |metric: String, value: f64| EvalScore {
        subject_id: subject,
        subject_type: SubjectType::StrategyVersion,
        metric,
        value,
        eval_version: EVAL_VERSION.to_owned(),
        ts: now,
    };
    for gate in &report.gates {
        rows.push(row(
            format!("gate_{}_status", gate.name),
            gate.status.score(),
        ));
        if let Some(value) = gate.value.filter(|v| v.is_finite()) {
            rows.push(row(format!("gate_{}", gate.name), value));
        }
    }
    eval_score::insert_many(client, &rows).await?;
    Ok(())
}

/// The newest non-holdout trial of the version, whether or not it has returns.
fn candidate(inputs: &ReportInputs) -> Option<&TrialSeries> {
    inputs
        .trials
        .iter()
        .filter(|t| t.strategy_version_id == inputs.version && !t.holdout)
        .max_by_key(|t| t.trial_id)
}

/// A verdict with a value, judged by `pass`.
fn judged(
    name: &'static str,
    pass: bool,
    value: Option<f64>,
    threshold: Option<f64>,
    note: String,
) -> GateResult {
    GateResult {
        name,
        status: if pass {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        value,
        threshold,
        note,
    }
}

fn undecided(
    name: &'static str,
    value: Option<f64>,
    threshold: Option<f64>,
    note: &str,
) -> GateResult {
    GateResult {
        name,
        status: GateStatus::NotEvaluated,
        value,
        threshold,
        note: note.to_owned(),
    }
}

fn trial_budget(inputs: &ReportInputs) -> GateResult {
    let value = inputs.lifetime_trials as f64;
    let threshold = f64::from(inputs.gates.trial_budget);
    judged(
        "trial_budget",
        value <= threshold,
        Some(value),
        Some(threshold),
        format!(
            "{} lifetime trials against a budget of {}",
            inputs.lifetime_trials, inputs.gates.trial_budget
        ),
    )
}

fn deflated_sharpe(inputs: &ReportInputs, candidate: Option<&TrialSeries>) -> GateResult {
    const NAME: &str = "deflated_sharpe";
    let threshold = Some(inputs.gates.deflated_sharpe_min);
    let Some(candidate) = candidate else {
        return undecided(
            NAME,
            None,
            threshold,
            "no backtest trial of this version has a stored result yet",
        );
    };
    let Some(returns) = candidate.returns.as_deref() else {
        return undecided(
            NAME,
            None,
            threshold,
            "the candidate trial has no stored returns (an older result)",
        );
    };
    let trial_sharpes: Vec<f64> = inputs
        .trials
        .iter()
        .filter(|t| !t.holdout)
        .filter_map(|t| t.returns.as_deref())
        .filter_map(moments::sharpe)
        .collect();
    let minimum = inputs.gates.deflated_sharpe_min;
    match dsr::deflated_sharpe(returns, &trial_sharpes, inputs.lifetime_trials) {
        None => undecided(
            NAME,
            None,
            threshold,
            "could not be computed: too few returns, zero variance, or fewer than two trial \
             Sharpes for more than one trial",
        ),
        Some(value) => judged(
            NAME,
            value >= minimum,
            Some(value),
            threshold,
            format!("deflated Sharpe {value:.4} against a minimum of {minimum}"),
        ),
    }
}

/// The candidate's returns (for its own version) and, for every other version, its newest
/// non-holdout trial with returns over the same snapshot, range and length, in ascending
/// strategy version id.
fn pbo_matrix<'a>(
    inputs: &'a ReportInputs,
    candidate: &TrialSeries,
    returns: &[f64],
) -> Vec<&'a [f64]> {
    let mut newest: BTreeMap<StrategyVersionId, (i64, &'a [f64])> = BTreeMap::new();
    for trial in &inputs.trials {
        if trial.holdout {
            continue;
        }
        let Some(series) = trial.returns.as_deref() else {
            continue;
        };
        let same_data = trial.data_snapshot_id == candidate.data_snapshot_id
            && trial.requested_start == candidate.requested_start
            && trial.requested_end == candidate.requested_end
            && series.len() == returns.len();
        if !same_data {
            continue;
        }
        let entry = newest
            .entry(trial.strategy_version_id)
            .or_insert((trial.trial_id, series));
        if trial.trial_id > entry.0 {
            *entry = (trial.trial_id, series);
        }
    }
    newest.into_values().map(|(_, series)| series).collect()
}

fn pbo_gate(inputs: &ReportInputs, candidate: Option<&TrialSeries>) -> GateResult {
    const NAME: &str = "pbo";
    const NEED: &str = "needs at least two configurations over the same data";
    let threshold = Some(inputs.gates.pbo_max);
    let Some(candidate) = candidate else {
        return undecided(NAME, None, threshold, NEED);
    };
    let Some(returns) = candidate.returns.as_deref() else {
        return undecided(NAME, None, threshold, NEED);
    };
    let matrix = pbo_matrix(inputs, candidate, returns);
    if matrix.len() < 2 {
        return undecided(NAME, None, threshold, NEED);
    }
    let maximum = inputs.gates.pbo_max;
    match pbo::pbo(&matrix) {
        None => undecided(
            NAME,
            None,
            threshold,
            "could not be computed: the series need at least 32 sessions and equal lengths",
        ),
        Some(value) => judged(
            NAME,
            value <= maximum,
            Some(value),
            threshold,
            format!(
                "PBO {value:.4} against a maximum of {maximum} over {} configurations",
                matrix.len()
            ),
        ),
    }
}

fn holdout_gate(inputs: &ReportInputs) -> GateResult {
    const NAME: &str = "holdout";
    let threshold = f64::from(inputs.gates.holdout_max_looks);
    let looks = inputs.holdout_looks as f64;
    if looks > threshold {
        return judged(
            NAME,
            false,
            Some(looks),
            Some(threshold),
            format!("{looks} holdout runs exceed the {threshold} allowed; the holdout is retired"),
        );
    }
    let has_run = inputs
        .trials
        .iter()
        .any(|t| t.holdout && t.strategy_version_id == inputs.version);
    if !has_run {
        return undecided(
            NAME,
            Some(looks),
            Some(threshold),
            "no holdout run for this version yet",
        );
    }
    judged(
        NAME,
        true,
        Some(looks),
        Some(threshold),
        format!("{looks} holdout runs against {threshold} allowed; this version has one"),
    )
}

fn replay_gate((failed, scored): (u64, u64)) -> GateResult {
    const NAME: &str = "replay";
    if scored == 0 {
        return undecided(
            NAME,
            None,
            Some(0.0),
            "no decision of this version has a replay score yet",
        );
    }
    judged(
        NAME,
        failed == 0,
        Some(failed as f64),
        Some(0.0),
        format!("{failed} of {scored} scored decisions do not replay to the same decision"),
    )
}

fn risk_gate(breaches: u64) -> GateResult {
    judged(
        "risk_breaches",
        breaches == 0,
        Some(breaches as f64),
        Some(0.0),
        format!("{breaches} orders went beyond the intent after a failed risk check"),
    )
}

fn tracking_gate(inputs: &ReportInputs) -> GateResult {
    const NAME: &str = "tracking";
    let max_error = inputs.gates.tracking_error_max;
    let max_gap = inputs.gates.tracking_gap_max;
    let Some((live, backtest)) = inputs.live.as_ref() else {
        return undecided(NAME, None, Some(max_error), "no live returns yet");
    };
    match tracking::tracking(live, backtest, inputs.gates.periods_per_year) {
        None => undecided(
            NAME,
            None,
            Some(max_error),
            "could not be computed: live and backtest returns must cover the same sessions",
        ),
        Some(t) => judged(
            NAME,
            t.tracking_error <= max_error && t.max_gap <= max_gap,
            Some(t.tracking_error),
            Some(max_error),
            format!(
                "tracking error {:.4} against {max_error} and largest gap {:.4} against {max_gap}",
                t.tracking_error, t.max_gap
            ),
        ),
    }
}

/// An annualised per-period ratio as a statistic for the bootstrap.
fn annualised(
    stat: fn(&[f64]) -> Option<f64>,
    periods_per_year: u32,
) -> impl Fn(&[f64]) -> Option<f64> {
    move |returns: &[f64]| stat(returns).map(|s| moments::annualise(s, periods_per_year))
}

/// A `[lower, upper]` pair as JSON, or null.
fn pair(interval: Option<(f64, f64)>) -> Value {
    interval.map_or(Value::Null, |(lower, upper)| json!([lower, upper]))
}

/// The annualised Sharpe of each purged fold's test range, or null when the folds cannot be
/// built.
fn fold_sharpes(returns: &[f64], gates: &Gates) -> Value {
    let embargo = gates.cv_embargo_sessions as usize;
    match cv::folds(returns.len(), gates.cv_folds as usize, embargo, embargo) {
        Ok(folds) => Value::Array(
            folds
                .iter()
                .map(|fold| {
                    finite(
                        returns
                            .get(fold.test.clone())
                            .and_then(moments::sharpe)
                            .map(|s| moments::annualise(s, gates.periods_per_year)),
                    )
                })
                .collect(),
        ),
        Err(_) => Value::Null,
    }
}

/// The figures that inform but do not gate. Those computed from the candidate's returns are
/// null when there is no candidate or no returns.
fn info(inputs: &ReportInputs, candidate: Option<&TrialSeries>) -> Value {
    let gates = &inputs.gates;
    let ppy = gates.periods_per_year;
    let returns = candidate.and_then(|t| t.returns.as_deref());
    let config = BootstrapConfig::with_seed(gates.bootstrap_seed);
    let sharpe = returns
        .and_then(moments::sharpe)
        .map(|s| moments::annualise(s, ppy));
    let haircut = sharpe.map(|s| ((1.0 - gates.sharpe_haircut) * s).max(0.0));
    let sharpe_interval =
        returns.and_then(|r| bootstrap::interval(r, annualised(moments::sharpe, ppy), &config));
    let sortino_interval =
        returns.and_then(|r| bootstrap::interval(r, annualised(moments::sortino, ppy), &config));
    let expectancy_interval = returns.and_then(|r| bootstrap::interval(r, moments::mean, &config));
    json!({
        "summary": candidate.map_or(Value::Null, |t| t.summary.clone()),
        "sharpe": finite(sharpe),
        "sortino": finite(
            returns
                .and_then(moments::sortino)
                .map(|s| moments::annualise(s, ppy))
        ),
        "expectancy": finite(returns.and_then(moments::mean)),
        "max_drawdown": finite(returns.and_then(moments::max_drawdown)),
        "sharpe_interval": pair(sharpe_interval),
        "sortino_interval": pair(sortino_interval),
        "expectancy_interval": pair(expectancy_interval),
        "fold_sharpes": returns.map_or(Value::Null, |r| fold_sharpes(r, gates)),
        "haircut_sharpe": finite(haircut),
        "haircut_sharpe_note": "the limits file's haircut_sharpe may not exceed this",
        "lifetime_trials": inputs.lifetime_trials,
        "trial_budget": gates.trial_budget,
        "candidate_trial_id": candidate.map(|t| t.trial_id),
    })
}
