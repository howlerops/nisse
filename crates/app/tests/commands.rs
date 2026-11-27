#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P7: the `kill`, `release-kill` and `status` commands against a real Postgres
//! (`TEST_DATABASE_URL`), with the simulated broker, a manual clock and a fake market.

mod support;

use std::time::Duration;

use chrono::NaiveDate;
use nisse_app::{
    commands::{Status, kill, release_kill, status},
    config::EngineConfig,
    session::{RebalanceStep, SessionCloseOutcome, session_close, tick},
};
use nisse_core::{Broker, ManualClock};
use nisse_risk::DrawdownMode;
use nisse_store::{
    Store,
    tables::scoring::{self, ENGINE_LOCK},
};
use support::{
    FakeMarket, Scratch, broker, close_at, count, plus_minutes, session_bars, session_config,
    setup, weekday_calendar,
};

/// The universe of the paper config, in its order (the price path follows this order).
const SYMBOLS: [&str; 8] = ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"];
/// How long the kill test waits for an engine run in flight.
const SHORT_WAIT: Duration = Duration::from_secs(1);

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A store that is already connected, in the form `kill` takes.
async fn have(store: &Store) -> Result<&Store, String> {
    Ok(store)
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
    let outcome = kill(have(&f.store), Some(&sim), &clock, "ana", SHORT_WAIT).await;

    assert!(outcome.complete(), "{outcome:?}");
    assert!(outcome.errors.is_empty());
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
    let outcome = kill(have(&f.store), Some(&sim), &clock, "ana", SHORT_WAIT).await;

    assert!(!outcome.engine_idle);
    assert!(
        outcome.switch_recorded && outcome.orders_cancelled,
        "{outcome:?}"
    );
    assert!(!outcome.complete());
    assert_eq!(outcome.errors.len(), 1, "{outcome:?}");
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

    kill(have(&f.store), Some(&sim), &clock, "ana", SHORT_WAIT).await;
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

#[tokio::test]
async fn kill_without_a_broker_still_records_the_switch() {
    let f = fixture().await;
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));

    let outcome = kill(
        have(&f.store),
        None::<&nisse_engine::SimBroker>,
        &clock,
        "ana",
        SHORT_WAIT,
    )
    .await;

    assert!(
        outcome.switch_recorded && outcome.engine_idle,
        "{outcome:?}"
    );
    assert!(!outcome.orders_cancelled);
    assert!(!outcome.complete());
    assert_eq!(outcome.errors.len(), 1, "{outcome:?}");
    assert!(
        status(&f.config, &f.store, &clock)
            .await
            .unwrap()
            .kill_switch
    );

    f.db.drop_db().await.unwrap();
}

/// A broker that accepts a cancel of everything and then cancels nothing: what a per-order
/// failure inside a "207 Multi-Status" answer looks like from outside.
struct Sticky<'a>(&'a nisse_engine::SimBroker);

impl Broker for Sticky<'_> {
    type Error = <nisse_engine::SimBroker as Broker>::Error;

    async fn submit(
        &self,
        intent: &nisse_core::OrderIntent,
    ) -> Result<nisse_core::BrokerOrder, nisse_core::SubmitError<Self::Error>> {
        self.0.submit(intent).await
    }

    async fn cancel(
        &self,
        venue_order_id: nisse_core::nautilus::VenueOrderId,
    ) -> Result<(), Self::Error> {
        self.0.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn replace(
        &self,
        venue_order_id: nisse_core::nautilus::VenueOrderId,
        request: &nisse_core::ReplaceRequest,
    ) -> Result<nisse_core::BrokerOrder, nisse_core::SubmitError<Self::Error>> {
        self.0.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: nisse_core::nautilus::ClientOrderId,
    ) -> Result<Option<nisse_core::BrokerOrder>, Self::Error> {
        self.0.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<nisse_core::OpenOrders, Self::Error> {
        self.0.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<nisse_core::BrokerPosition>, Self::Error> {
        self.0.positions().await
    }

    async fn account(&self) -> Result<nisse_core::AccountSnapshot, Self::Error> {
        self.0.account().await
    }
}

#[tokio::test]
async fn kill_reports_orders_the_cancel_left_open() {
    let f = fixture().await;
    let sim = broker();
    month_end_with_open_orders(&f.store, f.sink.clone(), &f.config, &f.market, &sim).await;
    let left = sim.open_orders().await.unwrap().orders.len();

    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));
    let sticky = Sticky(&sim);
    let outcome = kill(have(&f.store), Some(&sticky), &clock, "ana", SHORT_WAIT).await;

    assert!(
        outcome.switch_recorded && outcome.engine_idle,
        "{outcome:?}"
    );
    assert!(!outcome.orders_cancelled, "{outcome:?}");
    assert!(!outcome.complete());
    assert_eq!(
        outcome.errors,
        vec![format!("{left} orders are still open after the cancel")]
    );

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn kill_without_a_store_still_cancels_and_confirms() {
    let f = fixture().await;
    let sim = broker();
    month_end_with_open_orders(&f.store, f.sink.clone(), &f.config, &f.market, &sim).await;

    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));
    let no_store = async { Err::<&Store, String>("connection refused".to_owned()) };
    let outcome = kill(no_store, Some(&sim), &clock, "ana", SHORT_WAIT).await;

    assert!(outcome.orders_cancelled, "{outcome:?}");
    assert!(!outcome.switch_recorded && !outcome.engine_idle);
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    assert_eq!(kill_switch_rows(&f.store).await, 0);
    assert_eq!(outcome.errors.len(), 2, "{outcome:?}");

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn kill_with_a_store_that_never_answers_still_cancels() {
    let f = fixture().await;
    let sim = broker();
    month_end_with_open_orders(&f.store, f.sink.clone(), &f.config, &f.market, &sim).await;

    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 31)), 20));
    // A connection attempt that hangs: `kill` gives it ten seconds and no more.
    let started = std::time::Instant::now();
    let outcome = kill(
        std::future::pending::<Result<&Store, String>>(),
        Some(&sim),
        &clock,
        "ana",
        SHORT_WAIT,
    )
    .await;

    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(outcome.orders_cancelled, "{outcome:?}");
    assert!(!outcome.switch_recorded && !outcome.engine_idle);
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    assert!(
        outcome.errors[0].contains("the store did not answer"),
        "{outcome:?}"
    );

    f.db.drop_db().await.unwrap();
}
