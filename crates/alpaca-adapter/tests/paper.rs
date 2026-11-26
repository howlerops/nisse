//! Owner-run tests against Alpaca's PAPER environment.
//!
//! These need the paper API key pair in the environment of the engine host and are skipped
//! by default. Run them with:
//!
//! `cargo test -p nisse-alpaca-adapter --test paper -- --ignored --test-threads=1 --nocapture`
//!
//! Safety rules, enforced in the helpers below:
//! - credentials come only from `Credentials::from_env()`;
//! - the config is only `AlpacaConfig::paper(Feed::Iex)`, and `paper_config()` asserts the
//!   paper endpoint, so a later edit cannot point these tests at a live account;
//! - every order is a day limit BUY at 1.00 (far below the market) and is cancelled before the
//!   test ends, including when a check fails.
//!
//! See `docs/runbooks/paper-tests.md`.

#![allow(
    clippy::panic,
    reason = "the fractional probe panics on an unknown outcome"
)]

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use nisse_alpaca_adapter::{
    AdapterError, AlpacaConfig, AlpacaHttp, AlpacaTrading, Credentials, Feed, fetch_calendar,
};
use nisse_core::nautilus::{ClientOrderId, OrderSide, OrderType, Price, Quantity, TimeInForce};
use nisse_core::{
    Broker, Clock, DecisionId, IntentLog, OrderIntent, OrderState, SubmitError, SystemClock,
};
use nisse_engine::{
    EngineError, OrderGateway, OrderTracker, Submitted, SyncOptions, SyncReport, sync_with_broker,
};

/// Boxed error for test bodies, so setup, checks and cleanup can all use `?`.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How many times `sync_until_canceled` polls the broker, and the wait between polls.
const POLL_ATTEMPTS: u32 = 10;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Intent log for the gateway. Holds the intents in memory so the test can read them back
/// after the gateway has been handed a reference to it.
#[derive(Default)]
struct MemoryIntentLog(Mutex<Vec<OrderIntent>>);

impl IntentLog for &MemoryIntentLog {
    type Error = std::convert::Infallible;

    async fn record_intent(&self, intent: &OrderIntent) -> Result<(), Self::Error> {
        // The guard is dropped at the end of this statement; no await point is involved.
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*intent);
        Ok(())
    }
}

fn recorded(log: &MemoryIntentLog) -> Vec<OrderIntent> {
    let guard = log.0.lock().unwrap_or_else(PoisonError::into_inner);
    guard.clone()
}

/// Fails the test with `message` unless `condition` holds.
fn ensure(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// The paper environment. The assertion is the guard against a live endpoint.
fn paper_config() -> AlpacaConfig {
    let config = AlpacaConfig::paper(Feed::Iex);
    assert!(
        config.trading_base_url.contains("paper-api"),
        "paper tests must target the paper trading endpoint"
    );
    assert!(
        config.stream_url.contains("paper-api"),
        "paper tests must target the paper stream endpoint"
    );
    config
}

/// Reads the credentials from the environment. Fails with `MissingEnv` (naming the variable)
/// before any request is made.
fn creds() -> Result<Credentials, AdapterError> {
    Credentials::from_env()
}

fn http() -> Result<AlpacaHttp, AdapterError> {
    AlpacaHttp::new(&creds()?, Duration::from_secs(10))
}

/// A day limit buy of `qty` SPY at 1.00, which cannot fill. The client order id is unique per
/// run.
fn far_limit_intent(test_name: &str, qty: &str) -> OrderIntent {
    OrderIntent {
        decision_id: DecisionId(1),
        client_order_id: ClientOrderId::new(format!(
            "nisse-paper-{}-{}",
            test_name,
            SystemClock.now().as_u64()
        )),
        symbol: nisse_core::nautilus::Symbol::new("SPY"),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        qty: Quantity::from(qty),
        limit_px: Some(Price::from("1.00")),
        time_in_force: TimeInForce::Day,
        ts: SystemClock.now(),
    }
}

/// One reconciliation pass against the paper account, with a fresh (empty) position map.
async fn sync_once(
    trading: &AlpacaTrading,
    tracker: &mut OrderTracker,
) -> Result<SyncReport, EngineError> {
    let mut positions = BTreeMap::new();
    sync_with_broker(
        trading,
        tracker,
        &mut positions,
        SystemClock.now(),
        SyncOptions::default(),
    )
    .await
}

/// Syncs until the tracker shows `id` as `Canceled`, at most `POLL_ATTEMPTS` times.
async fn sync_until_canceled(
    trading: &AlpacaTrading,
    tracker: &mut OrderTracker,
    id: ClientOrderId,
) -> TestResult {
    for attempt in 0..POLL_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        sync_once(trading, tracker).await?;
        if tracker
            .get(id)
            .is_some_and(|tracked| tracked.state == OrderState::Canceled)
        {
            return Ok(());
        }
    }
    Err(format!("{id} did not reach Canceled after {POLL_ATTEMPTS} syncs").into())
}

/// Checks made while the test order is open at the broker.
async fn round_trip_checks(
    trading: &AlpacaTrading,
    log: &MemoryIntentLog,
    intent: &OrderIntent,
    submitted: &Submitted,
) -> TestResult {
    let id = intent.client_order_id;
    ensure(
        recorded(log) == [*intent],
        "intent log does not hold exactly the submitted intent",
    )?;
    ensure(
        submitted.order.client_order_id == id,
        "returned order has a different client order id",
    )?;
    ensure(
        matches!(
            submitted.order.state,
            OrderState::Submitted | OrderState::Accepted
        ),
        "order is neither Submitted nor Accepted right after submit",
    )?;
    let found = trading
        .order_by_client_id(id)
        .await?
        .ok_or("order not found by client order id")?;
    ensure(
        found.venue_order_id == submitted.order.venue_order_id,
        "order found by client order id has a different venue order id",
    )
}

#[tokio::test]
#[ignore = "needs Alpaca paper credentials on the engine host"]
async fn order_round_trip() -> TestResult {
    let config = paper_config();
    let trading = AlpacaTrading::new(http()?, &config);
    let log = MemoryIntentLog::default();
    let gateway = OrderGateway::new(trading.clone(), &log);
    let mut tracker = OrderTracker::new();

    let intent = far_limit_intent("order_round_trip", "1");
    let id = intent.client_order_id;
    let submitted = gateway.submit(&mut tracker, &intent).await?;
    let venue_order_id = submitted.order.venue_order_id;

    let checks = round_trip_checks(&trading, &log, &intent, &submitted).await;
    let cancel = trading.cancel(venue_order_id).await;
    let settled = sync_until_canceled(&trading, &mut tracker, id).await;

    checks?;
    cancel?;
    settled
}

#[tokio::test]
#[ignore = "needs Alpaca paper credentials on the engine host"]
async fn restart_mid_order_reconciles() -> TestResult {
    let config = paper_config();
    let trading = AlpacaTrading::new(http()?, &config);
    let log = MemoryIntentLog::default();
    let gateway = OrderGateway::new(trading.clone(), &log);
    let mut tracker = OrderTracker::new();

    let intent = far_limit_intent("restart_mid_order_reconciles", "1");
    let id = intent.client_order_id;
    let submitted = gateway.submit(&mut tracker, &intent).await?;
    let venue_order_id = submitted.order.venue_order_id;

    // Simulate a restart: the old tracker is gone, and the only memory left is the intent log.
    drop(tracker);
    let mut restarted = OrderTracker::new();
    let logged = recorded(&log)
        .first()
        .copied()
        .ok_or("intent log is empty after submit")?;
    restarted.track_intent(logged)?;

    let checks = async {
        let report = sync_once(&trading, &mut restarted).await?;
        let tracked = restarted
            .get(id)
            .ok_or("restarted tracker lost the order")?;
        ensure(
            tracked.venue_order_id == Some(venue_order_id),
            "restarted tracker did not adopt the broker's venue order id",
        )?;
        ensure(
            !tracked.state.is_terminal(),
            "restarted tracker shows a terminal state for an open order",
        )?;
        ensure(
            !report.findings.is_empty(),
            "reconciliation after restart reported no findings",
        )?;
        TestResult::Ok(())
    }
    .await;

    let cancel = trading.cancel(venue_order_id).await;
    let settled = sync_until_canceled(&trading, &mut restarted, id).await;

    checks?;
    cancel?;
    settled
}

#[tokio::test]
#[ignore = "needs Alpaca paper credentials on the engine host"]
async fn fractional_limit_order_probe() -> TestResult {
    let trading = AlpacaTrading::new(http()?, &paper_config());
    let intent = far_limit_intent("fractional_limit_order_probe", "0.5");

    match trading.submit(&intent).await {
        Ok(order) => {
            println!("FRACTIONAL LIMIT: accepted, state {:?}", order.state);
            trading.cancel(order.venue_order_id).await?;
        }
        Err(SubmitError::Rejected(e)) => println!("FRACTIONAL LIMIT: rejected: {e}"),
        Err(SubmitError::Unknown(e)) => {
            panic!("FRACTIONAL LIMIT: probe failed, outcome unknown: {e}")
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "needs Alpaca paper credentials on the engine host"]
async fn calendar_has_a_half_day() -> TestResult {
    let config = paper_config();
    let http = http()?;
    let year =
        chrono::DateTime::from_timestamp_nanos(i64::try_from(SystemClock.now().as_u64())?).year();
    let start = NaiveDate::from_ymd_opt(year, 1, 1).ok_or("invalid start date")?;
    let end = NaiveDate::from_ymd_opt(year, 12, 31).ok_or("invalid end date")?;

    let calendar = fetch_calendar(&http, &config, start, end).await?;
    ensure(
        calendar.sessions().len() > 240,
        "calendar for the current year has 240 or fewer sessions",
    )?;
    ensure(
        calendar
            .sessions()
            .iter()
            .any(|session| session.is_half_day()),
        "calendar for the current year has no half-day session",
    )
}
