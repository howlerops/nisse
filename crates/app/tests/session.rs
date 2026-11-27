#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P6b: the `tick` and `session-close` commands against a real Postgres
//! (`TEST_DATABASE_URL`), with the simulated broker, a manual clock and a fake market.

mod support;

use chrono::NaiveDate;
use nisse_app::{
    AppError,
    commands::{engage_kill_switch, release_kill},
    config::EngineConfig,
    ingest::bar_file_path,
    restore::take_engine_lock,
    session::{
        RESIZE_RUN_KEY, RESIZE_STEP, RebalanceStep, SessionCloseOutcome, session_close, tick,
    },
};
use nisse_core::nautilus::{Bar, ClientOrderId, UnixNanos, VenueOrderId};
use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, Clock, ManualClock, MarketCalendar,
    OpenOrders, OrderIntent, ReplaceRequest, RiskEvent, RiskEventKind, Session, SubmitError,
};
use nisse_engine::{EngineError, SimBroker, SimError};
use nisse_risk::DrawdownMode;
use nisse_store::{
    Store,
    tables::{engine_step::StepStatus, risk_event},
    testing::ScratchDb,
};
use serde_json::{Value, json};
use support::{
    FakeMarket, Scratch, broker, close_at, count, plus_minutes, session_bars, session_config,
    setup, weekday_calendar,
};

/// The universe of the paper config, in its order (the price path follows this order).
const SYMBOLS: [&str; 8] = ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"];

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// `hour:00` UTC on `day`.
fn at(day: NaiveDate, hour: u32) -> UnixNanos {
    support::utc_at(day, hour, 0)
}

/// Everything a test needs. The bars are the session bars, minus those `keep` rejects.
struct Fixture {
    db: ScratchDb,
    store: Store,
    sink: nisse_store::StoreWriter,
    scratch: Scratch,
    config: EngineConfig,
    market: FakeMarket,
    calendar: MarketCalendar,
}

async fn fixture_with(keep: impl Fn(&Bar) -> bool) -> Fixture {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new_dir();
    let config = session_config(scratch.path().to_path_buf());
    let calendar = weekday_calendar();
    let bars = session_bars(&calendar, &SYMBOLS)
        .into_iter()
        .filter(|bar| keep(bar))
        .collect();
    let market = FakeMarket::new(calendar.clone(), bars);
    Fixture {
        db,
        store,
        sink,
        scratch,
        config,
        market,
        calendar,
    }
}

async fn fixture() -> Fixture {
    fixture_with(|_| true).await
}

/// The run key of the `session-close` run for `day`.
fn run_key(day: NaiveDate) -> String {
    format!("session-close/{day}")
}

/// The risk event of a peak of `peak` (as the engine writes it), stamped at `ts`.
fn peak_event(peak: &str, ts: UnixNanos) -> RiskEvent {
    RiskEvent {
        kind: RiskEventKind::EquityPeak,
        detail: json!({ "peak": peak }),
        ts,
    }
}

#[tokio::test]
async fn off_session_nothing_happens() {
    let f = fixture().await;
    let sim = broker();
    // Seven hours after the close of 15 December 2025: the newest close is too old.
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 15)), 7 * 60));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert_eq!(outcome, SessionCloseOutcome::NoSession);
    assert_eq!(count(&f.store, "engine_step").await, 0);
    assert_eq!(count(&f.store, "heartbeat").await, 0);
    assert!(!bar_file_path(f.scratch.path(), date(2025, 12, 15)).exists());
    assert_eq!(f.market.calls(), 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_close_exactly_six_hours_old_is_still_decided() {
    let f = fixture().await;
    let sim = broker();
    let clock = ManualClock::new(plus_minutes(close_at(date(2025, 12, 15)), 6 * 60));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        SessionCloseOutcome::Ran {
            rebalance: RebalanceStep::NotDue,
            ..
        }
    ));

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_mid_month_close_ingests_and_does_not_decide() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    match outcome {
        SessionCloseOutcome::Ran {
            session_date,
            rebalance,
            ..
        } => {
            assert_eq!(session_date, day);
            assert_eq!(rebalance, RebalanceStep::NotDue);
        }
        other => panic!("expected a run, got {other:?}"),
    }
    assert!(bar_file_path(f.scratch.path(), day).exists());
    let key = run_key(day);
    for step in ["ingest", "reconcile", "observe", "rebalance"] {
        let status = f.store.step_status(&key, step).await.unwrap();
        assert_eq!(
            status.map(|(s, _)| s),
            Some(StepStatus::Done),
            "step {step}"
        );
    }
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(count(&f.store, "heartbeat").await, 1);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_month_end_close_decides_and_sends_orders() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    let report = match outcome {
        SessionCloseOutcome::Ran {
            rebalance: RebalanceStep::Ran(report),
            ..
        } => report,
        other => panic!("expected a rebalance, got {other:?}"),
    };
    assert_eq!(report.decisions.len(), SYMBOLS.len());
    assert!(report.orders_submitted > 0, "{report:?}");
    assert_eq!(count(&f.store, "decision").await, SYMBOLS.len() as i64);

    let open = sim.open_orders().await.unwrap();
    assert!(!open.orders.is_empty());

    let (status, detail) = f
        .store
        .step_status(&run_key(day), "rebalance")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);
    assert_eq!(detail["due"], json!(true));

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_second_run_of_the_same_close_sends_nothing() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    let decisions = count(&f.store, "decision").await;
    let calls = f.market.calls();

    let second = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(matches!(
        second,
        SessionCloseOutcome::Ran {
            rebalance: RebalanceStep::AlreadyDone,
            ..
        }
    ));
    assert_eq!(count(&f.store, "decision").await, decisions);
    assert_eq!(f.market.calls(), calls, "the bar file exists: no fetch");

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_hole_in_the_universe_fails_the_ingest_step() {
    let day = date(2025, 12, 15);
    let f = fixture_with(|bar| {
        !(bar.bar_type.instrument_id().symbol.as_str() == "DBC" && bar.ts_event == close_at(day))
    })
    .await;
    let sim = broker();
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock).await;

    assert!(
        matches!(outcome, Err(AppError::Market(_))),
        "got {outcome:?}"
    );
    let row = f
        .store
        .client()
        .query_one(
            "SELECT status, detail::text FROM engine_step \
             WHERE run_key = $1 AND step = 'ingest' ORDER BY id DESC LIMIT 1",
            &[&run_key(day)],
        )
        .await
        .unwrap();
    let status: String = row.get(0);
    let detail: String = row.get(1);
    assert_eq!(status, "failed");
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert!(
        detail["error"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{detail}"
    );
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(count(&f.store, "heartbeat").await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn fills_are_picked_up_by_the_next_tick() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    let decisions = count(&f.store, "decision").await;

    // The next session (the calendar has every weekday, so 1 January) opens: the broker fills
    // the orders at its bars. The tick's reconciliation must find those fills.
    let next = *f.calendar.next_session(close_at(day)).unwrap();
    let next_bars: Vec<Bar> = session_bars(&f.calendar, &SYMBOLS)
        .into_iter()
        .filter(|bar| bar.ts_event == next.close)
        .collect();
    sim.on_session(&next_bars).unwrap();

    let clock = ManualClock::new(at(next.date, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(outcome.fills > 0, "{outcome:?}");
    assert!(outcome.market_open);
    assert_eq!(outcome.rebalance, None);
    assert!(!f.store.load_positions().await.unwrap().is_empty());
    assert_eq!(count(&f.store, "decision").await, decisions);
    assert_eq!(count(&f.store, "heartbeat").await, 2);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tick_with_no_bar_file_yet_reconciles_and_writes_a_heartbeat() {
    let f = fixture().await;
    let sim = broker();
    let clock = ManualClock::new(at(date(2025, 12, 15), 15));

    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(outcome.market_open);
    assert_eq!(outcome.fills, 0);
    assert_eq!(outcome.mode_change, None);
    assert_eq!(outcome.rebalance, None);
    assert_eq!(count(&f.store, "heartbeat").await, 1);
    assert_eq!(f.market.calls(), 0, "a tick never fetches bars");

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tick_with_the_lock_held_elsewhere_is_busy() {
    let f = fixture().await;
    let other = Store::new(f.db.client().await.unwrap());
    take_engine_lock(&other).await.unwrap();
    let sim = broker();
    let clock = ManualClock::new(at(date(2025, 12, 15), 15));

    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock).await;

    assert!(matches!(outcome, Err(AppError::Busy)), "got {outcome:?}");
    assert_eq!(count(&f.store, "heartbeat").await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_step_down_seen_on_a_closed_market_is_carried_to_the_next_open_tick() {
    let f = fixture().await;
    let sim = broker();
    // A mid-month close first, so there is a bar file and an equity peak of the account's
    // 100_000.
    let mid = date(2025, 12, 15);
    let clock = ManualClock::new(plus_minutes(close_at(mid), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    // A peak of 200_000 is newer than the 100_000 peak: the account is at a 50% drawdown,
    // which is past the flat threshold (15%), so the mode goes from normal to flat.
    risk_event::insert_many(
        f.store.client(),
        &[peak_event("200000", at(date(2025, 12, 1), 21))],
    )
    .await
    .unwrap();

    // Saturday 20 December 2025: the market is closed.
    let saturday = date(2025, 12, 20);
    let clock = ManualClock::new(at(saturday, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(!outcome.market_open);
    assert_eq!(
        outcome.mode_change,
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );
    assert_eq!(outcome.rebalance, None);
    let (status, detail) = f
        .store
        .step_status(RESIZE_RUN_KEY, RESIZE_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Started);
    assert_eq!(detail, json!({"from": "normal", "to": "flat"}));

    // Monday 22 December 2025, 15:00 UTC: the market is open and the resize is owed.
    let monday = date(2025, 12, 22);
    let clock = ManualClock::new(at(monday, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(outcome.market_open);
    assert!(outcome.rebalance.is_some(), "{outcome:?}");
    let (status, _) = f
        .store
        .step_status(RESIZE_RUN_KEY, RESIZE_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);

    // Tuesday 23 December 2025: no resize is owed. The mode is flat, but nothing is held and
    // no order is open, so `needs_flatten` is false and there is nothing to rebalance.
    let clock = ManualClock::new(at(date(2025, 12, 23), 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    assert_eq!(outcome.rebalance, None);
    assert_eq!(count(&f.store, "heartbeat").await, 4);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_owed_resize_makes_a_mid_month_close_rebalance() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    f.store
        .record_step(
            RESIZE_RUN_KEY,
            RESIZE_STEP,
            StepStatus::Started,
            &json!({"from": "normal", "to": "halved"}),
            close_at(day),
        )
        .await
        .unwrap();
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        SessionCloseOutcome::Ran {
            rebalance: RebalanceStep::Ran(_),
            ..
        }
    ));
    let (status, _) = f
        .store
        .step_status(RESIZE_RUN_KEY, RESIZE_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);

    f.db.drop_db().await.unwrap();
}

/// The newest status of the owed-rebalance step.
async fn owed_status(store: &Store) -> Option<StepStatus> {
    store
        .step_status(RESIZE_RUN_KEY, RESIZE_STEP)
        .await
        .unwrap()
        .map(|(status, _)| status)
}

/// Marks a rebalance as owed, as a step-down would.
async fn owe(store: &Store, ts: UnixNanos) {
    store
        .record_step(
            RESIZE_RUN_KEY,
            RESIZE_STEP,
            StepStatus::Started,
            &json!({"from": "normal", "to": "halved"}),
            ts,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_rerun_after_a_crash_mid_rebalance_orders_nothing_twice() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    let sent = sim.open_orders().await.unwrap().orders.len();
    assert!(sent > 0);

    // As if the process had died after sending the orders and before recording `done`: the
    // newest row of the step is `started`.
    f.store
        .record_step(
            &run_key(day),
            "rebalance",
            StepStatus::Started,
            &json!({}),
            clock.now(),
        )
        .await
        .unwrap();

    let later = ManualClock::new(plus_minutes(close_at(day), 40));
    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &later)
        .await
        .unwrap();

    let report = match outcome {
        SessionCloseOutcome::Ran {
            rebalance: RebalanceStep::Ran(report),
            ..
        } => report,
        other => panic!("expected the rebalance to run again, got {other:?}"),
    };
    // The orders of the first run are working, so the second run wants nothing more.
    assert_eq!(report.orders_submitted, 0, "{report:?}");
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), sent);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_owed_rebalance_waits_out_the_kill_switch_and_is_tried_once_an_hour() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let close = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &close)
        .await
        .unwrap();
    owe(&f.store, close.now()).await;
    engage_kill_switch(&f.store, &close, "test").await.unwrap();

    // Market open, a rebalance owed, but the switch is engaged without `flatten_on_kill`:
    // nothing is decided and the rebalance stays owed.
    let next = date(2025, 12, 16);
    let halted = ManualClock::new(at(next, 15));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &halted,
    )
    .await
    .unwrap();
    assert!(outcome.market_open);
    assert!(outcome.rebalance.is_none(), "{outcome:?}");
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(owed_status(&f.store).await, Some(StepStatus::Started));

    // Released: the next tick makes the rebalance and settles it.
    release_kill(&f.store, &halted, "test").await.unwrap();
    let free = ManualClock::new(plus_minutes(at(next, 15), 5));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &free)
        .await
        .unwrap();
    let report = outcome.rebalance.expect("the owed rebalance runs");
    assert_eq!(report.orders_blocked, 0, "{report:?}");
    assert_eq!(owed_status(&f.store).await, Some(StepStatus::Done));
    let decided = count(&f.store, "decision").await;
    assert_eq!(decided, SYMBOLS.len() as i64);

    // Owed again within the same hour: a tick has already tried this hour, so it waits.
    owe(&f.store, free.now()).await;
    let same_hour = ManualClock::new(plus_minutes(at(next, 15), 10));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &same_hour,
    )
    .await
    .unwrap();
    assert!(outcome.rebalance.is_none(), "{outcome:?}");
    assert_eq!(count(&f.store, "decision").await, decided);

    let next_hour = ManualClock::new(at(next, 16));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &next_hour,
    )
    .await
    .unwrap();
    assert!(outcome.rebalance.is_some(), "{outcome:?}");

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_month_end_close_under_the_kill_switch_is_owed_not_lost() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    engage_kill_switch(&f.store, &clock, "test").await.unwrap();

    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            SessionCloseOutcome::Ran {
                rebalance: RebalanceStep::Halted,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(owed_status(&f.store).await, Some(StepStatus::Started));

    release_kill(&f.store, &clock, "test").await.unwrap();
    let open = ManualClock::new(at(date(2026, 1, 1), 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &open)
        .await
        .unwrap();
    let report = outcome
        .rebalance
        .expect("the month-end rebalance is made up");
    assert!(report.orders_submitted > 0, "{report:?}");
    assert_eq!(owed_status(&f.store).await, Some(StepStatus::Done));

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_step_down_is_acted_on_at_once_even_after_an_attempt_this_hour() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let close = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &close)
        .await
        .unwrap();

    // An owed rebalance is tried at 15:00; that is this hour's attempt.
    let next = date(2025, 12, 16);
    owe(&f.store, close.now()).await;
    let first = ManualClock::new(at(next, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &first)
        .await
        .unwrap();
    assert!(outcome.rebalance.is_some(), "{outcome:?}");

    // Five minutes later the equity is far under a peak (as if it had just fallen): the mode
    // steps down in this tick, and the tick does not wait for the next hour to act on it.
    risk_event::insert_many(f.store.client(), &[peak_event("200000", first.now())])
        .await
        .unwrap();
    let second = ManualClock::new(plus_minutes(at(next, 15), 5));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &second,
    )
    .await
    .unwrap();
    let (from, to) = outcome.mode_change.expect("a step down");
    assert!(to.scale() < from.scale());
    assert!(outcome.rebalance.is_some(), "{outcome:?}");

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_attempt_that_failed_this_hour_is_tried_again() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let close = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &close)
        .await
        .unwrap();
    owe(&f.store, close.now()).await;

    // An earlier tick of this hour started its attempt and failed part-way.
    let next = date(2025, 12, 16);
    f.store
        .record_step(
            &format!("tick/{next}T15"),
            "rebalance",
            StepStatus::Failed,
            &json!({"error": "broker timeout"}),
            at(next, 15),
        )
        .await
        .unwrap();

    let clock = ManualClock::new(plus_minutes(at(next, 15), 2));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    assert!(outcome.rebalance.is_some(), "{outcome:?}");
    assert_eq!(owed_status(&f.store).await, Some(StepStatus::Done));

    f.db.drop_db().await.unwrap();
}

/// The repository limits with `flatten_on_kill` on, written into `dir`.
fn flattening_limits(dir: &std::path::Path) -> std::path::PathBuf {
    let mut limits: Value =
        serde_json::from_str(&std::fs::read_to_string(support::repo_limits()).unwrap()).unwrap();
    limits["flatten_on_kill"] = json!(true);
    let path = dir.join("limits.json");
    std::fs::write(&path, limits.to_string()).unwrap();
    path
}

#[tokio::test]
async fn a_flattening_kill_is_acted_on_within_the_hour_and_then_not_repeated() {
    let mut f = fixture().await;
    let limits_dir = Scratch::new_dir();
    f.config.limits_path = flattening_limits(limits_dir.path());
    let sim = broker();
    let day = date(2025, 12, 15);
    let close = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &close)
        .await
        .unwrap();

    // An owed rebalance at 15:00 enters positions: this hour's attempt, at full scale.
    let next = date(2025, 12, 16);
    owe(&f.store, close.now()).await;
    let first = ManualClock::new(at(next, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &first)
        .await
        .unwrap();
    let entered = outcome.rebalance.expect("the owed rebalance runs");
    assert!(entered.orders_submitted > 0, "{entered:?}");
    let decided = count(&f.store, "decision").await;

    // The switch is engaged four minutes later. The limits flatten on a kill, so the scale is
    // now zero with orders working: the next tick acts although one has tried this hour. It
    // does so through the rebalance (which cancels the waiting buys and would send exits),
    // not through a blanket cancel, which on later runs would cancel the exits as well.
    engage_kill_switch(&f.store, &first, "test").await.unwrap();
    let second = ManualClock::new(plus_minutes(at(next, 15), 6));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &second,
    )
    .await
    .unwrap();
    assert!(!outcome.kill_cancelled, "{outcome:?}");
    assert!(outcome.rebalance.is_some(), "{outcome:?}");
    let after_kill = count(&f.store, "decision").await;
    assert!(after_kill > decided);

    // Same hour, same (zero) scale: whatever is still held, it is not decided again.
    let third = ManualClock::new(plus_minutes(at(next, 15), 8));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &third)
        .await
        .unwrap();
    assert!(outcome.rebalance.is_none(), "{outcome:?}");
    assert_eq!(count(&f.store, "decision").await, after_kill);

    f.db.drop_db().await.unwrap();
}

/// A market whose calendar stops at `last`, so that whether `last` ends its month is unknown.
fn market_ending_on(f: &Fixture, last: NaiveDate) -> FakeMarket {
    let sessions = f
        .calendar
        .sessions()
        .iter()
        .copied()
        .filter(|session| session.date <= last)
        .collect();
    FakeMarket::new(
        MarketCalendar::new(sessions).unwrap(),
        session_bars(&f.calendar, &SYMBOLS),
    )
}

#[tokio::test]
async fn an_unknown_month_end_fails_the_rebalance_step() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let market = market_ending_on(&f, day);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let err = session_close(&f.config, &market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Market(_)), "{err:?}");
    let (status, detail) = f
        .store
        .step_status(&run_key(day), "rebalance")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);
    assert!(detail["error"].as_str().unwrap().contains("ends its month"));
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(count(&f.store, "heartbeat").await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unknown_month_end_does_not_hold_back_an_owed_rebalance() {
    let f = fixture().await;
    let sim = broker();
    let day = date(2025, 12, 15);
    let market = market_ending_on(&f, day);
    owe(&f.store, close_at(day)).await;
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));

    let outcome = session_close(&f.config, &market, &f.store, f.sink.clone(), &sim, &clock)
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

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_deployment_that_must_be_signed_and_is_not_trades_nothing() {
    let mut f = fixture().await;
    // The config demands a signature and points at a manifest that does not exist.
    f.config.signature.required = true;
    f.config.signature.manifest = f.scratch.path().join("no-such.manifest.json");
    let sim = broker();
    let day = date(2025, 12, 31);

    let close = ManualClock::new(plus_minutes(close_at(day), 10));
    let err = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &close)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Unsigned(_)), "{err:?}");

    let open = ManualClock::new(at(date(2026, 1, 1), 15));
    let err = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &open)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Unsigned(_)), "{err:?}");

    // Nothing was fetched, decided, recorded or sent.
    assert_eq!(f.market.calls(), 0);
    assert_eq!(count(&f.store, "decision").await, 0);
    assert_eq!(count(&f.store, "engine_step").await, 0);
    assert_eq!(count(&f.store, "heartbeat").await, 0);
    assert!(sim.open_orders().await.unwrap().orders.is_empty());

    f.db.drop_db().await.unwrap();
}

/// The simulated broker, except that cancelling every open order fails.
struct CancelFails<'a>(&'a SimBroker);

impl Broker for CancelFails<'_> {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.0.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.0.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        Err(SimError("the broker refused the cancel".to_owned()))
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.0.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.0.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.0.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.0.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        self.0.account().await
    }
}

/// Sends the month-end orders of 31 December 2025 (they rest at the simulated broker, since no
/// session opens to fill them), then returns the session after that close.
async fn month_end_orders_resting(f: &Fixture, sim: &SimBroker) -> Session {
    let day = date(2025, 12, 31);
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), sim, &clock)
        .await
        .unwrap();
    assert!(!sim.open_orders().await.unwrap().orders.is_empty());
    *f.calendar.next_session(close_at(day)).unwrap()
}

#[tokio::test]
async fn a_kill_switch_engaged_elsewhere_cancels_open_orders_on_the_next_tick() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let decisions = count(&f.store, "decision").await;
    let events = count(&f.store, "order_event").await;
    assert!(!f.store.load_live_orders(None).await.unwrap().is_empty());

    // Engaged the way the phone does it: a row only, no `kill` command, so nothing is cancelled.
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();
    assert!(!sim.open_orders().await.unwrap().orders.is_empty());

    let clock = ManualClock::new(at(next.date, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(outcome.kill_cancelled, "{outcome:?}");
    assert_eq!(outcome.rebalance, None);
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    assert_eq!(count(&f.store, "decision").await, decisions);
    assert_eq!(count(&f.store, "heartbeat").await, 2);
    assert!(f.store.load_live_orders(None).await.unwrap().is_empty());
    assert!(count(&f.store, "order_event").await > events);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_second_tick_after_the_cancel_sends_nothing_more() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();
    let first = ManualClock::new(at(next.date, 15));
    tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &first)
        .await
        .unwrap();
    let events = count(&f.store, "order_event").await;

    let second = ManualClock::new(plus_minutes(at(next.date, 15), 5));
    let outcome = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        &sim,
        &second,
    )
    .await
    .unwrap();

    assert!(!outcome.kill_cancelled, "{outcome:?}");
    assert_eq!(count(&f.store, "order_event").await, events);
    assert_eq!(count(&f.store, "heartbeat").await, 3);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn no_cancel_without_the_switch() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let open = sim.open_orders().await.unwrap().orders.len();

    let clock = ManualClock::new(at(next.date, 15));
    let outcome = tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert!(!outcome.kill_cancelled, "{outcome:?}");
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), open);
    assert_eq!(count(&f.store, "heartbeat").await, 2);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn session_close_records_the_kill_cancel_step() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();

    // Two sessions after the month end: not a month end, so nothing is decided.
    let later = *f.calendar.next_session(next.close).unwrap();
    let clock = ManualClock::new(plus_minutes(later.close, 10));
    let outcome = session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            SessionCloseOutcome::Ran {
                rebalance: RebalanceStep::Halted | RebalanceStep::NotDue,
                ..
            }
        ),
        "{outcome:?}"
    );

    let (status, detail) = f
        .store
        .step_status(&run_key(later.date), "kill_cancel")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);
    assert_eq!(detail, json!({"cancelled": true}));
    assert!(sim.open_orders().await.unwrap().orders.is_empty());

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_session_close_with_nothing_to_cancel_records_no_kill_cancel_step() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();
    let clock = ManualClock::new(at(next.date, 15));
    tick(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    let later = *f.calendar.next_session(next.close).unwrap();
    let clock = ManualClock::new(plus_minutes(later.close, 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), &sim, &clock)
        .await
        .unwrap();

    assert_eq!(
        f.store
            .step_status(&run_key(later.date), "kill_cancel")
            .await
            .unwrap(),
        None
    );

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_refused_cancel_fails_the_tick_and_writes_no_heartbeat() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();
    let open = sim.open_orders().await.unwrap().orders.len();

    let clock = ManualClock::new(at(next.date, 15));
    let err = tick(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        CancelFails(&sim),
        &clock,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, AppError::Engine(EngineError::Broker(_))),
        "{err:?}"
    );
    assert_eq!(count(&f.store, "heartbeat").await, 1);
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), open);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_refused_cancel_in_session_close_records_a_failed_step() {
    let f = fixture().await;
    let sim = broker();
    let next = month_end_orders_resting(&f, &sim).await;
    let phone = ManualClock::new(at(next.date, 14));
    engage_kill_switch(&f.store, &phone, "phone").await.unwrap();

    let later = *f.calendar.next_session(next.close).unwrap();
    let clock = ManualClock::new(plus_minutes(later.close, 10));
    let err = session_close(
        &f.config,
        &f.market,
        &f.store,
        f.sink.clone(),
        CancelFails(&sim),
        &clock,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, AppError::Engine(EngineError::Broker(_))),
        "{err:?}"
    );
    let (status, detail) = f
        .store
        .step_status(&run_key(later.date), "kill_cancel")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);
    assert!(detail["error"].as_str().unwrap().contains("refused"));
    assert_eq!(count(&f.store, "heartbeat").await, 1);

    f.db.drop_db().await.unwrap();
}
