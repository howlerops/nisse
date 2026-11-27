#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P7: the `kill`, `release-kill` and `status` commands against a real Postgres
//! (`TEST_DATABASE_URL`), with the simulated broker, a manual clock and a fake market.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use nisse_app::{
    commands::{KillOutcome, Status, kill, release_kill, status},
    config::EngineConfig,
    session::{RebalanceStep, SessionCloseOutcome, session_close, tick},
};
use nisse_core::{Broker, ManualClock};
use nisse_risk::DrawdownMode;
use nisse_store::{
    Store,
    tables::scoring::{self, ENGINE_LOCK},
};
use serde_json::json;
use support::{
    FakeMarket, Scratch, broker, close_at, plus_minutes, repo_limits, session_bars, setup,
    weekday_calendar,
};

/// The universe of the paper config, in its order (the price path follows this order).
const SYMBOLS: [&str; 8] = ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"];
/// How long the kill test waits for an engine run in flight.
const SHORT_WAIT: Duration = Duration::from_secs(1);

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// The paper config, with the strategy's windows shortened so that the sessions of the fake
/// market are enough to decide. The paper universe, benchmark and limits are kept.
///
/// The same shape as `session.rs`'s `session_config`, which is private to that test binary.
fn session_config(bars_dir: PathBuf) -> EngineConfig {
    let paper = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/engine/paper.json"
    ));
    let mut config = EngineConfig::load(paper).unwrap();
    config.strategy["lookback_days"] = json!(20);
    config.strategy["skip_days"] = json!(5);
    config.strategy["vol_window"] = json!(10);
    config.bars_dir = bars_dir;
    config.limits_path = repo_limits();
    config
}

/// `SELECT count(*)` of `table`.
async fn count(store: &Store, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    store
        .client()
        .query_one(sql.as_str(), &[])
        .await
        .unwrap()
        .get(0)
}

/// The number of `kill_switch` rows of `risk_event`.
async fn kill_switch_rows(store: &Store) -> i64 {
    store
        .client()
        .query_one(
            "SELECT count(*) FROM risk_event WHERE kind = 'kill_switch'",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

/// Sends the month-end orders the way `session.rs`'s `a_month_end_close_decides_and_sends_orders`
/// does, then releases the engine lock that the close holds on the store's connection, so that
/// a test can decide who holds it next.
async fn month_end_with_open_orders(
    store: &Store,
    sink: nisse_store::StoreWriter,
    config: &EngineConfig,
    market: &FakeMarket,
    sim: &nisse_engine::SimBroker,
) {
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 10));
    let outcome = session_close(config, market, store, sink, sim, &clock)
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            SessionCloseOutcome::Ran {
                rebalance: RebalanceStep::Ran(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(!sim.open_orders().await.unwrap().orders.is_empty());
    assert!(
        scoring::advisory_unlock(store.client(), ENGINE_LOCK)
            .await
            .unwrap()
    );
}

/// A fresh database, the scratch bar directory, the paper config and the fake market.
struct Fixture {
    db: nisse_store::testing::ScratchDb,
    store: Store,
    sink: nisse_store::StoreWriter,
    _scratch: Scratch,
    config: EngineConfig,
    market: FakeMarket,
}

async fn fixture() -> Fixture {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new_dir();
    let config = session_config(scratch.path().to_path_buf());
    let calendar = weekday_calendar();
    let bars = session_bars(&calendar, &SYMBOLS);
    let market = FakeMarket::new(calendar, bars);
    Fixture {
        db,
        store,
        sink,
        _scratch: scratch,
        config,
        market,
    }
}

#[tokio::test]
async fn kill_records_the_switch_and_cancels_open_orders() {
    let f = fixture().await;
    let sim = broker();
    month_end_with_open_orders(&f.store, f.sink.clone(), &f.config, &f.market, &sim).await;

    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));
    let outcome = kill(&f.store, &sim, &clock, "ana", SHORT_WAIT)
        .await
        .unwrap();

    assert_eq!(outcome, KillOutcome { engine_idle: true });
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    assert!(
        status(&f.config, &f.store, &clock)
            .await
            .unwrap()
            .kill_switch
    );

    // The next market-open tick sends nothing: the switch is engaged and nothing is owed.
    let open = ManualClock::new(support::utc_at(date(2026, 1, 2), 15, 0));
    let tick_outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &open)
        .await
        .unwrap();
    assert!(tick_outcome.market_open);
    assert!(tick_outcome.rebalance.is_none(), "{tick_outcome:?}");
    assert!(sim.open_orders().await.unwrap().orders.is_empty());

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn kill_with_an_engine_in_flight_still_engages() {
    let f = fixture().await;
    let sim = broker();
    month_end_with_open_orders(&f.store, f.sink.clone(), &f.config, &f.market, &sim).await;

    // Another connection plays the engine run in flight: it holds the engine lock.
    let other = f.db.client().await.unwrap();
    assert!(
        scoring::try_advisory_lock(&other, ENGINE_LOCK)
            .await
            .unwrap()
    );

    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));
    let outcome = kill(&f.store, &sim, &clock, "ana", SHORT_WAIT)
        .await
        .unwrap();

    assert_eq!(outcome, KillOutcome { engine_idle: false });
    assert_eq!(kill_switch_rows(&f.store).await, 1);
    assert!(
        status(&f.config, &f.store, &clock)
            .await
            .unwrap()
            .kill_switch
    );
    assert!(sim.open_orders().await.unwrap().orders.is_empty());

    assert!(scoring::advisory_unlock(&other, ENGINE_LOCK).await.unwrap());
    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn release_kill_lowers_the_switch() {
    let f = fixture().await;
    let sim = broker();
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));

    kill(&f.store, &sim, &clock, "ana", SHORT_WAIT)
        .await
        .unwrap();
    release_kill(&f.store, &clock, "ana").await.unwrap();

    assert!(
        !status(&f.config, &f.store, &clock)
            .await
            .unwrap()
            .kill_switch
    );
    assert_eq!(kill_switch_rows(&f.store).await, 2);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn status_of_a_fresh_database() {
    let f = fixture().await;
    let clock = ManualClock::new(support::utc_at(date(2026, 1, 2), 15, 0));

    let report = status(&f.config, &f.store, &clock).await.unwrap();

    assert_eq!(
        report,
        Status {
            kill_switch: false,
            drawdown_mode: DrawdownMode::Normal,
            equity_peak: None,
            resize_owed: false,
            limits_hash: None,
            positions: Vec::new(),
            live_orders: 0,
            last_heartbeat: None,
            newest_bar_file: None,
        }
    );
    // Reads only: nothing was written.
    assert_eq!(count(&f.store, "strategy_version").await, 0);
    assert_eq!(count(&f.store, "risk_event").await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn status_after_a_close() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    let report = status(&f.config, &f.store, &clock).await.unwrap();

    assert!(report.last_heartbeat.is_some());
    assert_eq!(
        report.newest_bar_file.as_deref(),
        Some("2025-12-15.parquet")
    );
    assert!(report.limits_hash.is_some());
    assert!(!report.kill_switch);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn status_is_stable_and_writes_nothing() {
    let f = fixture().await;
    let sim = broker();
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 15)), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    let first = status(&f.config, &f.store, &clock).await.unwrap();
    let second = status(&f.config, &f.store, &clock).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(count(&f.store, "strategy_version").await, 1);

    f.db.drop_db().await.unwrap();
}
