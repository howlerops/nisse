#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P6: the start-up restore against a real Postgres (`TEST_DATABASE_URL`),
//! with the simulated broker, a manual clock and synthetic daily bar files.

mod support;

use std::path::Path;

use chrono::NaiveDate;
use nisse_app::{
    AppError,
    ingest::bar_file_path,
    restore::{Restored, restore, take_engine_lock},
};
use nisse_bars::write_bars;
use nisse_core::nautilus::UnixNanos;
use nisse_core::{ManualClock, PositionEvent, RiskEvent, RiskEventKind, config_hash};
use nisse_store::{
    Store,
    tables::{position_event, risk_event},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use support::{
    BENCHMARK, Scratch, UNIVERSE, bar, broker, close_at, config, repo_limits, setup, symbol,
};

const SECOND: u64 = 1_000_000_000;
/// Weekday sessions of January 2026 (Monday to Friday).
const SESSION_DAYS: [u32; 5] = [5, 6, 7, 8, 9];

/// 21:00 UTC on `day` of January 2026: the close of that session, in nanoseconds.
fn close_of(day: u32) -> UnixNanos {
    close_at(date_of(day))
}

fn date_of(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
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

#[tokio::test]
async fn the_engine_hashes_exactly_the_strategy_object() {
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

    // The backtest job hashes its job's strategy object the same way
    // (crates/eval/src/backtest_job.rs); the live trial, the equity series and a tracking run
    // meet through this hash.
    let row = store
        .client()
        .query_one(
            "SELECT config_hash FROM strategy_version WHERE id = $1",
            &[&restored.strategy_version_id.0],
        )
        .await
        .unwrap();
    let stored: Vec<u8> = row.get(0);
    assert_eq!(stored.len(), 32);
    assert_eq!(
        stored.as_slice(),
        config_hash(&cfg.strategy).as_bytes().as_slice()
    );

    db.drop_db().await.unwrap();
}
