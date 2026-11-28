//! The promotion gate report: for one strategy version, each gate as `pass`, `fail` or
//! `not_evaluated`, with the figures behind it. `not_evaluated` is never a pass.
//!
//! Loading ([`load_inputs`]) and judging ([`build_report`]) are separate, so the judging is
//! pure and tested without a database. Rules: `docs/phases/phase-5.md`, "Gate report".

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::NaiveDate;
use nisse_core::nautilus::UnixNanos;
use nisse_core::{EvalScore, StrategyVersionId, SubjectType};
use nisse_store::StoreError;
use nisse_store::tables::scoring::{self, TrialWithResult};
use nisse_store::tables::{account_equity, eval_score, trial_registry};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

use crate::EvalError;
use crate::gates::Gates;
use crate::stats::bootstrap::{self, BootstrapConfig};
use crate::stats::{cv, dsr, moments, pbo, tracking};

/// The `eval_version` of the scores a report writes.
pub const EVAL_VERSION: &str = "gate-v1";

/// How many distinct rules the risk gate writes for every order: an order with fewer on
/// record was sent without the full set of checks. Taken from the risk crate so it cannot
/// drift from the gate.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "a handful of rules"
)]
const GATE_RULES: i64 = nisse_risk::rule::ALL.len() as i64;

/// The fewest holdout sessions a holdout run may be judged on.
pub const MIN_HOLDOUT_SESSIONS: u64 = 60;

/// The fewest sessions with a non-zero return the holdout baseline may have. A short baseline has
/// a noisy Sharpe ratio, so a threshold drawn from it would be arbitrary. The flat warm-up is not
/// counted.
pub const MIN_BASELINE_SESSIONS: usize = 252;

/// The environment whose account equity the tracking gate reads first. Paper is read only when
/// it has no live rows.
const LIVE_ENVIRONMENT: &str = "live";

/// The environment the tracking gate falls back to when the live environment has no equity rows.
const PAPER_ENVIRONMENT: &str = "paper";

/// Fewest paired sessions the tracking gate judges. Two flat series a few days long would
/// otherwise pass.
pub const MIN_TRACKING_SESSIONS: usize = 20;

/// Most equity rows after a tracking run's last session before the run is too old to judge by.
pub const MAX_TRACKING_LAG_SESSIONS: usize = 5;

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
    /// The hash of the risk limits file the run used. `None` when not recorded.
    pub limits_hash: Option<String>,
    /// The starting cash of the run, as a decimal string. `None` when not recorded.
    pub starting_cash: Option<String>,
    /// The seed of the run. `None` when not recorded.
    pub seed: Option<u64>,
    /// The bar file the run read. `None` when not recorded (or no file was named).
    pub bars_file: Option<String>,
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
            limits_hash: stored_text(result, "limits_hash"),
            starting_cash: stored_text(result, "starting_cash"),
            seed: result.get("seed").and_then(Value::as_u64),
            bars_file: stored_text(result, "bars_file"),
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
        let boundary = gates.holdout_start_text();
        if self.holdout_start.as_deref() != Some(boundary.as_str()) {
            return Err("run under a different holdout boundary, or none recorded");
        }
        let Some(start) = gates.holdout_boundary() else {
            return Err("the holdout boundary is out of range");
        };
        if self.holdout {
            return if self.data_end.is_some() {
                Ok(())
            } else {
                Err(DATA_END)
            };
        }
        match self.data_end {
            Some(end) if end < start.as_u64() => Ok(()),
            _ => Err(DATA_END),
        }
    }
}

/// A stored string field, or `None` when absent or not a string.
fn stored_text(result: &Value, key: &str) -> Option<String> {
    result.get(key).and_then(Value::as_str).map(str::to_owned)
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
    /// What the tracking gate judges. `None` when there is no tracking run or fewer than two
    /// equity rows (see [`load_inputs`]).
    pub live: Option<TrackingInput>,
}

/// What the tracking gate judges: the engine's daily returns and a tracking run's over the same
/// sessions, and what is needed to say whether the comparison is sound.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingInput {
    /// `paper` or `live`: whose equity was read.
    pub environment: &'static str,
    /// The paired returns (equal lengths, session by session). Empty when nothing could be paired.
    pub live: Vec<f64>,
    pub backtest: Vec<f64>,
    /// First and last paired session. `None` when nothing was paired.
    pub span: Option<(NaiveDate, NaiveDate)>,
    /// The newest session that the tracking run has and the equity series lacks (or the reverse),
    /// if any: sessions up to it are left out of the comparison.
    pub gap: Option<NaiveDate>,
    /// Equity rows dated after the tracking run's last session.
    pub lag_sessions: usize,
    /// Whether the tracking run has the same inputs as the version's candidate backtest.
    pub same_experiment: bool,
    /// Why the series could not be read at all (a malformed or incomplete tracking result).
    pub unreadable: bool,
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
/// range and length and with the candidate's inputs; series are in ascending strategy version id.
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

/// Loads the inputs for `version` from the store. The tracking input (see [`TrackingInput`])
/// compares the daily equity (the `live` environment's rows if it has any, else `paper`'s) with
/// the newest tracking run, paired by [`pair_tracking`]. It is `None` when there is no tracking
/// run or fewer than two equity rows. A tracking run whose result is incomplete or malformed
/// gives `unreadable: true`, so the tracking gate says it could not be computed. The candidate
/// is found from the trials already loaded, not queried again.
pub async fn load_inputs<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    gates: Gates,
) -> Result<ReportInputs, EvalError> {
    let lifetime = trial_registry::lifetime_count(client).await?;
    let holdouts = trial_registry::holdout_count(client).await?;
    let trials: Vec<TrialSeries> = scoring::backtest_results(client)
        .await?
        .iter()
        .map(TrialSeries::from_stored)
        .collect();
    let (failed, scored) = scoring::replay_failures(client, version).await?;
    let breaches = scoring::risk_breaches(client, version, GATE_RULES).await?;
    let sent = scoring::sent_orders(client, version).await?;
    let (environment, equity) = {
        let live = account_equity::daily(client, version, LIVE_ENVIRONMENT).await?;
        if live.is_empty() {
            let paper = account_equity::daily(client, version, PAPER_ENVIRONMENT).await?;
            (PAPER_ENVIRONMENT, paper)
        } else {
            (LIVE_ENVIRONMENT, live)
        }
    };
    let live = match scoring::tracking_result(client, version).await? {
        None => None,
        Some(run) => tracking_input(
            &run,
            environment,
            &equity,
            newest_candidate(&trials, version),
        ),
    };
    Ok(ReportInputs {
        version,
        gates,
        lifetime_trials: count(lifetime)?,
        holdout_looks: count(holdouts)?,
        trials,
        replay: (count(failed)?, count(scored)?),
        risk_breaches: count(breaches)?,
        sent_orders: count(sent)?,
        live,
    })
}

/// The tracking input of one tracking run against the equity of `environment`. `None` when there
/// are fewer than two equity rows. `candidate` is the version's candidate trial, if any.
fn tracking_input(
    run: &TrialWithResult,
    environment: &'static str,
    equity: &[(NaiveDate, Decimal)],
    candidate: Option<&TrialSeries>,
) -> Option<TrackingInput> {
    if equity.len() < 2 {
        return None;
    }
    let same_experiment = candidate.is_some_and(|c| same_inputs(c, &TrialSeries::from_stored(run)));
    let parsed = tracking_series(&run.result);
    // Every equity row is newer than a run with no session at all.
    let lag_sessions = match parsed.as_ref().and_then(|(dates, _)| dates.last()) {
        Some(last) => equity.iter().filter(|(day, _)| day > last).count(),
        None => equity.len(),
    };
    let (live, backtest, span, gap, unreadable) = match parsed {
        None => (Vec::new(), Vec::new(), None, None, true),
        Some((dates, returns)) => {
            let pairing = pair_tracking(equity, &dates, &returns)?;
            (
                pairing.live,
                pairing.backtest,
                pairing.span,
                pairing.gap,
                pairing.malformed,
            )
        }
    };
    Some(TrackingInput {
        environment,
        live,
        backtest,
        span,
        gap,
        lag_sessions,
        same_experiment,
        unreadable,
    })
}

/// The tracking run's session dates and returns, when its result is complete, every return is a
/// number and every date is `%Y-%m-%d`. Otherwise `None`.
fn tracking_series(result: &Value) -> Option<(Vec<NaiveDate>, Vec<f64>)> {
    let returns = parse_returns(result)?;
    let dates = result
        .get("session_dates")?
        .as_array()?
        .iter()
        .map(|day| {
            day.as_str()
                .and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
        })
        .collect::<Option<Vec<NaiveDate>>>()?;
    Some((dates, returns))
}

/// The paired returns of a tracking run and the equity series, as [`pair_tracking`] gives them.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingPairing {
    /// The engine's returns, one per paired session, ascending.
    pub live: Vec<f64>,
    /// The tracking run's returns for the same sessions.
    pub backtest: Vec<f64>,
    /// First and last paired session. `None` when nothing was paired.
    pub span: Option<(NaiveDate, NaiveDate)>,
    /// The newest session the two series do not both have, if any.
    pub gap: Option<NaiveDate>,
    /// The inputs are malformed (see [`pair_tracking`]); the series are then empty.
    pub malformed: bool,
}

impl TrackingPairing {
    /// Drops every pair so far: the sessions up to `gap` are left out.
    fn leave_out(&mut self, gap: NaiveDate) {
        self.live.clear();
        self.backtest.clear();
        self.span = None;
        self.gap = Some(gap);
    }

    fn push(&mut self, day: NaiveDate, live: f64, backtest: f64) {
        self.live.push(live);
        self.backtest.push(backtest);
        self.span = Some(match self.span {
            Some((first, _)) => (first, day),
            None => (day, day),
        });
    }
}

/// Lines the engine's daily returns up with a tracking run's, session by session.
///
/// `equity` is the account's equity per session, ascending by date. The live return of a session
/// is `equity / previous equity row's equity - 1`. `dates` and `returns` are the tracking run's
/// (one date per return, ascending). The comparison covers the sessions up to the earlier of the
/// last equity row and the last tracking date.
///
/// A session pairs when its equity row and its tracking date are the same session, the tracking
/// run's session before it is the previous equity row, and the previous equity is positive with a
/// finite return. A session that cannot be paired is a gap: its date is recorded as `gap`, every
/// pair so far is dropped, and pairing resumes from the next equity row. The result is therefore
/// the longest gap-free run ending at the end of the span, and `gap` is the newest gap. Gaps are:
/// a tracking session the equity lacks (recorded as that session), an equity row the tracking run
/// lacks, a previous equity row the tracking run lacks, and a previous equity that is not positive
/// or a return that is not finite (recorded as the row's session).
///
/// Returns `None` only when there are fewer than two equity rows. Malformed input (`dates` and
/// `returns` differ in length, `dates` not strictly ascending, equity not strictly ascending by
/// date) gives empty series with `malformed` set. No tracking date at all gives empty series and
/// no gap. Runs in O(n log n) with the vectors sized up front.
#[must_use]
pub fn pair_tracking(
    equity: &[(NaiveDate, Decimal)],
    dates: &[NaiveDate],
    returns: &[f64],
) -> Option<TrackingPairing> {
    if equity.len() < 2 {
        return None;
    }
    if dates.len() != returns.len()
        || dates.windows(2).any(|w| w[0] >= w[1])
        || equity.windows(2).any(|w| w[0].0 >= w[1].0)
    {
        return Some(TrackingPairing {
            live: Vec::new(),
            backtest: Vec::new(),
            span: None,
            gap: None,
            malformed: true,
        });
    }
    let Some(&last_tracked) = dates.last() else {
        return Some(TrackingPairing {
            live: Vec::new(),
            backtest: Vec::new(),
            span: None,
            gap: None,
            malformed: false,
        });
    };
    let end = equity[equity.len() - 1].0.min(last_tracked);
    let capacity = (equity.len() - 1).min(dates.len());
    let mut out = TrackingPairing {
        live: Vec::with_capacity(capacity),
        backtest: Vec::with_capacity(capacity),
        span: None,
        gap: None,
        malformed: false,
    };
    let mut last_day = equity[0].0;
    for (index, &(day, today)) in equity.iter().enumerate().skip(1) {
        if day > end {
            break;
        }
        let (previous_day, previous_equity) = equity[index - 1];
        // The tracking sessions on or before the previous equity row, and those before `day`.
        let known = dates.partition_point(|tracked| *tracked <= previous_day);
        let before = dates.partition_point(|tracked| *tracked < day);
        if known < before {
            // Tracking sessions between the two equity rows: the equity series lacks them.
            out.leave_out(dates[before - 1]);
        } else if dates.get(before) != Some(&day) {
            // An equity row the tracking run lacks.
            out.leave_out(day);
        } else {
            // The tracking session before `day` must be the previous equity row, or the two
            // returns span different sessions. Before the first equity row of the span there
            // may be none (a run that starts later).
            let predecessor_ok = match before.checked_sub(1).map(|i| dates[i]) {
                Some(tracked) => tracked == previous_day,
                None => index == 1,
            };
            if !predecessor_ok {
                out.leave_out(previous_day);
            } else if let Some(live) = live_return(today, previous_equity) {
                out.push(day, live, returns[before]);
            } else {
                out.leave_out(day);
            }
        }
        last_day = day;
    }
    // Tracking sessions after the last equity row walked, up to `end`, have no equity row.
    let upto = dates.partition_point(|tracked| *tracked <= end);
    let from = dates.partition_point(|tracked| *tracked <= last_day);
    if upto > from {
        out.leave_out(dates[upto - 1]);
    }
    Some(out)
}

/// The return of `equity` over `previous`, in `Decimal` and converted to `f64` last. `None` when
/// `previous` is not positive, or the result is not finite.
fn live_return(equity: Decimal, previous: Decimal) -> Option<f64> {
    if previous <= Decimal::ZERO {
        return None;
    }
    let ratio = equity.checked_div(previous)?.checked_sub(Decimal::ONE)?;
    ratio.to_f64().filter(|r| r.is_finite())
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
    newest_candidate(&inputs.trials, inputs.version)
}

/// The newest non-holdout trial of `version` in `trials` (see [`candidate`]).
fn newest_candidate(trials: &[TrialSeries], version: StrategyVersionId) -> Option<&TrialSeries> {
    trials
        .iter()
        .filter(|t| t.strategy_version_id == version && !t.holdout)
        .max_by_key(|t| t.trial_id)
}

/// Whether two runs were made with the same inputs: both have a cost model and a symbol list,
/// the cost objects have the same keys and numerically equal decimal values (`"2"` and `"2.0"`
/// are equal; a value that is not a decimal string is not), and the symbol arrays hold the same
/// set of strings (order and duplicates ignored; a non-string element is not). The result also
/// depends on the risk limits, the starting cash and the seed, so those must be recorded and
/// equal on both sides; a missing value is never equal.
fn same_inputs(a: &TrialSeries, b: &TrialSeries) -> bool {
    same_costs(&a.costs, &b.costs)
        && same_symbols(&a.symbols, &b.symbols)
        && same_present(&a.limits_hash, &b.limits_hash)
        && same_starting_cash(a.starting_cash.as_deref(), b.starting_cash.as_deref())
        && same_present(&a.seed, &b.seed)
}

/// Both present and equal.
fn same_present<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x == y)
}

/// Both decimal strings and numerically equal (`"100000"` and `"100000.0"`); a value that is not
/// a decimal is not equal to anything.
fn same_starting_cash(a: Option<&str>, b: Option<&str>) -> bool {
    let parse = |text: Option<&str>| text.and_then(|s| Decimal::from_str(s).ok());
    matches!((parse(a), parse(b)), (Some(x), Some(y)) if x == y)
}

fn same_costs(a: &Value, b: &Value) -> bool {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
        return false;
    };
    a.len() == b.len()
        && a.iter()
            .all(|(key, value)| b.get(key).is_some_and(|other| same_decimal(value, other)))
}

/// Two JSON values that are decimal strings and numerically equal.
fn same_decimal(a: &Value, b: &Value) -> bool {
    let parse = |value: &Value| value.as_str().and_then(|s| Decimal::from_str(s).ok());
    matches!((parse(a), parse(b)), (Some(x), Some(y)) if x == y)
}

fn same_symbols(a: &Value, b: &Value) -> bool {
    matches!((symbol_set(a), symbol_set(b)), (Some(x), Some(y)) if x == y)
}

/// The set of symbols of a symbol array, or `None` when it is not an array of strings.
fn symbol_set(value: &Value) -> Option<BTreeSet<&str>> {
    value.as_array()?.iter().map(Value::as_str).collect()
}

/// The holdout run judged by the gate: the version's oldest holdout run, whatever its inputs.
/// The first look counts; no later look, re-run or change of costs can replace it.
fn judged_holdout_run(inputs: &ReportInputs) -> Option<&TrialSeries> {
    inputs
        .trials
        .iter()
        .filter(|t| t.holdout && t.strategy_version_id == inputs.version)
        .min_by_key(|t| t.trial_id)
}

/// The holdout baseline of `run`: the newest usable ordinary run of the version registered before
/// `run`, with the same inputs as `run`, the same bar file and the same requested start, and whose
/// returns are a strict prefix of the returns of `run` (see [`extends_returns`]). With the same
/// inputs and the same bars before the boundary a deterministic engine gives the same returns
/// session for session, so a prefix mismatch means the bars or an unrecorded input differed. It
/// depends only on what existed when the look was made, so nothing registered later can change it.
fn holdout_baseline<'a>(inputs: &'a ReportInputs, run: &TrialSeries) -> Option<&'a TrialSeries> {
    inputs
        .trials
        .iter()
        .filter(|t| {
            !t.holdout
                && t.strategy_version_id == inputs.version
                && t.trial_id < run.trial_id
                && t.usable(&inputs.gates).is_ok()
                && same_inputs(t, run)
                && same_present(&t.bars_file, &run.bars_file)
                && t.requested_start == run.requested_start
                && extends_returns(run, t)
        })
        .max_by_key(|t| t.trial_id)
}

/// Whether the returns of `base` are a strict prefix of the returns of `run`: both are present,
/// `run` has more, and the first `base.len()` elements are equal. Exact equality, since both series
/// were rounded the same way by the job.
fn extends_returns(run: &TrialSeries, base: &TrialSeries) -> bool {
    match (base.returns.as_deref(), run.returns.as_deref()) {
        (Some(b), Some(r)) => r.len() > b.len() && r.get(..b.len()) == Some(b),
        _ => false,
    }
}

/// Whether the returns of `a` and `b` are both present and equal, element for element.
fn same_returns(a: &TrialSeries, b: &TrialSeries) -> bool {
    matches!(
        (a.returns.as_deref(), b.returns.as_deref()),
        (Some(x), Some(y)) if x == y
    )
}

/// Whether two runs cover the same experiment: the same non-empty data snapshot and requested
/// range, with the same inputs (see [`same_inputs`]).
fn same_experiment(a: &TrialSeries, b: &TrialSeries) -> bool {
    !a.data_snapshot_id.is_empty()
        && a.data_snapshot_id == b.data_snapshot_id
        && a.requested_start == b.requested_start
        && a.requested_end == b.requested_end
        && same_inputs(a, b)
}

/// The annualised Sharpe of a run's returns from the first non-zero return on, or `None` when
/// it has no returns or none is defined.
fn baseline_active_sharpe(trial: &TrialSeries, periods_per_year: u32) -> Option<f64> {
    trial
        .returns
        .as_deref()
        .and_then(|returns| moments::sharpe(active_returns(returns)))
        .map(|s| moments::annualise(s, periods_per_year))
}

/// The number of holdout trials the version has, whatever their costs.
fn holdout_runs_for_version(inputs: &ReportInputs) -> usize {
    inputs
        .trials
        .iter()
        .filter(|t| t.holdout && t.strategy_version_id == inputs.version)
        .count()
}

/// A run's returns from its first non-zero return on. The flat warm-up before the first
/// trade would understate its Sharpe ratio.
fn active_returns(returns: &[f64]) -> &[f64] {
    let start = returns
        .iter()
        .position(|r| *r != 0.0)
        .unwrap_or(returns.len());
    returns.get(start..).unwrap_or_default()
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

/// The last `holdout_sessions` returns of a holdout run. An error (the note) when the run has no
/// returns, no recorded session count or fewer than [`MIN_HOLDOUT_SESSIONS`], or more sessions
/// than returns.
fn holdout_tail(run: &TrialSeries) -> Result<&[f64], String> {
    let too_few = || format!("fewer than {MIN_HOLDOUT_SESSIONS} holdout sessions");
    let returns = run.returns.as_deref().ok_or_else(too_few)?;
    let sessions = run
        .holdout_sessions
        .filter(|sessions| *sessions >= MIN_HOLDOUT_SESSIONS)
        .ok_or_else(too_few)?;
    let sessions = usize::try_from(sessions).map_err(|_| too_few())?;
    returns
        .len()
        .checked_sub(sessions)
        .and_then(|start| returns.get(start..))
        .ok_or_else(|| "more holdout sessions than the holdout run has returns".to_owned())
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

    // One Sharpe per experiment (see `same_experiment`) from usable ordinary runs: the newest of
    // each. Newest first, a run is kept unless a kept run of its version is the same experiment
    // or has exactly the same returns. The second test matters: a re-run that changes only an
    // input the strategy ignores (the seed, for one) is the same result again, and counting it
    // as a new trial Sharpe would shrink their variance and so flatter the deflated Sharpe.
    let mut usable: Vec<&TrialSeries> = inputs
        .trials
        .iter()
        .filter(|t| !t.holdout && t.usable(&inputs.gates).is_ok())
        .collect();
    usable.sort_by_key(|t| Reverse(t.trial_id));
    let mut kept: Vec<&TrialSeries> = Vec::new();
    let mut sharpes: Vec<f64> = Vec::new();
    for trial in usable {
        let Some(sharpe) = trial.returns.as_deref().and_then(moments::sharpe) else {
            continue;
        };
        let repeat = kept.iter().any(|k| {
            k.strategy_version_id == trial.strategy_version_id
                && (same_experiment(k, trial) || same_returns(k, trial))
        });
        if !repeat {
            kept.push(trial);
            sharpes.push(sharpe);
        }
    }
    // Oldest first, as the trials were registered.
    sharpes.reverse();

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
/// usable non-holdout trial with returns over the same snapshot, range and length and with the
/// candidate's inputs (see [`same_inputs`]), in ascending strategy version id.
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
        // The candidate stands for its own version; another version enters only with the same inputs.
        let own_version = trial.strategy_version_id == candidate.strategy_version_id;
        if !same_data || !(own_version || same_inputs(trial, candidate)) {
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
    let ppy = gates.periods_per_year;
    // (b) and (c): nothing to judge yet.
    let Some(candidate) = candidate(inputs) else {
        return undecided(
            NAME,
            None,
            None,
            "no backtest trial of this version has a stored result yet",
        );
    };
    let Some(run) = judged_holdout_run(inputs) else {
        return undecided(NAME, None, None, "no holdout run for this version yet");
    };
    // (d): the candidate must be what the holdout was looked at with.
    if !same_inputs(candidate, run) {
        return undecided(
            NAME,
            None,
            None,
            "the candidate's inputs (costs, symbols, limits, starting cash or seed) differ from those the holdout was looked at with",
        );
    }
    // (e) and (f): a run that cannot be used, or that went flat, is not judged on its Sharpe.
    if let Err(reason) = run.usable(gates) {
        return undecided(
            NAME,
            None,
            None,
            &format!("the holdout run cannot be used: {reason}"),
        );
    }
    // Before any Sharpe: a run that went flat has zero returns and no Sharpe ratio. A run whose
    // final mode is not recorded cannot be told apart from one that went flat.
    if run.final_drawdown_mode.is_none() {
        return undecided(
            NAME,
            None,
            None,
            "the holdout run's final drawdown mode is not recorded",
        );
    }
    if run.final_drawdown_mode.as_deref() == Some("flat") {
        return judged(
            NAME,
            false,
            None,
            None,
            "the holdout run ended flat after a drawdown".to_owned(),
        );
    }
    // (g): the holdout sessions.
    let tail = match holdout_tail(run) {
        Ok(tail) => tail,
        Err(note) => return undecided(NAME, None, None, &note),
    };
    // (h) and (i): the threshold comes from the baseline, which the look fixed.
    let Some(baseline) = holdout_baseline(inputs, run) else {
        return undecided(
            NAME,
            None,
            None,
            "no usable ordinary run before the first holdout look has its inputs, start date, bar file and the same returns up to its end",
        );
    };
    if !same_experiment(candidate, baseline) || !same_returns(candidate, baseline) {
        return undecided(
            NAME,
            None,
            None,
            "the holdout look tested another experiment than the candidate (the last ordinary run before the look)",
        );
    }
    // (j1): a baseline that ended flat after a drawdown has nothing to set a threshold, and one
    // whose final mode is not recorded cannot be told apart from one that did.
    if baseline.final_drawdown_mode.is_none() {
        return undecided(
            NAME,
            None,
            None,
            "the holdout baseline's final drawdown mode is not recorded",
        );
    }
    if baseline.final_drawdown_mode.as_deref() == Some("flat") {
        return undecided(
            NAME,
            None,
            None,
            "the holdout baseline ended flat after a drawdown",
        );
    }
    // (j2): a short baseline has a noisy Sharpe ratio, so it cannot set a threshold. Only
    // sessions with a non-zero return count: a run that idles through zeros is not a long run.
    let non_zero_sessions = baseline
        .returns
        .as_deref()
        .map_or(0, |returns| returns.iter().filter(|r| **r != 0.0).count());
    if non_zero_sessions < MIN_BASELINE_SESSIONS {
        return undecided(
            NAME,
            None,
            None,
            &format!(
                "the holdout baseline has fewer than {MIN_BASELINE_SESSIONS} sessions with a non-zero return"
            ),
        );
    }
    // (k) and (l): a zero or negative threshold would pass anything.
    let Some(baseline_sharpe) = baseline_active_sharpe(baseline, ppy) else {
        return undecided(
            NAME,
            None,
            None,
            "the holdout baseline's active Sharpe ratio is undefined",
        );
    };
    if baseline_sharpe <= 0.0 {
        return undecided(
            NAME,
            None,
            None,
            "the holdout baseline's Sharpe is not positive, so there is nothing for the holdout to hold up",
        );
    }
    let threshold = (1.0 - gates.sharpe_haircut) * baseline_sharpe;
    // (m): the holdout sessions have a Sharpe ratio.
    let Some(value) = moments::sharpe(tail).map(|s| moments::annualise(s, ppy)) else {
        return undecided(
            NAME,
            None,
            Some(threshold),
            "the holdout sessions have no Sharpe ratio",
        );
    };
    // (n).
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

/// The tracking gate's note sentence for a gap, or nothing when there was none.
fn gap_sentence(gap: Option<NaiveDate>) -> String {
    gap.map_or_else(String::new, |day| {
        format!(
            "; sessions up to {day} are left out: the equity and tracking series do not both have {day}"
        )
    })
}

/// The first and last paired session as `first to last`, or `no sessions`.
fn span_text(span: Option<(NaiveDate, NaiveDate)>) -> String {
    span.map_or_else(
        || "no sessions".to_owned(),
        |(first, last)| format!("{first} to {last}"),
    )
}

/// Judges the tracking gate in this order: no live returns, unreadable, not the same experiment,
/// stale, too few sessions, then the tracking error and largest gap.
fn tracking_gate(inputs: &ReportInputs) -> GateResult {
    const NAME: &str = "tracking";
    let max_error = inputs.gates.tracking_error_max;
    let max_gap = inputs.gates.tracking_gap_max;
    let Some(input) = inputs.live.as_ref() else {
        return undecided(NAME, None, Some(max_error), "no live returns yet");
    };
    if input.unreadable {
        return undecided(
            NAME,
            None,
            Some(max_error),
            "could not be computed: the tracking run's result is incomplete or malformed",
        );
    }
    if !input.same_experiment {
        return undecided(
            NAME,
            None,
            Some(max_error),
            "the tracking run does not have the same inputs as the candidate backtest (costs, symbols, limits, starting cash, seed)",
        );
    }
    if input.lag_sessions > MAX_TRACKING_LAG_SESSIONS {
        return undecided(
            NAME,
            None,
            Some(max_error),
            &format!(
                "the tracking run is {} sessions older than the newest equity row; make a new tracking run",
                input.lag_sessions
            ),
        );
    }
    let sessions = input.live.len();
    if sessions < MIN_TRACKING_SESSIONS {
        return undecided(
            NAME,
            None,
            Some(max_error),
            &format!(
                "{sessions} paired sessions; at least {MIN_TRACKING_SESSIONS} are needed{}",
                gap_sentence(input.gap)
            ),
        );
    }
    match tracking::tracking(&input.live, &input.backtest, inputs.gates.periods_per_year) {
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
                "{} equity, {}, {sessions} sessions: tracking error {:.4} against {max_error} and largest gap {:.4} against {max_gap}{}",
                input.environment,
                span_text(input.span),
                t.tracking_error,
                t.max_gap,
                gap_sentence(input.gap)
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
    let active_sharpe = returns
        .map(active_returns)
        .and_then(moments::sharpe)
        .map(|s| moments::annualise(s, ppy));
    let sharpe_interval =
        returns.and_then(|r| bootstrap::interval(r, annualised(moments::sharpe, ppy), &config));
    let sortino_interval =
        returns.and_then(|r| bootstrap::interval(r, annualised(moments::sortino, ppy), &config));
    let expectancy_interval = returns.and_then(|r| bootstrap::interval(r, moments::mean, &config));
    let baseline = judged_holdout_run(inputs).and_then(|run| holdout_baseline(inputs, run));
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
        "active_sharpe": finite(active_sharpe),
        "holdout_baseline_active_sharpe": finite(
            baseline.and_then(|baseline| baseline_active_sharpe(baseline, ppy))
        ),
        "lifetime_trials": inputs.lifetime_trials,
        "trial_budget": gates.trial_budget,
        "candidate_trial_id": candidate.map(|t| t.trial_id),
        "candidate_run": candidate_run(inputs, candidate),
        "holdout_run": holdout_run(inputs, baseline),
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
            "limits_hash": t.limits_hash,
            "starting_cash": t.starting_cash,
            "seed": t.seed,
            "bars_file": t.bars_file,
            "requested_start": t.requested_start,
            "requested_end": t.requested_end,
            "data_end": t.data_end,
            "holdout_start": t.holdout_start,
            "usable": reason.is_none(),
            "not_usable_reason": reason,
        })
    })
}

/// The holdout run the holdout gate judges (the version's oldest holdout run): its annualised
/// Sharpe over the holdout sessions (the figure the gate compares), its costs, symbols, limits,
/// starting cash and seed, its holdout baseline (`baseline`, null when there is none), and how many
/// holdout runs the version has in all. Null without a holdout run of the version.
fn holdout_run(inputs: &ReportInputs, baseline: Option<&TrialSeries>) -> Value {
    let ppy = inputs.gates.periods_per_year;
    judged_holdout_run(inputs).map_or(Value::Null, |run| {
        let sharpe = holdout_tail(run)
            .ok()
            .and_then(moments::sharpe)
            .map(|s| moments::annualise(s, ppy));
        json!({
            "trial_id": run.trial_id,
            "holdout_sessions": run.holdout_sessions,
            "final_drawdown_mode": run.final_drawdown_mode,
            "sharpe": finite(sharpe),
            "costs": run.costs,
            "symbols": run.symbols,
            "limits_hash": run.limits_hash,
            "starting_cash": run.starting_cash,
            "seed": run.seed,
            "bars_file": run.bars_file,
            "baseline_trial_id": baseline.map(|t| t.trial_id),
            "holdout_runs_for_version": holdout_runs_for_version(inputs),
        })
    })
}
