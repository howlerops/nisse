//! The backtest job: registers a trial, runs a strategy over stored bars and stores the result.
//!
//! This is glue. The strategy, the cost model and the statistics live in `nisse-engine`; the
//! bars come from `nisse-bars`; the rows go through the store. The order of the steps is
//! deliberate: a trial is registered before the run, so a run that fails or crashes still
//! counts towards the lifetime trial count.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use nisse_bars::BarReader;
use nisse_core::{
    MarketCalendar, StrategyVersion, StrategyVersionId, Trial, TrialKind, bars_snapshot_id,
    config_hash,
    nautilus::{Symbol, UnixNanos},
};
use nisse_engine::{
    BacktestConfig, CostModel, EngineError, GIT_SHA, metrics, run_backtest, strategies,
};
use nisse_risk::Limits;
use nisse_store::{
    Store,
    tables::{scoring, trial_registry, trial_result},
};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Map, Number, Value, json};

use crate::{EvalError, gates::Gates};

/// Top-level fields a job file may hold. Anything else is an error.
const JOB_FIELDS: [&str; 10] = [
    "limits_path",
    "gates_path",
    "strategy",
    "symbols",
    "start",
    "end",
    "starting_cash",
    "costs",
    "bars_dir",
    "seed",
];

/// What `nisse-backtest` reads from its config file.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestJob {
    /// Stored as `strategy_version.params`; must contain `"strategy"`.
    pub strategy_params: Value,
    pub symbols: Vec<String>,
    /// Inclusive bounds on a bar's `ts_event`.
    pub start: UnixNanos,
    pub end: UnixNanos,
    pub starting_cash: Decimal,
    pub costs: CostModel,
    pub bars_dir: PathBuf,
    /// The risk limits file the run is gated by.
    pub limits_path: PathBuf,
    /// The promotion gates file: it holds the holdout boundary and the look limit.
    pub gates_path: PathBuf,
    pub seed: u64,
}

impl BacktestJob {
    /// Parses the config file's JSON:
    /// {"strategy": {..params incl. "strategy"..}, "symbols": ["SPY", ..], "start": "2015-01-01", "end": "2024-12-31",
    ///  "starting_cash": "100000", "costs": {"slippage_bps": "2", "fee_per_share": "0.0001"}, "bars_dir": "data/bars", "limits_path": "config/limits/limits.json", "gates_path": "config/eval/gates.json", "seed": 0}
    /// Decimals are JSON strings (never floats; a JSON number for a decimal field is an error). `start` is 00:00:00 UTC of its date and `end` is 23:59:59.999999999 UTC of its date. `seed` is optional (default 0). Unknown top-level fields are an error. Errors are `EvalError::InvalidJob(String)` naming the field.
    pub fn from_json(value: &Value) -> Result<Self, EvalError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("job", "must be a JSON object"))?;
        if let Some(unknown) = object
            .keys()
            .find(|key| !JOB_FIELDS.contains(&key.as_str()))
        {
            return Err(invalid(unknown, "is an unknown field"));
        }

        let strategy = field(object, "strategy")?;
        if strategy.get("strategy").and_then(Value::as_str).is_none() {
            return Err(invalid(
                "strategy",
                "must be an object with a string \"strategy\" field",
            ));
        }

        let symbols = string_list(field(object, "symbols")?, "symbols")?;
        let start = day_bound(field(object, "start")?, "start", false)?;
        let end = day_bound(field(object, "end")?, "end", true)?;
        if start.as_u64() > end.as_u64() {
            return Err(invalid("end", "is before start"));
        }

        let starting_cash = decimal_field(object, "starting_cash")?;
        let costs = cost_model(field(object, "costs")?)?;
        let bars_dir = PathBuf::from(
            field(object, "bars_dir")?
                .as_str()
                .ok_or_else(|| invalid("bars_dir", "must be a string"))?,
        );
        let limits_path = PathBuf::from(
            field(object, "limits_path")?
                .as_str()
                .ok_or_else(|| invalid("limits_path", "must be a string"))?,
        );
        let gates_path = PathBuf::from(
            field(object, "gates_path")?
                .as_str()
                .ok_or_else(|| invalid("gates_path", "must be a string"))?,
        );
        let seed = match object.get("seed") {
            None => 0,
            Some(seed) => seed
                .as_u64()
                .ok_or_else(|| invalid("seed", "must be a non-negative integer"))?,
        };

        Ok(Self {
            strategy_params: strategy.clone(),
            symbols,
            start,
            end,
            starting_cash,
            costs,
            bars_dir,
            limits_path,
            gates_path,
            seed,
        })
    }
}

/// How a run is classified. `holdout` marks it as a counted look at the holdout period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    /// The build can be replayed (no uncommitted changes).
    pub replayable: bool,
    /// Run even if the build is not replayable; the result says so.
    pub allow_dirty: bool,
    /// A deliberate, counted look at the holdout period. Ordinary runs must stay before it.
    pub holdout: bool,
}

/// What a finished job reports.
#[derive(Debug, Clone, PartialEq)]
pub struct JobOutcome {
    pub trial_id: i64,
    pub strategy_version_id: StrategyVersionId,
    pub result: Value,
}

/// Runs `job` and stores its result. Steps, in order:
///
/// 1. Refuse a build that cannot be replayed unless `allow_dirty` (nothing is read or written).
/// 2. Build the strategy from its parameters; load the risk limits and the gates.
/// 3. Read the bars in range. No bars at all, or none for one of the requested symbols, is an
///    error; nothing has been written yet.
/// 4. The holdout rule. An ordinary run whose last bar is on or after the holdout boundary is
///    `HoldoutTouched`. A holdout run must start before the boundary (its warm-up is ordinary
///    history) and must reach it, else `InvalidJob`. Nothing is written in these.
/// 5. A holdout run takes `HOLDOUT_LOCK` (or is refused as busy), then checks its looks against
///    `holdout_max_looks` (`HoldoutRetired`, nothing written). Ordinary runs take no lock.
/// 6. Ensure the strategy version and register the trial, before the run. A holdout run is
///    marked in its registered result, so the look counts even if the run then fails. The
///    holdout lock is released after registration, and on every refusal between taking it and
///    registering.
/// 7. Run the backtest. On failure the trial stays registered with no result.
/// 8. Store the result.
pub async fn run_backtest_job(
    store: &Store,
    job: &BacktestJob,
    options: RunOptions,
    now: UnixNanos,
) -> Result<JobOutcome, EvalError> {
    if !options.replayable && !options.allow_dirty {
        return Err(EvalError::DirtyBuild);
    }
    let built = strategies::build(&job.strategy_params).map_err(strategy_error)?;
    let limits = Limits::load(&job.limits_path)?;
    let gates = Gates::load(&job.gates_path)?;
    // Taken before `limits` moves into the run's config.
    let limits_hash = limits.hash().to_string();
    let limits_json = limits.to_json();

    let symbols: Vec<&str> = job.symbols.iter().map(String::as_str).collect();
    let reader = BarReader::open(&job.bars_dir)?;
    let bars_file = reader.snapshot_file_name()?;
    let bars = reader.read(&symbols, job.start, job.end)?;
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Err(EvalError::InvalidJob("no bars in range".to_owned()));
    };
    // Every requested symbol must have bars. The strategy sizes each position from the number
    // of assets, so a symbol missing from the files (a typo, a gap in the download) would
    // silently change every weight, and the stored result could never be corrected.
    let present: BTreeSet<Symbol> = bars
        .iter()
        .map(|bar| bar.bar_type.instrument_id().symbol)
        .collect();
    if let Some(missing) = symbols
        .iter()
        .find(|symbol| !present.iter().any(|have| have.as_str() == **symbol))
    {
        return Err(EvalError::InvalidJob(format!(
            "no bars in range for {missing}"
        )));
    }
    // A symbol may start late (it is skipped until it has history), but its bars must reach
    // the last session: one that stops early would sit in the universe unpriced.
    let at_last_session: BTreeSet<Symbol> = bars
        .iter()
        .rev()
        .take_while(|bar| bar.ts_event == last.ts_event)
        .map(|bar| bar.bar_type.instrument_id().symbol)
        .collect();
    if let Some(short) = present.iter().find(|have| !at_last_session.contains(*have)) {
        return Err(EvalError::InvalidJob(format!(
            "the bars for {short} stop before the last session in range"
        )));
    }
    let boundary = gates.holdout_boundary().ok_or_else(|| {
        EvalError::InvalidGates("holdout_start: is outside the representable range".to_owned())
    })?;
    let first_session = first.ts_event;
    let last_session = last.ts_event;
    if options.holdout {
        // The warm-up is ordinary history, so the run starts before the boundary. With the first
        // session before it, `holdout_sessions <= sessions - 1 = returns.len()`, so the gate
        // report can always take the holdout tail of the returns.
        if first_session.as_u64() >= boundary.as_u64() {
            return Err(EvalError::InvalidJob(
                "a holdout run must start before the holdout period: its warm-up is ordinary history"
                    .to_owned(),
            ));
        }
        if last_session.as_u64() < boundary.as_u64() {
            return Err(EvalError::InvalidJob(
                "a holdout run must reach into the holdout period".to_owned(),
            ));
        }
    } else if last_session.as_u64() >= boundary.as_u64() {
        return Err(EvalError::HoldoutTouched(format!(
            "the holdout starts {}, but the bars run to {}",
            gates.holdout_start_text(),
            date_text(last_session)
        )));
    }

    let data_start = first_session;
    let data_end = last
        .ts_event
        .as_u64()
        .checked_add(1)
        .map(UnixNanos::from)
        .ok_or_else(|| {
            EvalError::InvalidJob("the last bar's timestamp is out of range".to_owned())
        })?;
    let data_snapshot_id = bars_snapshot_id(&bars);
    let closes: Vec<UnixNanos> = bars
        .chunk_by(|a, b| a.ts_event == b.ts_event)
        .filter_map(|session| session.first())
        .map(|bar| bar.ts_event)
        .collect();
    let calendar = MarketCalendar::from_closes(&closes)?;

    let version = StrategyVersion {
        git_sha: GIT_SHA.to_owned(),
        config_hash: config_hash(&job.strategy_params),
        params: job.strategy_params.clone(),
        parent_id: None,
        ts: now,
    };
    let (strategy_version_id, trial_id) = if options.holdout {
        // Held from the look-count check until the trial is registered, so two holdout runs
        // cannot both pass the check. Released on every path once taken.
        if !scoring::try_advisory_lock(store.client(), scoring::HOLDOUT_LOCK).await? {
            return Err(EvalError::InvalidJob(
                "another holdout run is in progress; try again when it has registered its trial"
                    .to_owned(),
            ));
        }
        let registered = register_trial(
            store,
            &version,
            (data_start, data_end),
            options,
            gates.holdout_max_looks,
            now,
        )
        .await;
        let released = scoring::advisory_unlock(store.client(), scoring::HOLDOUT_LOCK).await;
        let ids = registered?;
        released?;
        ids
    } else {
        register_trial(
            store,
            &version,
            (data_start, data_end),
            options,
            gates.holdout_max_looks,
            now,
        )
        .await?
    };

    let config = BacktestConfig {
        strategy_version_id,
        window: built.window,
        starting_cash: job.starting_cash,
        costs: job.costs,
        data_snapshot_id: data_snapshot_id.clone(),
        seed: job.seed,
        limits,
    };
    let run = run_backtest(built.strategy, &bars, &calendar, config).await?;

    let final_equity = run
        .equity_curve
        .last()
        .map_or(job.starting_cash, |point| point.1);
    let summary = metrics::summarize(&run.equity_curve, built.sessions_per_year)
        .map_or(Value::Null, |summary| summary.to_json());
    let (returns, returns_complete) = returns_json(&run.equity_curve);
    // Zero for an ordinary run, which is refused when its last bar reaches the boundary. For a
    // holdout run the first session is before the boundary, so this is at most `returns.len()`.
    let holdout_sessions = run
        .equity_curve
        .iter()
        .filter(|point| point.0.as_u64() >= boundary.as_u64())
        .count();
    let result = json!({
        "summary": summary,
        "returns": returns,
        "returns_complete": returns_complete,
        "holdout": options.holdout,
        "holdout_start": gates.holdout_start_text(),
        "holdout_sessions": holdout_sessions,
        "git_sha": GIT_SHA,
        "dirty_build": !options.replayable,
        "data_snapshot_id": data_snapshot_id,
        "bars_file": bars_file,
        "symbols": job.symbols,
        "requested_start": job.start.as_u64(),
        "requested_end": job.end.as_u64(),
        "data_end": last_session.as_u64(),
        "costs": {
            "slippage_bps": job.costs.slippage_bps.to_string(),
            "fee_per_share": job.costs.fee_per_share.to_string(),
        },
        // The summary covers every session, including the flat warm-up before the first
        // trade, so its volatility and return understate the invested period.
        "summary_includes_warm_up": true,
        // What the risk gate did: a run that ends `flat` stopped trading after a drawdown
        // and never re-entered, which the summary alone would not show.
        "final_drawdown_mode": run.final_mode.as_str(),
        "orders_blocked": run.orders_blocked,
        "risk_events": run.risk_events.len(),
        "sessions": run.equity_curve.len(),
        "decisions": run.decisions.len(),
        "fills": run.fills.len(),
        "fees_paid": run.fees_paid.to_string(),
        "starting_cash": job.starting_cash.to_string(),
        "final_equity": final_equity.to_string(),
        "seed": job.seed,
        "limits_hash": limits_hash,
        "limits": limits_json,
    });
    trial_result::insert(store.client(), trial_id, &result, now).await?;

    Ok(JobOutcome {
        trial_id,
        strategy_version_id,
        result,
    })
}

/// The look-count check (a holdout run only), then the strategy version and the trial. A holdout
/// run is refused here, before anything is written, once `max_looks` looks exist. Returns the
/// strategy version id and the trial id. The caller holds `HOLDOUT_LOCK` across this call.
async fn register_trial(
    store: &Store,
    version: &StrategyVersion,
    span: (UnixNanos, UnixNanos),
    options: RunOptions,
    max_looks: u32,
    now: UnixNanos,
) -> Result<(StrategyVersionId, i64), EvalError> {
    if options.holdout {
        let looks = trial_registry::holdout_count(store.client()).await?;
        if looks >= i64::from(max_looks) {
            return Err(EvalError::HoldoutRetired {
                looks,
                max: max_looks,
            });
        }
    }
    let strategy_version_id = store.ensure_strategy_version(version).await?;
    let trial = Trial {
        strategy_version_id,
        kind: TrialKind::Backtest,
        data_start: span.0,
        data_end: span.1,
        result: options.holdout.then(|| json!({"holdout": true})),
        ts: now,
    };
    let trial_id = trial_registry::insert(store.client(), &trial).await?;
    Ok((strategy_version_id, trial_id))
}

/// The daily returns `equity_t / equity_{t-1} - 1`, one per session after the first, as JSON
/// numbers rounded to 10 decimal places. The ratio is computed in `Decimal`; only the final
/// conversion is `f64`. A return with a zero or negative equity on either side, or one that is
/// not finite, is `null`, and the flag is `false`.
fn returns_json(curve: &[(UnixNanos, Decimal)]) -> (Value, bool) {
    const SCALE: f64 = 1e10;
    let mut complete = true;
    let mut returns = Vec::with_capacity(curve.len().saturating_sub(1));
    for pair in curve.windows(2) {
        let (before, after) = (pair[0].1, pair[1].1);
        let ratio = if before > Decimal::ZERO && after > Decimal::ZERO {
            after
                .checked_div(before)
                .and_then(|ratio| ratio.checked_sub(Decimal::ONE))
                .and_then(|ret| ret.to_f64())
        } else {
            None
        };
        let number = ratio
            .map(|ret| (ret * SCALE).round() / SCALE)
            .and_then(Number::from_f64);
        complete &= number.is_some();
        returns.push(number.map_or(Value::Null, Value::Number));
    }
    (Value::Array(returns), complete)
}

/// The UTC date of a bar's timestamp, as `YYYY-MM-DD`.
fn date_text(ts: UnixNanos) -> String {
    i64::try_from(ts.as_u64())
        .ok()
        .map_or_else(String::new, |nanos| {
            DateTime::<Utc>::from_timestamp_nanos(nanos)
                .date_naive()
                .to_string()
        })
}

/// An unknown strategy name keeps its own error; anything else about the parameters is a bad job.
fn strategy_error(error: EngineError) -> EvalError {
    match error {
        EngineError::UnknownStrategy(name) => EvalError::UnknownStrategy(name),
        other => EvalError::InvalidJob(format!("strategy: {other}")),
    }
}

fn invalid(field: &str, reason: &str) -> EvalError {
    EvalError::InvalidJob(format!("{field}: {reason}"))
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, EvalError> {
    object.get(name).ok_or_else(|| invalid(name, "is missing"))
}

/// A non-empty JSON array of non-empty strings.
fn string_list(value: &Value, name: &str) -> Result<Vec<String>, EvalError> {
    let items = value
        .as_array()
        .ok_or_else(|| invalid(name, "must be an array of strings"))?;
    if items.is_empty() {
        return Err(invalid(name, "must name at least one symbol"));
    }
    items
        .iter()
        .map(|item| match item.as_str() {
            Some(text) if !text.is_empty() => Ok(text.to_owned()),
            _ => Err(invalid(name, "must contain only non-empty strings")),
        })
        .collect()
}

/// A decimal given as a JSON string. A JSON number is refused: it would arrive as a float.
fn decimal_text(value: &Value, name: &str) -> Result<Decimal, EvalError> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid(name, "must be a decimal given as a JSON string"))?;
    Decimal::from_str(text).map_err(|_| invalid(name, "is not a valid decimal"))
}

fn decimal_field(object: &Map<String, Value>, name: &str) -> Result<Decimal, EvalError> {
    decimal_text(field(object, name)?, name)
}

fn cost_model(value: &Value) -> Result<CostModel, EvalError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("costs", "must be an object"))?;
    let slippage_bps = decimal_field(object, "slippage_bps")?;
    let fee_per_share = decimal_field(object, "fee_per_share")?;
    CostModel::new(slippage_bps, fee_per_share).map_err(|e| invalid("costs", &e.to_string()))
}

/// `start` (`end == false`) is 00:00:00 UTC of the date; `end` is 23:59:59.999999999 UTC.
fn day_bound(value: &Value, name: &str, end: bool) -> Result<UnixNanos, EvalError> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid(name, "must be a date as a string, YYYY-MM-DD"))?;
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| invalid(name, "must be a date as YYYY-MM-DD"))?;
    day_nanos(date, end, name)
}

/// The instant a date's day starts (`end == false`) or ends (`end == true`), UTC.
fn day_nanos(date: NaiveDate, end: bool, name: &str) -> Result<UnixNanos, EvalError> {
    let at = if end {
        date.and_hms_nano_opt(23, 59, 59, 999_999_999)
    } else {
        date.and_hms_opt(0, 0, 0)
    };
    at.and_then(|at| at.and_utc().timestamp_nanos_opt())
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(UnixNanos::from)
        .ok_or_else(|| invalid(name, "is outside the representable range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// A curve of equity values, one per session, at consecutive timestamps.
    fn curve(values: &[&str]) -> Vec<(UnixNanos, Decimal)> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| (UnixNanos::from(i as u64), dec(v)))
            .collect()
    }

    #[test]
    fn returns_are_simple_daily_ratios() {
        assert_eq!(
            returns_json(&curve(&["100", "110", "99"])),
            (json!([0.1, -0.1]), true)
        );
    }

    #[test]
    fn a_zero_equity_makes_its_return_and_the_next_null() {
        assert_eq!(
            returns_json(&curve(&["100", "0", "50", "100"])),
            (json!([null, null, 1.0]), false)
        );
    }

    #[test]
    fn a_negative_equity_makes_its_returns_null() {
        assert_eq!(
            returns_json(&curve(&["100", "-5", "10"])),
            (json!([null, null]), false)
        );
    }

    #[test]
    fn one_session_has_no_returns_and_is_complete() {
        assert_eq!(returns_json(&curve(&["100"])), (json!([]), true));
    }

    #[test]
    fn an_empty_curve_has_no_returns_and_is_complete() {
        assert_eq!(returns_json(&[]), (json!([]), true));
    }

    #[test]
    fn returns_are_rounded_to_ten_decimal_places() {
        assert_eq!(
            returns_json(&curve(&["100", "100.000000000001"])),
            (json!([0.0]), true)
        );
    }

    #[test]
    fn a_flat_account_has_exact_zero_returns() {
        assert_eq!(
            returns_json(&curve(&["100000", "100000", "100000"])),
            (json!([0.0, 0.0]), true)
        );
    }

    #[test]
    fn a_day_bound_is_the_start_or_the_end_of_its_day() {
        let date = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap();
        let start = day_nanos(date, false, "start").unwrap().as_u64();
        let end = day_nanos(date, true, "end").unwrap().as_u64();
        assert_eq!(start, 1_672_531_200 * 1_000_000_000);
        assert_eq!(end, start + 86_400 * 1_000_000_000 - 1);
    }
}
