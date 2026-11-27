#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P6: the start-up restore against a real Postgres (`TEST_DATABASE_URL`),
//! with the simulated broker, a manual clock and synthetic daily bar files.

use std::{
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicUsize, Ordering},
};

use chrono::NaiveDate;
use nisse_alpaca_adapter::{Feed, VENUE};
use nisse_app::{
    AppError,
    config::EngineConfig,
    ingest::bar_file_path,
    restore::{Restored, restore, take_engine_lock},
};
use nisse_bars::write_bars;
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
    PriceType, Quantity, Symbol, UnixNanos, Venue,
};
use nisse_core::{ManualClock, PositionEvent, RiskEvent, RiskEventKind};
use nisse_engine::{CostModel, SimBroker};
use nisse_store::{
    Store, StoreWriter, WriterConfig,
    tables::{position_event, risk_event},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const SECOND: u64 = 1_000_000_000;
const UNIVERSE: [&str; 3] = ["AAPL", "MSFT", "NVDA"];
const BENCHMARK: &str = "SPY";
/// Weekday sessions of January 2026 (Monday to Friday).
const SESSION_DAYS: [u32; 5] = [5, 6, 7, 8, 9];

/// Removes its directory when dropped, including on a failed assertion.
struct Scratch(PathBuf);

impl Scratch {
    /// A path under the system temp dir, unique to this process and call. Not created.
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("nisse-app-restore-test-{}-{n}", process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    /// The same path, created as an empty directory.
    fn new_dir() -> Self {
        let scratch = Self::new();
        std::fs::create_dir_all(&scratch.0).unwrap();
        scratch
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn symbol(text: &str) -> Symbol {
    Symbol::new_checked(text).unwrap()
}

/// 21:00 UTC on `day` of January 2026: the close of that session, in nanoseconds.
fn close_of(day: u32) -> UnixNanos {
    let secs = NaiveDate::from_ymd_opt(2026, 1, day)
        .unwrap()
        .and_hms_opt(21, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    UnixNanos::from(u64::try_from(secs).unwrap() * SECOND)
}

fn date_of(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
}

/// A daily bar for `symbol_text` stamped at `ts` (event and init time).
fn bar(symbol_text: &str, ts: UnixNanos) -> Bar {
    let instrument_id = InstrumentId::new(symbol(symbol_text), Venue::new(VENUE));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    Bar::new(
        bar_type,
        Price::from("10.00"),
        Price::from("11.00"),
        Price::from("9.00"),
        Price::from("10.50"),
        Quantity::from("1000"),
        ts,
        ts,
    )
}

/// Writes one bar file holding every session in `days` for the universe and the benchmark.
/// The file is named after the last day, so it is the one the reader picks.
fn write_session_bars(dir: &Path, days: &[u32]) {
    let mut symbols: Vec<&str> = UNIVERSE.to_vec();
    symbols.push(BENCHMARK);
    symbols.sort_unstable();
    let mut bars = Vec::new();
    for &day in days {
        for &text in &symbols {
            bars.push(bar(text, close_of(day)));
        }
    }
    let last = *days.last().unwrap();
    write_bars(&bar_file_path(dir, date_of(last)), &bars).unwrap();
}

/// The engine config the tests run with: the paper strategy, three symbols and the benchmark.
fn config(bars_dir: PathBuf, limits_path: PathBuf) -> EngineConfig {
    let paper: Value =
        serde_json::from_str(include_str!("../../../config/engine/paper.json")).unwrap();
    EngineConfig {
        strategy: paper["strategy"].clone(),
        symbols: UNIVERSE.iter().map(|text| symbol(text)).collect(),
        benchmark: symbol(BENCHMARK),
        feed: Feed::Iex,
        history_start: NaiveDate::from_ymd_opt(2015, 1, 1).unwrap(),
        bars_dir,
        limits_path,
        seed: 0,
        source: "engine".to_owned(),
    }
}

/// The repository's limits file.
fn repo_limits() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    ))
}

/// A simulated broker with no costs.
fn broker() -> SimBroker {
    SimBroker::new(
        Decimal::from(100_000),
        CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
    )
}

/// A database with a store on one connection and a record writer on another.
async fn setup() -> (ScratchDb, Store, StoreWriter) {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let (sink, _handle) = StoreWriter::spawn(db.client().await.unwrap(), WriterConfig::default());
    (db, store, sink)
}

fn risk(kind: RiskEventKind, detail: Value, ts: UnixNanos) -> RiskEvent {
    RiskEvent { kind, detail, ts }
}

async fn limits_loaded_count(store: &Store) -> usize {
    store
        .load_risk_events()
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, RiskEventKind::LimitsLoaded))
        .count()
}

// ---- tests -------------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_account_restores_empty() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new();
    let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());

    let restored = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();

    assert_eq!(restored.bars_fed, 0);
    assert_eq!(restored.newest_bar_ts, None);
    assert!(restored.trader.state().positions.quantities().is_empty());
    assert_eq!(restored.trader.state().tracker.open_orders().count(), 0);
    assert!(!restored.trader.risk().state().kill_switch_engaged);

    sink.flush().await.unwrap();
    assert_eq!(limits_loaded_count(&store).await, 1);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn limits_loaded_is_recorded_once_per_hash() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new();
    let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());

    let _first = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    let _second = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    sink.flush().await.unwrap();
    assert_eq!(limits_loaded_count(&store).await, 1);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_changed_limits_file_is_recorded_again() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new_dir();
    let cfg_path = scratch.path().join("limits.json");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(repo_limits()).unwrap()).unwrap();
    value["heartbeat_max_age_seconds"] = json!(301);
    std::fs::write(&cfg_path, value.to_string()).unwrap();

    let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());
    let _first = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    let changed = config(scratch.path().join("no-bars-yet"), cfg_path);
    let _second = restore(
        &changed,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    sink.flush().await.unwrap();
    assert_eq!(limits_loaded_count(&store).await, 2);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn positions_and_bars_come_back() {
    let (db, store, sink) = setup().await;
    let client = db.client().await.unwrap();
    let rows = [
        ("AAPL", "5", "100", 1),
        ("MSFT", "-3", "50", 2),
        ("AAPL", "10", "101", 3),
    ]
    .map(|(sym, qty, cost, ts)| PositionEvent {
        symbol: symbol(sym),
        qty_after: qty.parse().unwrap(),
        avg_cost: cost.parse().unwrap(),
        ts: UnixNanos::from(ts * SECOND),
    });
    position_event::insert_many(&client, &rows).await.unwrap();

    let scratch = Scratch::new_dir();
    write_session_bars(scratch.path(), &SESSION_DAYS);
    let cfg = config(scratch.path().to_path_buf(), repo_limits());

    let restored = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();

    let state = restored.trader.state();
    let aapl = state.positions.get(symbol("AAPL"));
    assert_eq!(aapl.qty, Decimal::from(10));
    assert_eq!(aapl.avg_cost, Decimal::from(101));
    assert_eq!(state.positions.get(symbol("MSFT")).qty, Decimal::from(-3));
    assert!(state.positions.get(symbol("NVDA")).qty.is_zero());

    // Three universe symbols over five sessions; the benchmark is not fed.
    assert_eq!(restored.bars_fed, UNIVERSE.len() * SESSION_DAYS.len());
    assert_eq!(restored.newest_bar_ts, Some(close_of(9)));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn bars_after_now_are_not_fed() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new_dir();
    write_session_bars(scratch.path(), &SESSION_DAYS);
    let cfg = config(scratch.path().to_path_buf(), repo_limits());

    // The clock is at the close of the fourth session: the fifth session's bars are later.
    let restored = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(8)),
    )
    .await
    .unwrap();

    assert_eq!(restored.bars_fed, UNIVERSE.len() * 4);
    assert_eq!(restored.newest_bar_ts, Some(close_of(8)));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_kill_switch_comes_back_engaged() {
    let (db, store, sink) = setup().await;
    let client = db.client().await.unwrap();
    risk_event::insert_many(
        &client,
        &[risk(
            RiskEventKind::KillSwitch,
            json!({"engaged": true, "by": "owner"}),
            close_of(5),
        )],
    )
    .await
    .unwrap();

    let scratch = Scratch::new();
    let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());
    let restored = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();

    assert!(restored.trader.risk().state().kill_switch_engaged);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unreadable_risk_event_stops_the_restore() {
    for bad in [
        risk(
            RiskEventKind::DrawdownMode,
            json!({"mode": "sideways"}),
            close_of(5),
        ),
        risk(
            RiskEventKind::KillSwitch,
            json!({"engaged": "yes"}),
            close_of(5),
        ),
    ] {
        let (db, store, sink) = setup().await;
        let client = db.client().await.unwrap();
        risk_event::insert_many(&client, &[bad]).await.unwrap();

        let scratch = Scratch::new();
        let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());
        let outcome = restore(
            &cfg,
            &store,
            sink.clone(),
            broker(),
            ManualClock::new(close_of(9)),
        )
        .await
        .err();
        assert!(
            matches!(outcome, Some(AppError::Risk(_))),
            "got {outcome:?}"
        );

        db.drop_db().await.unwrap();
    }
}

#[tokio::test]
async fn the_engine_lock_is_exclusive() {
    let db = ScratchDb::create().await;
    let first = Store::new(db.client().await.unwrap());
    let second = Store::new(db.client().await.unwrap());

    take_engine_lock(&first).await.unwrap();
    assert!(matches!(
        take_engine_lock(&second).await,
        Err(AppError::Busy)
    ));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_second_restore_reuses_the_strategy_version() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new();
    let cfg = config(scratch.path().join("no-bars-yet"), repo_limits());

    let first: Restored<'_, _, _> = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    let second = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .unwrap();
    assert_eq!(first.strategy_version_id, second.strategy_version_id);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unknown_strategy_stops_the_restore() {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new();
    let mut cfg = config(scratch.path().join("no-bars-yet"), repo_limits());
    cfg.strategy = json!({"strategy": "no_such_strategy"});

    let outcome = restore(
        &cfg,
        &store,
        sink.clone(),
        broker(),
        ManualClock::new(close_of(9)),
    )
    .await
    .err();
    assert!(
        matches!(outcome, Some(AppError::Engine(_))),
        "got {outcome:?}"
    );

    db.drop_db().await.unwrap();
}
