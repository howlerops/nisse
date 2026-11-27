#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

//! Acceptance tests U6: the kill switch is enforced from the store and the broker alone, at
//! any hour, with the `kill-watch` command and the first step of `tick` and `session-close`.
//! Real Postgres (`TEST_DATABASE_URL`), the simulated broker, a manual clock and a fake market.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use nisse_app::{
    AppError,
    commands::{Command, USAGE, describe_kill_watch, engage_kill_switch},
    config::EngineConfig,
    kill_watch::{
        KILL_CANCEL_STEP, KILL_RUN_KEY, KillEnforcement, WATCH_SOURCE, enforce_kill,
        flatten_on_kill, watch_once,
    },
    session::session_close,
};
use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, TimeInForce, UnixNanos, VenueOrderId,
};
use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, DecisionId, ManualClock, OpenOrders,
    OrderIntent, ReplaceRequest, RiskEvent, RiskEventKind, SubmitError,
};
use nisse_engine::{EngineError, SimBroker, SimError};
use nisse_store::{Store, StoreWriter, tables::engine_step::StepStatus, testing::ScratchDb};
use serde_json::json;
use support::{
    CancelFails, FakeMarket, Scratch, broker, close_at, flattening_limits, plus_minutes,
    session_bars, session_config, setup, weekday_calendar,
};

/// The universe of the paper config, in its order.
const SYMBOLS: [&str; 8] = ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"];

/// 1 January 2026, 14:00 UTC: an hour the market is closed, with orders resting from the
/// month-end close.
fn now() -> UnixNanos {
    support::utc_at(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 14, 0)
}

/// Everything a test needs.
struct Fixture {
    db: ScratchDb,
    store: Store,
    sink: StoreWriter,
    #[allow(dead_code, reason = "keeps the bar directory alive")]
    scratch: Scratch,
    config: EngineConfig,
    market: FakeMarket,
}

async fn fixture() -> Fixture {
    let (db, store, sink) = setup().await;
    let scratch = Scratch::new_dir();
    let config = session_config(scratch.path().to_path_buf());
    let bars = session_bars(&weekday_calendar(), &SYMBOLS);
    let market = FakeMarket::new(weekday_calendar(), bars);
    Fixture {
        db,
        store,
        sink,
        scratch,
        config,
        market,
    }
}

/// Sends the month-end orders of 31 December 2025: they rest at the simulated broker, since
/// no session opens to fill them.
async fn month_end_orders_resting(f: &Fixture, sim: &SimBroker) {
    let day = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    let clock = ManualClock::new(plus_minutes(close_at(day), 10));
    session_close(&f.config, &f.market, &f.store, f.sink.clone(), sim, &clock)
        .await
        .unwrap();
    assert!(!sim.open_orders().await.unwrap().orders.is_empty());
}

/// The kill-cancel step rows under the kill run key.
async fn kill_rows(store: &Store) -> i64 {
    store
        .client()
        .query_one(
            "SELECT count(*) FROM engine_step WHERE run_key = $1 AND step = $2",
            &[&KILL_RUN_KEY, &KILL_CANCEL_STEP],
        )
        .await
        .unwrap()
        .get(0)
}

/// A limit sell of one SPY share at a price no session opens at, so it rests.
fn resting_sell(id: &str) -> OrderIntent {
    OrderIntent {
        decision_id: DecisionId(1),
        client_order_id: ClientOrderId::new(id),
        symbol: support::symbol("SPY"),
        side: OrderSide::Sell,
        order_type: OrderType::Limit,
        qty: Quantity::from("1"),
        limit_px: Some("1000000".parse::<Price>().unwrap()),
        time_in_force: TimeInForce::Day,
        ts: UnixNanos::from(1_000_u64),
    }
}

/// The simulated broker, counting the calls to `open_orders`, with faults that can be turned
/// on: the listing fails, a cancel of everything is accepted and does nothing, or the account
/// cannot be read.
#[derive(Default)]
struct Faults {
    list_fails: bool,
    cancel_all_does_nothing: bool,
    account_fails: bool,
}

struct Counting<'a> {
    sim: &'a SimBroker,
    opens: AtomicUsize,
    faults: Faults,
}

impl<'a> Counting<'a> {
    fn with(sim: &'a SimBroker, faults: Faults) -> Self {
        Self {
            sim,
            opens: AtomicUsize::new(0),
            faults,
        }
    }
}

impl Broker for Counting<'_> {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.sim.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.sim.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        if self.faults.cancel_all_does_nothing {
            return Ok(());
        }
        self.sim.cancel_all().await
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.sim.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.sim.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        if self.faults.list_fails {
            return Err(SimError("the broker could not list its orders".to_owned()));
        }
        self.sim.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.sim.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        if self.faults.account_fails {
            return Err(SimError("the broker refused the keys".to_owned()));
        }
        self.sim.account().await
    }
}

/// The simulated broker, except that cancelling the order with venue id `fails_on` is refused.
struct RefusesOne<'a> {
    sim: &'a SimBroker,
    fails_on: VenueOrderId,
}

impl Broker for RefusesOne<'_> {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.sim.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        if venue_order_id == self.fails_on {
            return Err(SimError("the broker refused this cancel".to_owned()));
        }
        self.sim.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        self.sim.cancel_all().await
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.sim.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.sim.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.sim.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.sim.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        self.sim.account().await
    }
}

#[tokio::test]
async fn switch_off_does_not_ask_the_broker() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let open = sim.open_orders().await.unwrap().orders.len();
    let counting = Counting::with(&sim, Faults::default());

    let outcome = enforce_kill(&f.store, &counting, false, now())
        .await
        .unwrap();

    assert_eq!(
        outcome,
        KillEnforcement {
            engaged: false,
            open_orders: 0,
            cancelled: false,
            failed: false,
        }
    );
    assert_eq!(counting.opens.load(Ordering::SeqCst), 0);
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), open);
    assert_eq!(kill_rows(&f.store).await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn engaged_with_halting_limits_cancels_everything_and_records_it() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let open = sim.open_orders().await.unwrap().orders.len();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let outcome = enforce_kill(&f.store, &sim, false, now()).await.unwrap();

    assert_eq!(
        outcome,
        KillEnforcement {
            engaged: true,
            open_orders: open,
            cancelled: true,
            failed: false,
        }
    );
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    assert_eq!(kill_rows(&f.store).await, 1);
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);
    assert_eq!(
        detail,
        json!({"open_orders": open, "flatten": false, "remaining": 0})
    );

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn engaged_with_nothing_open_cancels_nothing_and_records_nothing() {
    let f = fixture().await;
    let sim = broker();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let outcome = enforce_kill(&f.store, &sim, false, now()).await.unwrap();

    assert_eq!(
        outcome,
        KillEnforcement {
            engaged: true,
            open_orders: 0,
            cancelled: false,
            failed: false,
        }
    );
    assert_eq!(kill_rows(&f.store).await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn flatten_mode_cancels_the_buys_and_leaves_the_sells() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let buys = sim.open_orders().await.unwrap().orders.len();
    // A resting sell beside the buys: the exit that flatten mode must keep.
    sim.submit(&resting_sell("exit-1")).await.unwrap();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let outcome = enforce_kill(&f.store, &sim, true, now()).await.unwrap();

    assert_eq!(
        outcome,
        KillEnforcement {
            engaged: true,
            open_orders: buys + 1,
            cancelled: true,
            failed: false,
        }
    );
    let left = sim.open_orders().await.unwrap().orders;
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].side, OrderSide::Sell);
    let (_, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        detail,
        json!({"open_orders": buys + 1, "flatten": true, "remaining": 0})
    );

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn flatten_mode_with_only_sells_open_cancels_nothing() {
    let f = fixture().await;
    let sim = broker();
    sim.submit(&resting_sell("exit-1")).await.unwrap();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let outcome = enforce_kill(&f.store, &sim, true, now()).await.unwrap();

    assert_eq!(
        outcome,
        KillEnforcement {
            engaged: true,
            open_orders: 1,
            cancelled: false,
            failed: false,
        }
    );
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), 1);
    assert_eq!(kill_rows(&f.store).await, 0);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn flatten_mode_tries_every_buy_when_one_cancel_is_refused() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let orders = sim.open_orders().await.unwrap().orders;
    let buys = orders.len();
    let refused = orders[0].venue_order_id;
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let outcome = enforce_kill(
        &f.store,
        &RefusesOne {
            sim: &sim,
            fails_on: refused,
        },
        true,
        now(),
    )
    .await
    .unwrap();

    // A switch that flattens goes on (the rebalance that follows has the exits to send); the
    // buy that could not be cancelled is on record.
    assert!(outcome.cancelled, "{outcome:?}");
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);
    assert!(detail["error"].is_string(), "{detail}");
    // Every other buy was cancelled: only the refused one is left.
    let left = sim.open_orders().await.unwrap().orders;
    assert_eq!(left.len(), 1, "of {buys}: {left:?}");
    assert_eq!(left[0].venue_order_id, refused);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unreadable_switch_counts_as_engaged() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    // A kill_switch row without a boolean "engaged": the store cannot say what it means.
    f.store
        .record_risk_event(&RiskEvent {
            kind: RiskEventKind::KillSwitch,
            detail: json!({ "engaged": "maybe" }),
            ts: now(),
        })
        .await
        .unwrap();

    let outcome = enforce_kill(&f.store, &sim, false, now()).await.unwrap();

    assert!(outcome.engaged, "{outcome:?}");
    assert!(outcome.cancelled, "{outcome:?}");
    assert!(sim.open_orders().await.unwrap().orders.is_empty());

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_refused_cancel_is_an_error_and_records_a_failed_step() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let open = sim.open_orders().await.unwrap().orders.len();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    let err = enforce_kill(&f.store, &CancelFails(&sim), false, now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, AppError::Engine(EngineError::Broker(_))),
        "{err:?}"
    );
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);
    assert!(detail["error"].as_str().unwrap().contains("refused"));
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), open);

    f.db.drop_db().await.unwrap();
}

#[test]
fn flatten_on_kill_reads_the_limits_file() {
    let dir = Scratch::new_dir();
    // The repository limits halt on a kill.
    assert!(!flatten_on_kill(&support::repo_limits()));
    // The same limits with `flatten_on_kill` on.
    assert!(flatten_on_kill(&flattening_limits(dir.path())));
    // A missing file counts as "no": the safe reading.
    assert!(!flatten_on_kill(&dir.path().join("no-such-limits.json")));
}

#[test]
fn kill_watch_parses_and_refuses_by() {
    let parse = |words: &[&str]| Command::parse(words.iter().map(|w| (*w).to_owned()));
    assert_eq!(
        parse(&["kill-watch", "c.json"]),
        Ok(Command::KillWatch {
            config: "c.json".into()
        })
    );
    assert!(parse(&["kill-watch", "c.json", "--by", "ana"]).is_err());
    assert!(parse(&["--by", "ana", "kill-watch", "c.json"]).is_err());
    assert!(parse(&["kill-watch"]).is_err());
    assert!(USAGE.contains("kill-watch"), "{USAGE}");
    assert_eq!(
        parse(&["kill-watch", "c.json"]).unwrap().config_path(),
        std::path::Path::new("c.json")
    );
}

#[test]
fn describe_kill_watch_lines() {
    let line = |engaged, open_orders, cancelled| {
        describe_kill_watch(&KillEnforcement {
            engaged,
            open_orders,
            cancelled,
            failed: false,
        })
    };
    assert_eq!(line(false, 0, false), "kill-watch: switch off");
    assert_eq!(
        line(true, 0, false),
        "kill-watch: switch ENGAGED; nothing open"
    );
    assert_eq!(
        line(true, 3, true),
        "kill-watch: switch ENGAGED; 3 open orders cancelled"
    );
    assert_eq!(
        line(true, 2, false),
        "kill-watch: switch ENGAGED; 2 open orders, none cancelled (exits left)"
    );
}

/// The number of heartbeats the watcher has written.
async fn watch_beats(store: &Store) -> i64 {
    store
        .client()
        .query_one(
            "SELECT count(*) FROM heartbeat WHERE source = $1",
            &[&WATCH_SOURCE],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn a_halting_switch_cancels_blind_when_the_broker_cannot_list_orders() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();
    let no_list = Counting::with(
        &sim,
        Faults {
            list_fails: true,
            ..Faults::default()
        },
    );

    let outcome = enforce_kill(&f.store, &no_list, false, now())
        .await
        .unwrap();

    assert!(outcome.engaged && outcome.cancelled, "{outcome:?}");
    assert!(sim.open_orders().await.unwrap().orders.is_empty());
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    // Sent, but nothing could confirm it: "started", not "done".
    assert_eq!(status, StepStatus::Started);
    assert!(detail["listing_failed"].is_string(), "{detail}");

    // The next run can list again, finds nothing open, and that confirms the cancel.
    let outcome = enforce_kill(&f.store, &sim, false, now()).await.unwrap();
    assert!(!outcome.cancelled, "{outcome:?}");
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Done);
    assert_eq!(detail["confirmed_empty"], json!(true));

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_flattening_switch_that_cannot_list_orders_fails_and_says_so() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let resting = sim.open_orders().await.unwrap().orders.len();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();
    let no_list = Counting::with(
        &sim,
        Faults {
            list_fails: true,
            ..Faults::default()
        },
    );

    let err = enforce_kill(&f.store, &no_list, true, now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, AppError::Engine(EngineError::Broker(_))),
        "{err:?}"
    );
    // Nothing was cancelled blind: the exits must not be cancelled.
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), resting);
    let (status, _) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_watcher_leaves_a_heartbeat_when_it_did_its_job() {
    let f = fixture().await;
    let sim = broker();

    // Switch off and a broker that answers: nothing to do, and a heartbeat says the watcher
    // could have cancelled.
    watch_once(&f.store, Ok(&sim), false, now()).await.unwrap();
    assert_eq!(watch_beats(&f.store).await, 1);

    // Switch off but no broker client, or a broker that refuses the keys: the watcher could
    // not cancel if it had to, so it fails and leaves no heartbeat.
    let err = watch_once(
        &f.store,
        Err::<&SimBroker, _>("no keys".to_owned()),
        false,
        now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::KillNotEnforced(_)), "{err:?}");
    let locked_out = Counting::with(
        &sim,
        Faults {
            account_fails: true,
            ..Faults::default()
        },
    );
    watch_once(&f.store, Ok(&locked_out), false, now())
        .await
        .unwrap_err();
    assert_eq!(watch_beats(&f.store).await, 1);
    // With the switch off nothing was recorded as a failed kill cancel.
    assert_eq!(kill_rows(&f.store).await, 0);

    // Engaged with orders resting: cancelled, and a heartbeat.
    month_end_orders_resting(&f, &sim).await;
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();
    let outcome = watch_once(&f.store, Ok(&sim), false, now()).await.unwrap();
    assert!(outcome.cancelled, "{outcome:?}");
    assert_eq!(watch_beats(&f.store).await, 2);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_watcher_that_cannot_enforce_an_engaged_switch_leaves_no_heartbeat() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();

    // No broker client (missing keys).
    let err = watch_once(
        &f.store,
        Err::<&SimBroker, _>("no keys".to_owned()),
        false,
        now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::KillNotEnforced(_)), "{err:?}");
    // The cancel is refused.
    watch_once(&f.store, Ok(&CancelFails(&sim)), false, now())
        .await
        .unwrap_err();

    assert_eq!(watch_beats(&f.store).await, 0);
    let (status, _) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Failed);

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_cancel_the_broker_accepted_but_did_not_carry_out_is_not_called_done() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let resting = sim.open_orders().await.unwrap().orders.len();
    engage_kill_switch(&f.store, &ManualClock::new(now()), "phone")
        .await
        .unwrap();
    // The broker answers "accepted" and cancels nothing (what a per-order failure inside a
    // multi-status answer looks like from outside).
    let sticky = Counting::with(
        &sim,
        Faults {
            cancel_all_does_nothing: true,
            ..Faults::default()
        },
    );

    let outcome = enforce_kill(&f.store, &sticky, false, now()).await.unwrap();

    assert!(outcome.cancelled, "{outcome:?}");
    let (status, detail) = f
        .store
        .step_status(KILL_RUN_KEY, KILL_CANCEL_STEP)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Started);
    assert_eq!(detail["remaining"], json!(resting));

    f.db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_lost_store_connection_is_an_error_and_cancels_nothing() {
    let f = fixture().await;
    let sim = broker();
    month_end_orders_resting(&f, &sim).await;
    let resting = sim.open_orders().await.unwrap().orders.len();

    // The switch is OFF. The store's connection is then killed from another one, as a
    // database restart would: the read fails, which says nothing about the switch.
    let pid: i32 = f
        .store
        .client()
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let other = f.db.client().await.unwrap();
    other
        .execute("SELECT pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();

    let err = enforce_kill(&f.store, &sim, false, now())
        .await
        .unwrap_err();

    assert!(matches!(err, AppError::Store(_)), "{err:?}");
    assert_eq!(sim.open_orders().await.unwrap().orders.len(), resting);

    drop(other);
    f.db.drop_db().await.unwrap();
}
