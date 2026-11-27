#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests P6b: the `tick` and `session-close` commands against a real Postgres
//! (`TEST_DATABASE_URL`), with the simulated broker, a manual clock and a fake market.

mod support;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use nisse_app::{
    AppError,
    config::EngineConfig,
    ingest::bar_file_path,
    restore::take_engine_lock,
    session::{
        RESIZE_RUN_KEY, RESIZE_STEP, RebalanceStep, SessionCloseOutcome, session_close, tick,
    },
};
use nisse_core::nautilus::{Bar, UnixNanos};
use nisse_core::{Broker, ManualClock, MarketCalendar, RiskEvent, RiskEventKind};
use nisse_risk::DrawdownMode;
use nisse_store::{
    Store,
    tables::{engine_step::StepStatus, risk_event},
    testing::ScratchDb,
};
use serde_json::{Value, json};
use support::{
    FakeMarket, Scratch, broker, close_at, plus_minutes, repo_limits, session_bars, setup,
    weekday_calendar,
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

/// The paper config, with the strategy's windows shortened so that the ~40 sessions of history
/// in the fake market are enough to decide: `lookback_days` 20 (above `skip_days` 5),
/// `vol_window` 10. The paper universe, benchmark and limits are kept.
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
