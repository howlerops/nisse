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

/// The fewest holdout sessions a holdout run may be judged on.
const MIN_HOLDOUT_SESSIONS: usize = 20;

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
    /// Whether the build that produced the run had uncommitted changes. `None` when not
    /// recorded.
    pub dirty_build: Option<bool>,
    /// The holdout boundary the run was made under, as `YYYY-MM-DD`.
    pub holdout_start: Option<String>,
    /// The last bar's time, in nanoseconds.
    pub data_end: Option<u64>,
    /// How many of the returns are holdout sessions (the tail of the series).
    pub holdout_sessions: Option<u64>,
    /// How the risk gate left the run at its end: `normal`, `halved` or `flat`.
    pub final_drawdown_mode: Option<String>,
    /// The cost model of the run, as stored (`Null` when absent).
    pub costs: Value,
    /// The symbols of the run, as stored (`Null` when absent).
    pub symbols: Value,
}

impl TrialSeries {
    /// Reads a stored trial. A result without a field (older rows) gives `returns: None`, an
    /// empty snapshot id, zeros and `None`s: such a trial can still be counted but is never
    /// used in statistics (see [`TrialSeries::usable`]).
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
            dirty_build: result.get("dirty_build").and_then(Value::as_bool),
            holdout_start: result
                .get("holdout_start")
                .and_then(Value::as_str)
                .map(str::to_owned),
            data_end: result.get("data_end").and_then(Value::as_u64),
            holdout_sessions: result.get("holdout_sessions").and_then(Value::as_u64),
            final_drawdown_mode: result
                .get("final_drawdown_mode")
                .and_then(Value::as_str)
                .map(str::to_owned),
            costs: result.get("costs").cloned().unwrap_or(Value::Null),
            symbols: result.get("symbols").cloned().unwrap_or(Value::Null),
        }
    }

    /// Whether the trial may be used in statistics, or the fixed reason it may not. A holdout
    /// run needs only a recorded end; an ordinary run must end before the holdout boundary.
    pub fn usable(&self, gates: &Gates) -> Result<(), &'static str> {
        const DATA_END: &str = "its data reaches into the holdout, or its end is not recorded";
        if self.returns.is_none() {
            return Err("no complete return series");
        }
        if self.dirty_build != Some(false) {
            return Err("dirty build, or the build state is not recorded");
        }
        let boundary = gates.holdout_start.format("%Y-%m-%d").to_string();
        if self.holdout_start.as_deref() != Some(boundary.as_str()) {
            return Err("run under a different holdout boundary, or none recorded");
        }
        if self.holdout {
            return if self.data_end.is_some() {
                Ok(())
            } else {
                Err(DATA_END)
            };
        }
        match (self.data_end, boundary_nanos(gates)) {
            (Some(end), Some(start)) if end < start => Ok(()),
            _ => Err(DATA_END),
        }
    }
}

/// The holdout boundary, 00:00:00 UTC on `holdout_start`, in nanoseconds.
fn boundary_nanos(gates: &Gates) -> Option<u64> {
    let nanos = gates
        .holdout_start
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp_nanos_opt()?;
    u64::try_from(nanos).ok()
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
    /// Orders of this version sent beyond the intent. Zero means the risk gate has nothing to
    /// judge.
    pub sent_orders: u64,
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
/// whether or not it has returns or is usable. The statistical gates use it only when it is
/// usable. The PBO matrix is the candidate's returns for its own version plus, for every other
/// version, its newest usable non-holdout trial with returns over the candidate's snapshot,
/// range and length; series are in ascending strategy version id.
#[must_use]
pub fn build_report(inputs: &ReportInputs) -> GateReport {
    let candidate = candidate(inputs);
    let gates = vec![
        trial_budget(inputs),
        deflated_sharpe(inputs, candidate),
        pbo_gate(inputs, candidate),
        holdout_gate(inputs),
        replay_gate(inputs.replay),
        risk_gate(inputs.risk_breaches, inputs.sent_orders),
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
    let sent = scoring::sent_orders(client, version).await?;
    Ok(ReportInputs {
        version,
        gates,
        lifetime_trials: count(lifetime)?,
        holdout_looks: count(holdouts)?,
        trials,
        replay: (count(failed)?, count(scored)?),
        risk_breaches: count(breaches)?,
        sent_orders: count(sent)?,
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

/// The newest non-holdout trial of the version, whether or not it has returns or is usable.
fn candidate(inputs: &ReportInputs) -> Option<&TrialSeries> {
    inputs
        .trials
        .iter()
        .filter(|t| t.strategy_version_id == inputs.version && !t.holdout)
        .max_by_key(|t| t.trial_id)
}

/// The newest holdout run of the version, whether or not it is usable.
fn newest_holdout(inputs: &ReportInputs) -> Option<&TrialSeries> {
    inputs
        .trials
        .iter()
        .filter(|t| t.holdout && t.strategy_version_id == inputs.version)
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

/// A statistical gate's note when its candidate trial cannot be used.
fn cannot_use(reason: &str) -> String {
    format!("the candidate trial cannot be used: {reason}")
}

/// The candidate's returns, when the candidate is usable; otherwise the note saying why not.
fn usable_returns<'a>(candidate: &'a TrialSeries, gates: &Gates) -> Result<&'a [f64], String> {
    candidate.usable(gates).map_err(cannot_use)?;
    candidate
        .returns
        .as_deref()
        .ok_or_else(|| cannot_use("no complete return series"))
}

/// The last `holdout_sessions` returns of a holdout run. `None` when the run has no returns, no
/// recorded session count, fewer than [`MIN_HOLDOUT_SESSIONS`], or more than it has returns.
fn holdout_tail(run: &TrialSeries) -> Option<&[f64]> {
    let returns = run.returns.as_deref()?;
    let sessions = usize::try_from(run.holdout_sessions?).ok()?;
    if sessions < MIN_HOLDOUT_SESSIONS {
        return None;
    }
    returns
        .len()
        .checked_sub(sessions)
        .and_then(|start| returns.get(start..))
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
    let returns = match usable_returns(candidate, &inputs.gates) {
        Ok(returns) => returns,
        Err(note) => return undecided(NAME, None, threshold, &note),
    };

    // One Sharpe per (version, snapshot, range) from usable ordinary runs: the newest of each.
    let mut newest: BTreeMap<(i64, &str, u64, u64), (i64, f64)> = BTreeMap::new();
    for trial in &inputs.trials {
        if trial.holdout || trial.usable(&inputs.gates).is_err() {
            continue;
        }
        let Some(sharpe) = trial.returns.as_deref().and_then(moments::sharpe) else {
            continue;
        };
        let key = (
            trial.strategy_version_id.0,
            trial.data_snapshot_id.as_str(),
            trial.requested_start,
            trial.requested_end,
        );
        let entry = newest.entry(key).or_insert((trial.trial_id, sharpe));
        if trial.trial_id > entry.0 {
            *entry = (trial.trial_id, sharpe);
        }
    }
    let mut by_trial: Vec<(i64, f64)> = newest.into_values().collect();
    by_trial.sort_by_key(|(trial_id, _)| *trial_id);
    let sharpes: Vec<f64> = by_trial.into_iter().map(|(_, sharpe)| sharpe).collect();

    let lifetime = inputs.lifetime_trials;
    if lifetime >= 2 {
        let usable = u64::try_from(sharpes.len()).unwrap_or(u64::MAX);
        if usable.saturating_mul(2) < lifetime {
            return undecided(
                NAME,
                None,
                threshold,
                &format!(
                    "fewer than half the lifetime trials have a usable Sharpe: {usable} of {lifetime}"
                ),
            );
        }
        if moments::variance(&sharpes).is_none_or(|v| v <= 0.0) {
            return undecided(
                NAME,
                None,
                threshold,
                "the trial Sharpes do not vary, so there is nothing to deflate with",
            );
        }
    }

    let minimum = inputs.gates.deflated_sharpe_min;
    match dsr::deflated_sharpe(returns, &sharpes, lifetime) {
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
/// usable non-holdout trial with returns over the same snapshot, range and length, in ascending
/// strategy version id.
fn pbo_matrix<'a>(
    inputs: &'a ReportInputs,
    candidate: &TrialSeries,
    returns: &[f64],
) -> Vec<&'a [f64]> {
    let mut newest: BTreeMap<StrategyVersionId, (i64, &'a [f64])> = BTreeMap::new();
    for trial in &inputs.trials {
        if trial.holdout || trial.usable(&inputs.gates).is_err() {
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
    let threshold = Some(inputs.gates.pbo_max);
    let Some(candidate) = candidate else {
        return undecided(
            NAME,
            None,
            threshold,
            "no backtest trial of this version has a stored result yet",
        );
    };
    let returns = match usable_returns(candidate, &inputs.gates) {
        Ok(returns) => returns,
        Err(note) => return undecided(NAME, None, threshold, &note),
    };
    let matrix = pbo_matrix(inputs, candidate, returns);
    let minimum = inputs.gates.pbo_min_configs;
    let found = matrix.len();
    if found < usize::try_from(minimum).unwrap_or(usize::MAX) {
        return undecided(
            NAME,
            None,
            threshold,
            &format!("needs at least {minimum} configurations over the same data, found {found}"),
        );
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
            format!("PBO {value:.4} against a maximum of {maximum} over {found} configurations"),
        ),
    }
}

fn holdout_gate(inputs: &ReportInputs) -> GateResult {
    const NAME: &str = "holdout";
    let gates = &inputs.gates;
    let max_looks = f64::from(gates.holdout_max_looks);
    let looks = inputs.holdout_looks as f64;
    if looks > max_looks {
        return judged(
            NAME,
            false,
            Some(looks),
            Some(max_looks),
            format!("{looks} holdout runs exceed the {max_looks} allowed; the holdout is retired"),
        );
    }
    let Some(run) = newest_holdout(inputs) else {
        return undecided(NAME, None, None, "no holdout run for this version yet");
    };
    if let Err(reason) = run.usable(gates) {
        return undecided(
            NAME,
            None,
            None,
            &format!("the holdout run cannot be used: {reason}"),
        );
    }
    let Some(tail) = holdout_tail(run) else {
        return undecided(NAME, None, None, "fewer than 20 holdout sessions");
    };
    let ppy = gates.periods_per_year;
    let Some(candidate) = candidate(inputs) else {
        return undecided(
            NAME,
            None,
            None,
            "no backtest trial of this version has a stored result yet",
        );
    };
    let candidate_returns = match usable_returns(candidate, gates) {
        Ok(returns) => returns,
        Err(note) => return undecided(NAME, None, None, &note),
    };
    let Some(candidate_sharpe) = moments::sharpe(candidate_returns) else {
        return undecided(
            NAME,
            None,
            None,
            "the candidate's Sharpe ratio is undefined",
        );
    };
    let threshold =
        ((1.0 - gates.sharpe_haircut) * moments::annualise(candidate_sharpe, ppy)).max(0.0);
    let Some(value) = moments::sharpe(tail).map(|s| moments::annualise(s, ppy)) else {
        return undecided(
            NAME,
            None,
            Some(threshold),
            "the holdout sessions have no Sharpe ratio",
        );
    };
    if run.final_drawdown_mode.as_deref() == Some("flat") {
        return judged(
            NAME,
            false,
            Some(value),
            Some(threshold),
            "the holdout run ended flat after a drawdown".to_owned(),
        );
    }
    judged(
        NAME,
        value >= threshold,
        Some(value),
        Some(threshold),
        format!(
            "holdout Sharpe {value:.4} over the last {} sessions against a haircut Sharpe of {threshold:.4}",
            tail.len()
        ),
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

fn risk_gate(breaches: u64, sent: u64) -> GateResult {
    const NAME: &str = "risk_breaches";
    if sent == 0 {
        return undecided(NAME, None, Some(0.0), "this version has sent no order");
    }
    judged(
        NAME,
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
        "candidate_run": candidate_run(inputs, candidate),
        "holdout_run": holdout_run(inputs),
    })
}

/// How the candidate was run, and whether its statistics may be used. Null without a candidate.
fn candidate_run(inputs: &ReportInputs, candidate: Option<&TrialSeries>) -> Value {
    candidate.map_or(Value::Null, |t| {
        let reason = t.usable(&inputs.gates).err();
        json!({
            "dirty_build": t.dirty_build,
            "costs": t.costs,
            "symbols": t.symbols,
            "requested_start": t.requested_start,
            "requested_end": t.requested_end,
            "data_end": t.data_end,
            "holdout_start": t.holdout_start,
            "usable": reason.is_none(),
            "not_usable_reason": reason,
        })
    })
}

/// The newest holdout run of the version: its annualised Sharpe over the holdout sessions (the
/// figure the holdout gate judges). Null without a holdout run.
fn holdout_run(inputs: &ReportInputs) -> Value {
    let ppy = inputs.gates.periods_per_year;
    newest_holdout(inputs).map_or(Value::Null, |run| {
        let sharpe = holdout_tail(run)
            .and_then(moments::sharpe)
            .map(|s| moments::annualise(s, ppy));
        json!({
            "trial_id": run.trial_id,
            "holdout_sessions": run.holdout_sessions,
            "final_drawdown_mode": run.final_drawdown_mode,
            "sharpe": finite(sharpe),
        })
    })
}
