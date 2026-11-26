//! Acceptance tests for the backtest job (phase 3, CLI1): against a real Postgres
//! (`TEST_DATABASE_URL`), with bars written to and read back from Parquet.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{Datelike, NaiveDate};
use nisse_bars::{BarReader, write_bars};
use nisse_core::{
    bars_snapshot_id,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Symbol, UnixNanos, Venue,
    },
};
use nisse_engine::CostModel;
use nisse_eval::{
    EvalError,
    backtest_job::{BacktestJob, run_backtest_job},
};
use nisse_store::{
    Store,
    tables::{trial_registry, trial_result},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z: the `now` every job is given.
const NOW: u64 = 1_767_600_000_123_456_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Weekday closes from 2 January 2023 to 30 June 2023.
const SESSIONS: usize = 130;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp directory, unique to one test and removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nisse-backtest-job-{}-{n}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- synthetic bars -------------------------------------------------------------------------

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// `(year, month, day) hour:minute:second.nanos` UTC as nanoseconds since the epoch.
fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, ns: u32) -> UnixNanos {
    let at = NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_nano_opt(h, min, s, ns)
        .unwrap();
    UnixNanos::from(u64::try_from(at.and_utc().timestamp_nanos_opt().unwrap()).unwrap())
}

/// `SESSIONS` weekday closes at 21:00 UTC, starting on Monday 2 January 2023.
fn closes() -> Vec<UnixNanos> {
    let mut day = NaiveDate::from_ymd_opt(2023, 1, 2).unwrap();
    let mut out = Vec::with_capacity(SESSIONS);
    while out.len() < SESSIONS {
        if day.weekday().num_days_from_monday() < 5 {
            let secs = day.and_hms_opt(21, 0, 0).unwrap().and_utc().timestamp();
            out.push(UnixNanos::from(
                u64::try_from(secs).unwrap() * NANOS_PER_SECOND,
            ));
        }
        day = day.succ_opt().unwrap();
    }
    out
}

/// A price at exactly 4 decimal places (a bar's prices must share one precision).
fn px(d: Decimal) -> Price {
    let mut at_4dp = d.round_dp(4);
    at_4dp.rescale(4);
    Price::from(at_4dp.to_string().as_str())
}

/// One bar per close. Open is the previous close times 1.002 (the first open is `start_px`),
/// close is the previous close times `1 + daily_change` plus or minus 0.1% on alternate days.
fn series(
    symbol: &str,
    closes: &[UnixNanos],
    start_px: Decimal,
    daily_change: Decimal,
) -> Vec<Bar> {
    let wobble = dec("0.001");
    let open_gap = dec("1.002");
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let mut previous = start_px;
    closes
        .iter()
        .enumerate()
        .map(|(i, &ts)| {
            let open = if i == 0 {
                start_px
            } else {
                (previous * open_gap).round_dp(4)
            };
            let sign = if i % 2 == 0 {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            let close = (previous * (Decimal::ONE + daily_change + sign * wobble)).round_dp(4);
            previous = close;
            Bar::new(
                bar_type,
                px(open),
                px(open.max(close)),
                px(open.min(close)),
                px(close),
                Quantity::from("1000"),
                ts,
                ts,
            )
        })
        .collect()
}

/// FALL (down about 0.5% a day) and RISE (up about 0.5% a day), every session, sorted by time
/// then symbol as the engine and the reader expect.
fn market() -> Vec<Bar> {
    let closes = closes();
    let mut bars: Vec<Bar> = series("FALL", &closes, dec("100"), dec("-0.005"))
        .into_iter()
        .chain(series("RISE", &closes, dec("100"), dec("0.005")))
        .collect();
    bars.sort_by(|a, b| {
        let (sa, sb) = (symbol_of(a), symbol_of(b));
        (a.ts_event.as_u64(), sa.as_str()).cmp(&(b.ts_event.as_u64(), sb.as_str()))
    });
    bars
}

fn symbol_of(bar: &Bar) -> Symbol {
    bar.bar_type.instrument_id().symbol
}

/// A fresh directory holding `bars` in one Parquet file.
fn bars_dir(name: &str, bars: &[Bar]) -> TempDir {
    let dir = TempDir::new(name);
    write_bars(&dir.path().join("bars.parquet"), bars).unwrap();
    dir
}

// ---- jobs and helpers -----------------------------------------------------------------------

fn strategy_params() -> Value {
    json!({
        "strategy": "trend_vol_target",
        "lookback_days": 20,
        "skip_days": 2,
        "vol_window": 10,
        "target_vol": "0.09",
        "max_weight": "0.5",
        "min_trade_notional": "25",
        "sessions_per_year": 252,
    })
}

/// A job over FALL and RISE for the whole synthetic range, reading from `dir`.
fn job(dir: &Path, strategy: Value) -> BacktestJob {
    BacktestJob {
        strategy_params: strategy,
        symbols: vec!["FALL".to_owned(), "RISE".to_owned()],
        start: utc(2023, 1, 1, 0, 0, 0, 0),
        end: utc(2023, 6, 30, 23, 59, 59, 999_999_999),
        starting_cash: dec("100000"),
        costs: CostModel::new(dec("2"), dec("0.0001")).unwrap(),
        bars_dir: dir.to_path_buf(),
        limits_path: PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/limits/limits.json"
        )),
        seed: 7,
    }
}

fn now() -> UnixNanos {
    UnixNanos::from(NOW)
}

async fn trial_count(client: &Client) -> i64 {
    trial_registry::lifetime_count(client).await.unwrap()
}

async fn version_count(client: &Client) -> i64 {
    client
        .query_one("SELECT COUNT(*) FROM strategy_version", &[])
        .await
        .unwrap()
        .get(0)
}

async fn newest_trial(client: &Client) -> i64 {
    client
        .query_one(
            "SELECT id FROM trial_registry ORDER BY id DESC LIMIT 1",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

/// The text of the error, for matching on the field it names.
fn error_text<T>(result: Result<T, EvalError>) -> String {
    result.err().expect("an error").to_string()
}

const RESULT_FIELDS: [&str; 17] = [
    "bars_file",
    "requested_start",
    "requested_end",
    "costs",
    "summary_includes_warm_up",
    "summary",
    "git_sha",
    "dirty_build",
    "data_snapshot_id",
    "symbols",
    "sessions",
    "decisions",
    "fills",
    "fees_paid",
    "starting_cash",
    "final_equity",
    "seed",
];

// ---- run acceptance -------------------------------------------------------------------------

#[tokio::test]
async fn a_backtest_registers_runs_and_stores_its_result() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("one", &market());
    let job = job(dir.path(), strategy_params());
    let before = trial_count(&client).await;

    let outcome = run_backtest_job(&store, &job, true, false, now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, before + 1);
    assert_eq!(
        trial_result::get(&client, outcome.trial_id).await.unwrap(),
        Some(outcome.result.clone())
    );

    let result = outcome.result.as_object().unwrap();
    let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = RESULT_FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(keys, expected);

    let summary = result["summary"].as_object().unwrap();
    assert_eq!(summary["sessions"], json!(SESSIONS));
    for field in ["total_return", "cagr", "ann_vol", "sharpe", "max_drawdown"] {
        assert!(summary[field].is_f64(), "{field} is not a number");
    }
    assert_eq!(result["git_sha"].as_str().unwrap().len(), 40);
    assert_eq!(result["dirty_build"], json!(false));
    assert_eq!(result["symbols"], json!(["FALL", "RISE"]));
    assert_eq!(result["sessions"], json!(SESSIONS));
    assert!(result["decisions"].is_u64());
    assert!(result["fills"].as_u64().unwrap() > 0);
    assert!(Decimal::from_str(result["fees_paid"].as_str().unwrap()).is_ok());
    assert_eq!(result["starting_cash"], json!("100000"));
    assert!(Decimal::from_str(result["final_equity"].as_str().unwrap()).is_ok());
    assert_eq!(result["seed"], json!(7));
    assert_eq!(result["requested_start"], json!(job.start.as_u64()));
    assert_eq!(result["requested_end"], json!(job.end.as_u64()));
    assert_eq!(
        result["costs"],
        json!({
            "slippage_bps": job.costs.slippage_bps.to_string(),
            "fee_per_share": job.costs.fee_per_share.to_string(),
        })
    );
    assert_eq!(result["summary_includes_warm_up"], json!(true));
    assert!(result["bars_file"].as_str().unwrap().ends_with(".parquet"));

    // The snapshot id names the bars as they read back, not as they were written.
    let read_back = BarReader::open(dir.path())
        .unwrap()
        .read(&["FALL", "RISE"], job.start, job.end)
        .unwrap();
    assert_eq!(
        result["data_snapshot_id"],
        json!(bars_snapshot_id(&read_back))
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn running_the_same_job_twice_gives_two_trials_and_equal_results() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("twice", &market());
    let job = job(dir.path(), strategy_params());
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let first = run_backtest_job(&store, &job, true, false, now())
        .await
        .unwrap();
    let second = run_backtest_job(&store, &job, true, false, now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, trials_before + 2);
    assert_eq!(version_count(&client).await, versions_before + 1);
    assert_eq!(first.strategy_version_id, second.strategy_version_id);
    assert_ne!(first.trial_id, second.trial_id);
    assert_eq!(first.result, second.result);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_dirty_build_is_refused_before_anything_is_written() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("dirty-refused", &market());
    let job = job(dir.path(), strategy_params());
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &job, false, false, now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::DirtyBuild));
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_dirty_build_with_allow_dirty_runs_and_says_so() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("dirty-allowed", &market());
    let job = job(dir.path(), strategy_params());
    let before = trial_count(&client).await;

    let outcome = run_backtest_job(&store, &job, false, true, now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, before + 1);
    assert_eq!(outcome.result["dirty_build"], json!(true));
    assert_eq!(
        trial_result::get(&client, outcome.trial_id).await.unwrap(),
        Some(outcome.result)
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_failed_run_leaves_its_trial_registered_without_a_result() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    // A zero price is not a usable bar: the trader refuses it inside the run, after the trial
    // is registered.
    let zero: Vec<Bar> = series("ZERO", &closes(), dec("0"), dec("0"));
    let dir = bars_dir("failing", &zero);
    let mut failing = job(dir.path(), strategy_params());
    failing.symbols = vec!["ZERO".to_owned()];
    let before = trial_count(&client).await;

    let err = run_backtest_job(&store, &failing, true, false, now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::Engine(_)), "got {err:?}");
    assert_eq!(trial_count(&client).await, before + 1);
    let newest = newest_trial(&client).await;
    assert_eq!(trial_result::get(&client, newest).await.unwrap(), None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unknown_strategy_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("unknown", &market());
    let unknown = job(dir.path(), json!({"strategy": "nope"}));
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &unknown, true, false, now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::UnknownStrategy(ref name) if name == "nope"));
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn no_bars_in_range_is_an_invalid_job_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("empty-range", &market());
    let mut outside = job(dir.path(), strategy_params());
    outside.start = utc(2015, 1, 1, 0, 0, 0, 0);
    outside.end = utc(2015, 12, 31, 23, 59, 59, 999_999_999);
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &outside, true, false, now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m == "no bars in range"),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_requested_symbol_without_bars_is_an_invalid_job_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("missing-symbol", &market());
    let mut with_typo = job(dir.path(), strategy_params());
    with_typo.symbols.push("TYPO".to_owned());
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &with_typo, true, false, now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m == "no bars in range for TYPO"),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_symbol_whose_bars_stop_early_is_an_invalid_job_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    // RISE loses its last ten sessions.
    let mut bars = market();
    let cut = bars[bars.len() - 20].ts_event;
    bars.retain(|bar| symbol_of(bar).as_str() != "RISE" || bar.ts_event < cut);
    let dir = bars_dir("stops-early", &bars);
    let job = job(dir.path(), strategy_params());
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &job, true, false, now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m.contains("RISE") && m.contains("stop")),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

// ---- config parsing -------------------------------------------------------------------------

/// The documented example, with `seed` present.
fn example() -> Value {
    json!({
        "strategy": strategy_params(),
        "symbols": ["SPY", "QQQ"],
        "start": "2015-01-01",
        "end": "2024-12-31",
        "starting_cash": "100000",
        "costs": {"slippage_bps": "2", "fee_per_share": "0.0001"},
        "bars_dir": "data/bars",
        "limits_path": "config/limits/limits.json",
        "seed": 0
    })
}

fn with(mut value: Value, field: &str, replacement: Value) -> Value {
    value
        .as_object_mut()
        .unwrap()
        .insert(field.to_owned(), replacement);
    value
}

fn without(mut value: Value, field: &str) -> Value {
    value.as_object_mut().unwrap().remove(field);
    value
}

#[test]
fn the_documented_example_parses_to_the_expected_job() {
    let job = BacktestJob::from_json(&example()).unwrap();

    assert_eq!(job.strategy_params, strategy_params());
    assert_eq!(job.symbols, vec!["SPY".to_owned(), "QQQ".to_owned()]);
    // 2015-01-01 00:00:00 UTC is 1_420_070_400 seconds after the epoch.
    assert_eq!(job.start.as_u64(), 1_420_070_400 * NANOS_PER_SECOND);
    // 2024-12-31 23:59:59.999999999 UTC.
    assert_eq!(job.end.as_u64(), 1_735_689_599_999_999_999);
    assert_eq!(job.starting_cash, dec("100000"));
    assert_eq!(job.costs, CostModel::new(dec("2"), dec("0.0001")).unwrap());
    assert_eq!(job.bars_dir, PathBuf::from("data/bars"));
    assert_eq!(job.limits_path, PathBuf::from("config/limits/limits.json"));
    assert_eq!(job.seed, 0);
}

#[test]
fn a_nonzero_seed_is_kept() {
    let job = BacktestJob::from_json(&with(example(), "seed", json!(42))).unwrap();
    assert_eq!(job.seed, 42);
}

#[test]
fn seed_defaults_to_zero() {
    let job = BacktestJob::from_json(&without(example(), "seed")).unwrap();
    assert_eq!(job.seed, 0);
}

#[test]
fn a_float_for_a_decimal_field_is_an_error() {
    let cash = with(example(), "starting_cash", json!(100000.0));
    let text = error_text(BacktestJob::from_json(&cash));
    assert!(text.contains("starting_cash"), "{text}");

    let slippage = with(
        example(),
        "costs",
        json!({"slippage_bps": 2, "fee_per_share": "0.0001"}),
    );
    let text = error_text(BacktestJob::from_json(&slippage));
    assert!(text.contains("slippage_bps"), "{text}");
}

#[test]
fn a_missing_field_is_an_error_naming_it() {
    for field in [
        "strategy",
        "symbols",
        "start",
        "end",
        "starting_cash",
        "costs",
        "bars_dir",
        "limits_path",
    ] {
        let text = error_text(BacktestJob::from_json(&without(example(), field)));
        assert!(text.contains(field), "{field}: {text}");
    }
}

#[test]
fn an_unknown_top_level_field_is_an_error_naming_it() {
    let extra = with(example(), "extra", json!(1));
    let text = error_text(BacktestJob::from_json(&extra));
    assert!(text.contains("extra"), "{text}");
}

#[test]
fn a_start_after_the_end_is_an_error() {
    let swapped = with(
        with(example(), "start", json!("2024-12-31")),
        "end",
        json!("2015-01-01"),
    );
    let text = error_text(BacktestJob::from_json(&swapped));
    assert!(text.contains("end"), "{text}");
}

#[test]
fn a_negative_cost_is_an_error() {
    let negative = with(
        example(),
        "costs",
        json!({"slippage_bps": "-1", "fee_per_share": "0"}),
    );
    let text = error_text(BacktestJob::from_json(&negative));
    assert!(text.contains("costs"), "{text}");
}

#[test]
fn a_strategy_without_a_name_is_an_error() {
    let unnamed = with(example(), "strategy", json!({"lookback_days": 20}));
    let text = error_text(BacktestJob::from_json(&unnamed));
    assert!(text.contains("strategy"), "{text}");
}

#[test]
fn a_bad_date_is_an_error_naming_the_field() {
    let bad = with(example(), "end", json!("31/12/2024"));
    let text = error_text(BacktestJob::from_json(&bad));
    assert!(text.contains("end"), "{text}");
}

#[test]
fn empty_symbols_are_an_error() {
    let none = with(example(), "symbols", json!([]));
    let text = error_text(BacktestJob::from_json(&none));
    assert!(text.contains("symbols"), "{text}");
}

#[test]
fn a_bad_seed_is_an_error() {
    let negative = with(example(), "seed", json!(-1));
    let text = error_text(BacktestJob::from_json(&negative));
    assert!(text.contains("seed"), "{text}");
}

#[test]
fn the_end_bound_includes_the_last_nanosecond_of_its_day_only() {
    let job = BacktestJob::from_json(&example()).unwrap();
    // The 2024-12-31 close is inside the range; the first instant of 2025 is not.
    assert!(utc(2024, 12, 31, 21, 0, 0, 0).as_u64() <= job.end.as_u64());
    assert!(utc(2025, 1, 1, 0, 0, 0, 0).as_u64() > job.end.as_u64());
}
