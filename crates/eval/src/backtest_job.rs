//! The backtest job: registers a trial, runs a strategy over stored bars and stores the result.
//!
//! This is glue. The strategy, the cost model and the statistics live in `nisse-engine`; the
//! bars come from `nisse-bars`; the rows go through the store. The order of the steps is
//! deliberate: a trial is registered before the run, so a run that fails or crashes still
//! counts towards the lifetime trial count.

use std::path::PathBuf;
use std::str::FromStr;

use chrono::NaiveDate;
use nisse_bars::BarReader;
use nisse_core::{
    MarketCalendar, StrategyVersion, StrategyVersionId, Trial, TrialKind, bars_snapshot_id,
    config_hash, nautilus::UnixNanos,
};
use nisse_engine::{
    BacktestConfig, CostModel, EngineError, GIT_SHA, metrics, run_backtest, strategies,
};
use nisse_store::{
    Store,
    tables::{trial_registry, trial_result},
};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};

use crate::EvalError;

/// Top-level fields a job file may hold. Anything else is an error.
const JOB_FIELDS: [&str; 8] = [
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
    pub seed: u64,
}

impl BacktestJob {
    /// Parses the config file's JSON:
    /// {"strategy": {..params incl. "strategy"..}, "symbols": ["SPY", ..], "start": "2015-01-01", "end": "2024-12-31",
    ///  "starting_cash": "100000", "costs": {"slippage_bps": "2", "fee_per_share": "0.0001"}, "bars_dir": "data/bars", "seed": 0}
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
            seed,
        })
    }
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
/// 2. Build the strategy from its parameters.
/// 3. Read the bars in range. No bars is an error; nothing has been written yet.
/// 4. Ensure the strategy version.
/// 5. Register the trial, before the run.
/// 6. Run the backtest. On failure the trial stays registered with no result.
/// 7. Store the result.
pub async fn run_backtest_job(
    store: &Store,
    job: &BacktestJob,
    replayable: bool,
    allow_dirty: bool,
    now: UnixNanos,
) -> Result<JobOutcome, EvalError> {
    if !replayable && !allow_dirty {
        return Err(EvalError::DirtyBuild);
    }
    let built = strategies::build(&job.strategy_params).map_err(strategy_error)?;

    let symbols: Vec<&str> = job.symbols.iter().map(String::as_str).collect();
    let bars = BarReader::open(&job.bars_dir)?.read(&symbols, job.start, job.end)?;
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Err(EvalError::InvalidJob("no bars in range".to_owned()));
    };
    let data_start = first.ts_event;
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
    let strategy_version_id = store.ensure_strategy_version(&version).await?;

    let trial = Trial {
        strategy_version_id,
        kind: TrialKind::Backtest,
        data_start,
        data_end,
        result: None,
        ts: now,
    };
    let trial_id = trial_registry::insert(store.client(), &trial).await?;

    let config = BacktestConfig {
        strategy_version_id,
        window: built.window,
        starting_cash: job.starting_cash,
        costs: job.costs,
        data_snapshot_id: data_snapshot_id.clone(),
        seed: job.seed,
    };
    let run = run_backtest(built.strategy, &bars, &calendar, config).await?;

    let final_equity = run
        .equity_curve
        .last()
        .map_or(job.starting_cash, |point| point.1);
    let summary = metrics::summarize(&run.equity_curve, built.sessions_per_year)
        .map_or(Value::Null, |summary| summary.to_json());
    let result = json!({
        "summary": summary,
        "git_sha": GIT_SHA,
        "dirty_build": !replayable,
        "data_snapshot_id": data_snapshot_id,
        "symbols": job.symbols,
        "sessions": run.equity_curve.len(),
        "decisions": run.decisions.len(),
        "fills": run.fills.len(),
        "fees_paid": run.fees_paid.to_string(),
        "starting_cash": job.starting_cash.to_string(),
        "final_equity": final_equity.to_string(),
        "seed": job.seed,
    });
    trial_result::insert(store.client(), trial_id, &result, now).await?;

    Ok(JobOutcome {
        trial_id,
        strategy_version_id,
        result,
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
