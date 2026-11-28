//! Acceptance tests for the backtest job (phase 3, CLI1): against a real Postgres
//! (`TEST_DATABASE_URL`), with bars written to and read back from Parquet.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use nisse_bars::{BarReader, write_bars};
use nisse_core::{
    StrategyVersionId, bars_snapshot_id, config_hash,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Symbol, UnixNanos, Venue,
    },
};
use nisse_engine::CostModel;
use nisse_eval::{
    EvalError,
    backtest_job::{BacktestJob, RunOptions, TRACKING_KEPT_KEYS, run_backtest_job},
};
use nisse_risk::Limits;
use nisse_store::{
    Store,
    tables::{scoring, trial_registry, trial_result},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z: the `now` every job is given.
const NOW: u64 = 1_767_600_000_123_456_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Weekday closes from 4 January 2021 to 2 July 2021, well before the 2023 holdout.
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

/// `SESSIONS` weekday closes at 21:00 UTC, starting on Monday 4 January 2021.
fn closes() -> Vec<UnixNanos> {
    let mut day = NaiveDate::from_ymd_opt(2021, 1, 4).unwrap();
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

/// The repository's gates file with its holdout boundary and look limit replaced, written to
/// `path`.
fn write_gates(path: &Path, holdout_start: &str, max_looks: u32) {
    let repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/eval/gates.json");
    let mut gates: Value = serde_json::from_str(&std::fs::read_to_string(repo).unwrap()).unwrap();
    gates["holdout_start"] = json!(holdout_start);
    gates["holdout_max_looks"] = json!(max_looks);
    std::fs::write(path, gates.to_string()).unwrap();
}

/// A job over FALL and RISE for the whole synthetic range, reading from `dir`. Its gates file
/// keeps the default holdout boundary (2023-01-01), after the data, so an ordinary run is allowed.
/// Call `set_holdout` after the last `job` of a test, since jobs in one directory share the file.
fn job(dir: &Path, strategy: Value) -> BacktestJob {
    let gates_path = dir.join("gates.json");
    write_gates(&gates_path, "2023-01-01", 10);
    BacktestJob {
        strategy_params: strategy,
        symbols: vec!["FALL".to_owned(), "RISE".to_owned()],
        start: utc(2021, 1, 1, 0, 0, 0, 0),
        end: utc(2021, 7, 31, 23, 59, 59, 999_999_999),
        starting_cash: dec("100000"),
        costs: CostModel::new(dec("2"), dec("0.0001")).unwrap(),
        bars_dir: dir.to_path_buf(),
        limits_path: PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/limits/limits.json"
        )),
        gates_path,
        seed: 7,
    }
}

/// Moves the holdout boundary and the look limit of the job's gates file.
fn set_holdout(job: &BacktestJob, holdout_start: &str, max_looks: u32) {
    write_gates(&job.gates_path, holdout_start, max_looks);
}

fn options(replayable: bool, allow_dirty: bool, holdout: bool) -> RunOptions {
    RunOptions {
        replayable,
        allow_dirty,
        holdout,
        tracking: false,
    }
}

/// Options for a tracking run on a clean build.
fn tracking_options() -> RunOptions {
    RunOptions {
        tracking: true,
        ..options(true, false, false)
    }
}

/// Runs an ordinary backtest of `job` (which creates its strategy version) and registers a paper
/// `live` trial for that version starting at `live_from`. Returns the version. Call it before
/// `set_holdout` moves the boundary inside the bars, since an ordinary run cannot reach it.
async fn traded(
    store: &Store,
    client: &Client,
    job: &BacktestJob,
    live_from: UnixNanos,
) -> StrategyVersionId {
    let ordinary = run_backtest_job(store, job, options(true, false, false), now())
        .await
        .unwrap();
    trial_registry::insert_live(
        client,
        ordinary.strategy_version_id,
        "paper",
        live_from,
        now(),
    )
    .await
    .unwrap();
    ordinary.strategy_version_id
}

/// The UTC date of a timestamp, as `YYYY-MM-DD`.
fn date_of(ts: UnixNanos) -> String {
    let nanos = i64::try_from(ts.as_u64()).unwrap();
    DateTime::<Utc>::from_timestamp_nanos(nanos)
        .date_naive()
        .to_string()
}

/// The `result` column of a trial's registry row; `None` for SQL NULL (no marker).
async fn registry_result(client: &Client, trial_id: i64) -> Option<Value> {
    client
        .query_one(
            "SELECT result FROM trial_registry WHERE id = $1",
            &[&trial_id],
        )
        .await
        .unwrap()
        .get(0)
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

const RESULT_FIELDS: [&str; 29] = [
    "returns",
    "returns_complete",
    "holdout",
    "tracking",
    "holdout_start",
    "holdout_sessions",
    "final_drawdown_mode",
    "orders_blocked",
    "risk_events",
    "limits_hash",
    "limits",
    "bars_file",
    "requested_start",
    "requested_end",
    "data_end",
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

    let outcome = run_backtest_job(&store, &job, options(true, false, false), now())
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
    assert_eq!(result["final_drawdown_mode"], json!("normal"));
    assert_eq!(result["orders_blocked"], json!(0));
    assert!(result["risk_events"].as_u64().unwrap() >= 1);
    assert!(result["bars_file"].as_str().unwrap().ends_with(".parquet"));
    let limits = Limits::load(&job.limits_path).unwrap();
    assert_eq!(result["limits_hash"], json!(limits.hash().to_string()));
    assert_eq!(result["limits"], limits.to_json());

    // The snapshot id names the bars as they read back, not as they were written.
    let read_back = BarReader::open(dir.path())
        .unwrap()
        .read(&["FALL", "RISE"], job.start, job.end)
        .unwrap();
    assert_eq!(
        result["data_snapshot_id"],
        json!(bars_snapshot_id(&read_back))
    );

    // The last bar read back, and an ordinary run has no sessions in the holdout.
    let last_read = read_back.last().unwrap().ts_event;
    assert_eq!(result["data_end"], json!(last_read.as_u64()));
    assert_eq!(result["holdout_sessions"], json!(0));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_missing_limits_file_is_a_risk_error_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("missing-limits", &market());
    let mut missing = job(dir.path(), strategy_params());
    missing.limits_path = dir.path().join("no-such-limits.json");
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &missing, options(true, false, false), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::Risk(_)), "got {err:?}");
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn limits_looser_than_the_ceilings_are_a_risk_error_and_write_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("loose-limits", &market());
    let repo_limits = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    ));
    let mut loose: Value =
        serde_json::from_str(&std::fs::read_to_string(&repo_limits).unwrap()).unwrap();
    loose
        .as_object_mut()
        .unwrap()
        .insert("max_risk_per_trade".to_owned(), json!("0.01"));
    let loose_path = dir.path().join("loose-limits.json");
    std::fs::write(&loose_path, loose.to_string()).unwrap();
    let mut looser = job(dir.path(), strategy_params());
    looser.limits_path = loose_path;
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &looser, options(true, false, false), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::Risk(_)), "got {err:?}");
    assert!(err.to_string().contains("max_risk_per_trade"), "{err}");
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
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

    let first = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();
    let second = run_backtest_job(&store, &job, options(true, false, false), now())
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

    let err = run_backtest_job(&store, &job, options(false, false, false), now())
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

    let outcome = run_backtest_job(&store, &job, options(false, true, false), now())
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

    let err = run_backtest_job(&store, &failing, options(true, false, false), now())
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

    let err = run_backtest_job(&store, &unknown, options(true, false, false), now())
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

    let err = run_backtest_job(&store, &outside, options(true, false, false), now())
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

    let err = run_backtest_job(&store, &with_typo, options(true, false, false), now())
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

    let err = run_backtest_job(&store, &job, options(true, false, false), now())
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

// ---- returns and holdout acceptance ---------------------------------------------------------

#[tokio::test]
async fn a_backtest_stores_its_daily_returns() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("returns", &market());
    let job = job(dir.path(), strategy_params());

    let outcome = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();

    let result = &outcome.result;
    let returns = result["returns"].as_array().unwrap();
    assert_eq!(returns.len(), SESSIONS - 1);
    assert!(returns.iter().all(Value::is_number), "{returns:?}");
    assert_eq!(result["returns_complete"], json!(true));

    let compounded: f64 = returns.iter().map(|r| 1.0 + r.as_f64().unwrap()).product();
    let final_equity = dec(result["final_equity"].as_str().unwrap());
    let ratio = (final_equity / job.starting_cash)
        .to_string()
        .parse::<f64>()
        .unwrap();
    assert!(
        (compounded - ratio).abs() < 1e-6,
        "compounded {compounded} vs ratio {ratio}"
    );

    // The account is flat through the warm-up, before the first trade.
    for r in &returns[..5] {
        assert_eq!(r.as_f64(), Some(0.0));
    }

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_ordinary_run_that_reaches_the_holdout_is_refused_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("touched", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2021-05-03", 10);
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let err = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::HoldoutTouched(ref m) if m.contains("2021-05-03") && m.contains("2021-07-02")),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_holdout_run_is_counted_and_marked_and_an_ordinary_run_before_it_is_not() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-marked", &market());
    let full = job(dir.path(), strategy_params());
    let mut early = job(dir.path(), strategy_params());
    early.end = utc(2021, 4, 28, 23, 59, 59, 999_999_999);
    set_holdout(&full, "2021-05-03", 10);
    let trials_before = trial_count(&client).await;
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let outcome = run_backtest_job(&store, &full, options(true, false, true), now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, trials_before + 1);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before + 1
    );
    assert_eq!(outcome.result["holdout"], json!(true));
    assert_eq!(outcome.result["holdout_start"], json!("2021-05-03"));
    // The sessions on or after the boundary, counted from the synthetic closes.
    let boundary = utc(2021, 5, 3, 0, 0, 0, 0).as_u64();
    let holdout_sessions = closes().iter().filter(|ts| ts.as_u64() >= boundary).count();
    assert!(holdout_sessions > 0);
    assert_eq!(outcome.result["holdout_sessions"], json!(holdout_sessions));
    assert_eq!(
        trial_result::get(&client, outcome.trial_id).await.unwrap(),
        Some(outcome.result.clone())
    );

    let before_holdout = run_backtest_job(&store, &early, options(true, false, false), now())
        .await
        .unwrap();

    assert_eq!(before_holdout.result["holdout"], json!(false));
    assert_eq!(before_holdout.result["holdout_start"], json!("2021-05-03"));
    assert_eq!(before_holdout.result["holdout_sessions"], json!(0));
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before + 1
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_holdout_run_whose_data_ends_before_the_holdout_is_an_invalid_job_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-short", &market());
    // The default boundary (2023-01-01) is after the data.
    let job = job(dir.path(), strategy_params());
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let err = run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m == "a holdout run must reach into the holdout period"),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_holdout_is_retired_after_its_allowed_looks_and_the_refused_run_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("retired", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2021-05-03", 2);

    for _ in 0..2 {
        run_backtest_job(&store, &job, options(true, false, true), now())
            .await
            .unwrap();
    }
    let trials_before = trial_count(&client).await;

    let err = run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::HoldoutRetired { looks: 2, max: 2 }),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(trial_registry::holdout_count(&client).await.unwrap(), 2);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_holdout_run_that_fails_after_registration_still_counts_as_a_look() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let zero: Vec<Bar> = series("ZERO", &closes(), dec("0"), dec("0"));
    let dir = bars_dir("holdout-failing", &zero);
    let mut failing = job(dir.path(), strategy_params());
    failing.symbols = vec!["ZERO".to_owned()];
    set_holdout(&failing, "2021-05-03", 10);
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let err = run_backtest_job(&store, &failing, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::Engine(_)), "got {err:?}");
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before + 1
    );
    let newest = newest_trial(&client).await;
    assert_eq!(trial_result::get(&client, newest).await.unwrap(), None);
    db.drop_db().await.unwrap();
}

/// Whether the holdout lock can be taken from a fresh connection now. Takes and releases it.
async fn holdout_lock_is_free(db: &ScratchDb) -> bool {
    let other = db.client().await.unwrap();
    let free = scoring::try_advisory_lock(&other, scoring::HOLDOUT_LOCK)
        .await
        .unwrap();
    if free {
        assert!(
            scoring::advisory_unlock(&other, scoring::HOLDOUT_LOCK)
                .await
                .unwrap()
        );
    }
    free
}

#[tokio::test]
async fn a_holdout_run_that_starts_in_the_holdout_is_refused_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-start-late", &market());
    let mut late = job(dir.path(), strategy_params());
    // The first bar is the 2021-05-03 close: the boundary is 00:00 that day, so the warm-up
    // would be inside the holdout.
    late.start = utc(2021, 5, 3, 0, 0, 0, 0);
    set_holdout(&late, "2021-05-03", 10);
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let err = run_backtest_job(&store, &late, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m.contains("start before the holdout")),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_holdout_run_has_no_more_holdout_sessions_than_returns_to_judge() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-tail", &market());
    let job = job(dir.path(), strategy_params());
    // The boundary moves from the second session (every session but the first is held out) to
    // the last session (one session is held out). The first session is always before it, so
    // the holdout sessions are at most the returns, `SESSIONS - 1`.
    for (boundary, y, m, d) in [
        ("2021-01-05", 2021, 1, 5),
        ("2021-05-03", 2021, 5, 3),
        ("2021-07-02", 2021, 7, 2),
    ] {
        set_holdout(&job, boundary, 10);
        let outcome = run_backtest_job(&store, &job, options(true, false, true), now())
            .await
            .unwrap();

        let from = utc(y, m, d, 0, 0, 0, 0).as_u64();
        let expected = closes().iter().filter(|ts| ts.as_u64() >= from).count();
        let returns = outcome.result["returns"].as_array().unwrap().len();
        let holdout_sessions = outcome.result["holdout_sessions"].as_u64().unwrap();
        assert_eq!(
            holdout_sessions,
            u64::try_from(expected).unwrap(),
            "{boundary}"
        );
        assert!(holdout_sessions >= 1, "{boundary}: {holdout_sessions}");
        assert!(
            holdout_sessions <= u64::try_from(returns).unwrap(),
            "{boundary}: {holdout_sessions} sessions, {returns} returns"
        );
    }
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_held_holdout_lock_refuses_a_holdout_run_and_writes_nothing_until_it_is_released() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-locked", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2021-05-03", 10);
    let holder = db.client().await.unwrap();
    assert!(
        scoring::try_advisory_lock(&holder, scoring::HOLDOUT_LOCK)
            .await
            .unwrap()
    );
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;
    let holdout_before = trial_registry::holdout_count(&client).await.unwrap();

    let err = run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::InvalidJob(ref m) if m.contains("another holdout run")),
        "got {err:?}"
    );
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before
    );

    assert!(
        scoring::advisory_unlock(&holder, scoring::HOLDOUT_LOCK)
            .await
            .unwrap()
    );
    let outcome = run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap();
    assert_eq!(outcome.result["holdout"], json!(true));
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        holdout_before + 1
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_holdout_lock_is_released_after_a_successful_holdout_run() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("lock-success", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2021-05-03", 10);

    run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap();

    assert!(holdout_lock_is_free(&db).await);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_holdout_lock_is_released_after_a_refused_retired_holdout_run() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("lock-retired", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2021-05-03", 1);

    run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap();
    let err = run_backtest_job(&store, &job, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, EvalError::HoldoutRetired { looks: 1, max: 1 }),
        "got {err:?}"
    );
    assert!(holdout_lock_is_free(&db).await);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_holdout_lock_is_released_after_a_holdout_run_that_fails_after_registration() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let zero: Vec<Bar> = series("ZERO", &closes(), dec("0"), dec("0"));
    let dir = bars_dir("lock-failing", &zero);
    let mut failing = job(dir.path(), strategy_params());
    failing.symbols = vec!["ZERO".to_owned()];
    set_holdout(&failing, "2021-05-03", 10);

    let err = run_backtest_job(&store, &failing, options(true, false, true), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::Engine(_)), "got {err:?}");
    assert!(holdout_lock_is_free(&db).await);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_ordinary_run_is_not_blocked_by_a_held_holdout_lock() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("ordinary-under-lock", &market());
    let job = job(dir.path(), strategy_params());
    let holder = db.client().await.unwrap();
    assert!(
        scoring::try_advisory_lock(&holder, scoring::HOLDOUT_LOCK)
            .await
            .unwrap()
    );
    let before = trial_count(&client).await;

    let outcome = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, before + 1);
    assert_eq!(outcome.result["holdout"], json!(false));
    assert!(
        scoring::advisory_unlock(&holder, scoring::HOLDOUT_LOCK)
            .await
            .unwrap()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_missing_gates_file_is_an_invalid_gates_error_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("missing-gates", &market());
    let mut missing = job(dir.path(), strategy_params());
    missing.gates_path = dir.path().join("no-such-gates.json");
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &missing, options(true, false, false), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::InvalidGates(_)), "got {err:?}");
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_holdout_boundary_after_2023_is_an_invalid_gates_error_and_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("late-holdout", &market());
    let job = job(dir.path(), strategy_params());
    set_holdout(&job, "2023-01-02", 10);
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;

    let err = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::InvalidGates(_)), "got {err:?}");
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
        "gates_path": "config/eval/gates.json",
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
    assert_eq!(job.gates_path, PathBuf::from("config/eval/gates.json"));
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
        "gates_path",
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

/// The link between the two halves: what a real backtest stores is what the gate report
/// accepts as a usable trial. If a result field is renamed on one side only, this fails.
#[tokio::test]
async fn a_stored_backtest_result_is_a_usable_trial_for_the_gate_report() {
    use nisse_eval::{gate_report::TrialSeries, gates::Gates};

    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("usable", &market());
    let job = job(dir.path(), strategy_params());
    let gates = Gates::load(&job.gates_path).unwrap();

    // A clean build is usable.
    let clean = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();
    // A dirty build ran, but may not be judged.
    let dirty = run_backtest_job(&store, &job, options(false, true, false), now())
        .await
        .unwrap();

    let stored = scoring::backtest_results(&client).await.unwrap();
    let series = |trial_id: i64| {
        let trial = stored.iter().find(|t| t.trial_id == trial_id).unwrap();
        TrialSeries::from_stored(trial)
    };
    let clean_series = series(clean.trial_id);
    assert_eq!(clean_series.usable(&gates), Ok(()));
    assert_eq!(
        clean_series.returns.as_ref().map(Vec::len),
        Some(SESSIONS - 1)
    );
    assert!(series(dirty.trial_id).usable(&gates).is_err());

    // The same clean result judged under an earlier holdout boundary is not usable: it was
    // run under another one, and its data reaches past the earlier date.
    let earlier = dir.path().join("earlier-gates.json");
    write_gates(&earlier, "2021-05-03", 10);
    assert!(
        clean_series
            .usable(&Gates::load(&earlier).unwrap())
            .is_err()
    );
    db.drop_db().await.unwrap();
}

// ---- tracking runs --------------------------------------------------------------------------

/// A paper live start inside the synthetic bars, used by most tracking tests.
fn live_from() -> UnixNanos {
    utc(2021, 2, 1, 0, 0, 0, 0)
}

#[tokio::test]
async fn a_tracking_run_is_refused_without_a_live_trial() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-no-live", &market());
    let job = job(dir.path(), strategy_params());
    let before = trial_count(&client).await;

    let err = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap_err();

    assert!(err.to_string().contains("no live trial"));
    assert_eq!(trial_count(&client).await, before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_is_refused_for_a_config_that_was_never_traded() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-other-config", &market());
    let traded_job = job(dir.path(), strategy_params());
    traded(&store, &client, &traded_job, live_from()).await;
    let mut other_params = strategy_params();
    other_params["lookback_days"] = json!(21);
    let other = job(dir.path(), other_params);
    let before = trial_count(&client).await;

    let err = run_backtest_job(&store, &other, tracking_options(), now())
        .await
        .unwrap_err();

    assert!(err.to_string().contains("no live trial"));
    assert_eq!(trial_count(&client).await, before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_is_refused_when_the_bars_end_before_the_first_live_session() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-late-live", &market());
    let job = job(dir.path(), strategy_params());
    // The last session in the bars is 2 July 2021.
    traded(&store, &client, &job, utc(2021, 7, 5, 0, 0, 0, 0)).await;
    let before = trial_count(&client).await;

    let err = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap_err();

    assert!(err.to_string().contains("the bars end before it"));
    assert_eq!(trial_count(&client).await, before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_may_start_on_the_last_session_the_engine_ran() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-last-session", &market());
    let job = job(dir.path(), strategy_params());
    let last_close = *closes().last().unwrap();
    traded(&store, &client, &job, last_close).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    assert_eq!(outcome.result["tracking"], json!(true));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_may_reach_past_the_holdout_boundary() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-past-holdout", &market());
    let full = job(dir.path(), strategy_params());
    traded(&store, &client, &full, live_from()).await;
    set_holdout(&full, "2021-05-03", 10);

    let ordinary = run_backtest_job(&store, &full, options(true, false, false), now())
        .await
        .unwrap_err();
    assert!(matches!(ordinary, EvalError::HoldoutTouched(_)));

    let outcome = run_backtest_job(&store, &full, tracking_options(), now())
        .await
        .unwrap();
    assert_eq!(outcome.result["holdout"], json!(false));
    assert_eq!(outcome.result["tracking"], json!(true));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_is_not_a_holdout_look() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-not-look", &market());
    let full = job(dir.path(), strategy_params());
    traded(&store, &client, &full, live_from()).await;
    set_holdout(&full, "2021-05-03", 10);
    let looks_before = trial_registry::holdout_count(&client).await.unwrap();

    run_backtest_job(&store, &full, tracking_options(), now())
        .await
        .unwrap();

    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        looks_before
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_registers_a_trial_marked_tracking() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-marked", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, live_from()).await;
    let before = trial_count(&client).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    assert_eq!(trial_count(&client).await, before + 1);
    assert_eq!(
        registry_result(&client, outcome.trial_id).await,
        Some(json!({"tracking": true}))
    );
    assert_eq!(outcome.result["tracking"], json!(true));
    assert_eq!(outcome.result["holdout"], json!(false));
    assert_eq!(outcome.result["holdout_sessions"], json!(0));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_stores_one_session_date_per_return() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-dates", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, live_from()).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    let returns = outcome.result["returns"].as_array().unwrap();
    let dates = outcome.result["session_dates"].as_array().unwrap();
    assert_eq!(dates.len(), returns.len());
    // The stored returns start at the engine's first day (1 February 2021): the 20 weekdays of
    // January before it are not stored.
    assert_eq!(dates.len(), 110);
    let texts: Vec<&str> = dates.iter().map(|d| d.as_str().unwrap()).collect();
    for text in &texts {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap();
    }
    assert!(texts.windows(2).all(|pair| pair[0] < pair[1]));
    let last_bar_date = date_of(*closes().last().unwrap());
    assert_eq!(last_bar_date, "2021-07-02");
    assert_eq!(texts.last().copied(), Some(last_bar_date.as_str()));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_is_left_out_of_backtest_results() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-left-out", &market());
    let job = job(dir.path(), strategy_params());
    let version = traded(&store, &client, &job, live_from()).await;
    let ordinary = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();

    let tracking = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    let results = scoring::backtest_results(&client).await.unwrap();
    assert!(results.iter().all(|r| r.trial_id != tracking.trial_id));
    assert!(results.iter().any(|r| r.trial_id == ordinary.trial_id));
    let found = scoring::tracking_result(&client, version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.trial_id, tracking.trial_id);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn holdout_and_tracking_together_are_refused() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("holdout-and-tracking", &market());
    let full = job(dir.path(), strategy_params());
    traded(&store, &client, &full, live_from()).await;
    set_holdout(&full, "2021-05-03", 10);
    let trials_before = trial_count(&client).await;
    let versions_before = version_count(&client).await;
    let looks_before = trial_registry::holdout_count(&client).await.unwrap();
    let both = RunOptions {
        holdout: true,
        ..tracking_options()
    };

    let err = run_backtest_job(&store, &full, both, now())
        .await
        .unwrap_err();

    assert!(matches!(err, EvalError::InvalidJob(_)));
    assert!(err.to_string().contains("not both"));
    assert_eq!(trial_count(&client).await, trials_before);
    assert_eq!(version_count(&client).await, versions_before);
    assert_eq!(
        trial_registry::holdout_count(&client).await.unwrap(),
        looks_before
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_ordinary_run_stores_tracking_false_and_no_session_dates() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("ordinary-tracking-false", &market());
    let job = job(dir.path(), strategy_params());

    let outcome = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();

    assert_eq!(outcome.result["tracking"], json!(false));
    assert!(outcome.result.get("session_dates").is_none());
    assert!(outcome.result.get("tracking_start").is_none());
    assert_eq!(registry_result(&client, outcome.trial_id).await, None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_stores_returns_only_from_the_engines_first_day() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-from-first-day", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, live_from()).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    let dates: Vec<&str> = outcome.result["session_dates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    let expected: Vec<String> = closes()
        .iter()
        .map(|ts| date_of(*ts))
        .filter(|date| date.as_str() >= "2021-02-01")
        .collect();
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(dates, expected);
    assert_eq!(dates.first().copied(), Some("2021-02-01"));
    assert_eq!(
        outcome.result["returns"].as_array().unwrap().len(),
        dates.len()
    );
    assert_eq!(outcome.result["tracking_start"], json!("2021-02-01"));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_stores_no_whole_span_figures() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-no-whole-span", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, live_from()).await;

    let ordinary = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();
    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    // The tracking result has the ordinary result's keys, plus the two tracking-only keys.
    let ordinary_keys: BTreeSet<&str> = ordinary
        .result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let tracking_keys: BTreeSet<&str> = outcome
        .result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected_keys = ordinary_keys;
    expected_keys.insert("session_dates");
    expected_keys.insert("tracking_start");
    assert_eq!(tracking_keys, expected_keys, "a key is not classified");

    // Exact partition: the non-null keys are the kept keys and the tracking keys. None of the
    // kept keys is null in this fixture (the bars file and holdout start are always present),
    // so every other key, the whole-span figures, must be null.
    let tracking_only = [
        "returns",
        "returns_complete",
        "session_dates",
        "tracking_start",
        "sessions",
    ];
    let expected_non_null: BTreeSet<&str> = TRACKING_KEPT_KEYS
        .iter()
        .copied()
        .chain(tracking_only)
        .collect();
    let non_null: BTreeSet<&str> = outcome
        .result
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(non_null, expected_non_null);

    assert_eq!(outcome.result["symbols"], json!(["FALL", "RISE"]));
    let kept = outcome.result["returns"].as_array().unwrap().len();
    assert_eq!(outcome.result["sessions"], json!(kept));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_job_hashes_exactly_the_strategy_object() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("config-hash", &market());
    let job = job(dir.path(), strategy_params());

    let outcome = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();

    // The engine hashes its config's strategy object the same way (crates/app/src/restore.rs); the
    // live trial, the equity series and a tracking run meet through this hash.
    let stored: Vec<u8> = client
        .query_one(
            "SELECT config_hash FROM strategy_version WHERE id = $1",
            &[&outcome.strategy_version_id.0],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        stored,
        config_hash(&job.strategy_params).as_bytes().to_vec()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_whose_bars_end_on_the_engines_first_day_is_accepted() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-same-day", &market());
    let job = job(dir.path(), strategy_params());
    // The last bar is 21:00 UTC on 2 July 2021; the engine first ran later that same day.
    traded(&store, &client, &job, utc(2021, 7, 2, 22, 0, 0, 0)).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    let returns = outcome.result["returns"].as_array().unwrap();
    assert_eq!(returns.len(), 1);
    assert_eq!(outcome.result["session_dates"], json!(["2021-07-02"]));
    assert_eq!(outcome.result["tracking_start"], json!("2021-07-02"));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_whose_engine_started_before_the_bars_keeps_every_return() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-before-bars", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, utc(2020, 12, 1, 0, 0, 0, 0)).await;

    let outcome = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    // Every return but the first session's: the first return is dated the second session.
    assert_eq!(
        outcome.result["returns"].as_array().unwrap().len(),
        SESSIONS - 1
    );
    assert_eq!(outcome.result["tracking_start"], json!("2020-12-01"));
    assert_eq!(outcome.result["session_dates"][0], json!("2021-01-05"));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_tracking_tail_matches_the_same_sessions_of_an_ordinary_run() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("tracking-tail-matches", &market());
    let job = job(dir.path(), strategy_params());
    traded(&store, &client, &job, live_from()).await;

    let ordinary = run_backtest_job(&store, &job, options(true, false, false), now())
        .await
        .unwrap();
    let tracking = run_backtest_job(&store, &job, tracking_options(), now())
        .await
        .unwrap();

    let whole = ordinary.result["returns"].as_array().unwrap();
    let kept = tracking.result["returns"].as_array().unwrap();
    assert_eq!(kept.len(), 110);
    assert_eq!(kept.as_slice(), &whole[whole.len() - kept.len()..]);
    assert_eq!(tracking.result["returns_complete"], json!(true));
    db.drop_db().await.unwrap();
}

/// The link from a real holdout look to the gate report: an ordinary backtest, then a holdout look
/// made on it with the same inputs and the same bar file, both read back through the store and
/// judged by the report. Reaching the holdout gate's "fewer than 252 sessions" note means every
/// stored key the earlier checks need is present and matches on both sides: the inputs, the
/// usability of both runs, the holdout sessions, the bar file, the requested start and the returns
/// prefix (the look's returns begin with the ordinary run's). A missing or differing key gives
/// another note, so this test fails if a result field is renamed or the engine's returns differ.
#[tokio::test]
async fn a_real_holdout_look_is_judged_against_the_ordinary_run_it_extends() {
    use nisse_eval::gate_report::{GateStatus, ReportInputs, TrialSeries, build_report};
    use nisse_eval::gates::Gates;

    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let dir = bars_dir("e2e-holdout", &market());
    // The boundary 2021-04-01 leaves 67 synthetic sessions after it (at least 60). The ordinary
    // run ends on 2021-03-31, the last session of March, as a real baseline ends on the last
    // session of a month (the default holdout starts on 1 January). That is the hard case for
    // the prefix rule: the ordinary run cannot know its final session is a month end and does
    // not rebalance there, while the look does. The returns still agree session for session,
    // because equity is recorded before the rebalance and its orders fill at the next open.
    let mut early = job(dir.path(), strategy_params());
    early.end = utc(2021, 3, 31, 23, 59, 59, 999_999_999);
    // Jobs in one directory share the gates file, so the boundary is set after the last `job`.
    let full = job(dir.path(), strategy_params());
    set_holdout(&full, "2021-04-01", 10);

    let ordinary = run_backtest_job(&store, &early, options(true, false, false), now())
        .await
        .unwrap();
    let look = run_backtest_job(&store, &full, options(true, false, true), now())
        .await
        .unwrap();

    let stored = scoring::backtest_results(&client).await.unwrap();
    let series = |trial_id: i64| {
        let trial = stored.iter().find(|t| t.trial_id == trial_id).unwrap();
        TrialSeries::from_stored(trial)
    };
    let inputs = ReportInputs {
        version: ordinary.strategy_version_id,
        gates: Gates::load(&full.gates_path).unwrap(),
        lifetime_trials: 2,
        holdout_looks: 1,
        trials: vec![series(ordinary.trial_id), series(look.trial_id)],
        replay: (0, 0),
        risk_breaches: 0,
        sent_orders: 0,
        live: None,
    };
    let report = build_report(&inputs);

    let holdout = report.gates.iter().find(|g| g.name == "holdout").unwrap();
    assert_eq!(holdout.status, GateStatus::NotEvaluated, "{}", holdout.note);
    assert_eq!(
        holdout.note,
        "the holdout baseline has fewer than 252 sessions with a non-zero return"
    );
    assert_eq!(
        report.info["holdout_run"]["baseline_trial_id"],
        json!(ordinary.trial_id)
    );
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(look.trial_id));
    db.drop_db().await.unwrap();
}
