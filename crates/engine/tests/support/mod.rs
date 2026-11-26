#![allow(
    dead_code,
    reason = "shared by several integration tests; each uses a subset"
)]
#![allow(
    clippy::expect_used,
    reason = "builders parse literals; a malformed literal is a bug in the test"
)]

//! Test doubles for the order path, shared by the engine's integration tests.
//!
//! - [`FakeBroker`] implements [`Broker`]. Scripted results are queued per method (`submit`,
//!   `replace`); reads come from scripted state. Every call is pushed to a [`CallLog`].
//! - [`MemoryIntentLog`] implements [`IntentLog`], stores intents in memory and can be set to
//!   fail. It shares the same [`CallLog`].
//! - Builders make `OrderIntent` and `BrokerOrder` values from strings, never `f64`.
//!
//! Both doubles are `Clone` and share their state, so a test can keep a handle, move a clone
//! into the code under test, and script or inspect the original afterwards.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, DecisionId, IntentLog, OpenOrders,
    OrderIntent, OrderState, RecordSink, StoreRecord, SubmitError,
    nautilus::{
        ClientOrderId, Money, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce,
        UnixNanos, VenueOrderId,
    },
};

/// Every broker and intent-log call, in the order it was made.
pub type CallLog = Arc<Mutex<Vec<Call>>>;

/// One call made to a test double.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// An intent-log write attempt, successful or not.
    RecordIntent(ClientOrderId),
    Submit(ClientOrderId),
    /// `Replace(venue order id of the order being replaced, new client order id)`.
    Replace(VenueOrderId, ClientOrderId),
    Cancel(VenueOrderId),
    CancelAll,
    OrderByClientId(ClientOrderId),
    OpenOrders,
    Positions,
    Account,
}

/// The error the test doubles return.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FakeError(pub String);

/// A new, empty call log.
#[must_use]
pub fn new_call_log() -> CallLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// A copy of every call recorded so far.
#[must_use]
pub fn calls(log: &CallLog) -> Vec<Call> {
    lock(log).clone()
}

/// Locks a mutex, ignoring poisoning: a test double must keep answering after another test
/// thread panicked while holding its state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn push(log: &CallLog, call: Call) {
    lock(log).push(call);
}

#[derive(Debug, Default)]
struct BrokerState {
    submit_results: VecDeque<Result<BrokerOrder, SubmitError<FakeError>>>,
    replace_results: VecDeque<Result<BrokerOrder, SubmitError<FakeError>>>,
    open_orders: Vec<BrokerOrder>,
    positions: Vec<BrokerPosition>,
    orders_by_client_id: BTreeMap<String, BrokerOrder>,
    fail_reads: Option<String>,
    fail_lookups: Option<String>,
    unrepresentable: u32,
}

/// A scripted [`Broker`].
///
/// - `submit`: pops the next scripted result; when none is queued it accepts the order with
///   [`accepted_from`].
/// - `replace`: pops the next scripted result; when none is queued it returns
///   `SubmitError::Rejected`, because there is no intent to build an acceptance from. Tests
///   that expect a replace to succeed must script it.
/// - `order_by_client_id`, `open_orders`, `positions`, `account`: answer from the scripted
///   state. `fail_reads` makes these four return `Err(FakeError)`.
/// - `cancel`, `cancel_all`: succeed and do nothing.
///
/// Every method records its call in the shared [`CallLog`] before it answers.
#[derive(Debug, Clone)]
pub struct FakeBroker {
    log: CallLog,
    state: Arc<Mutex<BrokerState>>,
}

impl FakeBroker {
    #[must_use]
    pub fn new(log: CallLog) -> Self {
        Self {
            log,
            state: Arc::new(Mutex::new(BrokerState::default())),
        }
    }

    /// Queues the answer to the next `submit` call.
    pub fn push_submit_result(&self, result: Result<BrokerOrder, SubmitError<FakeError>>) {
        lock(&self.state).submit_results.push_back(result);
    }

    /// Queues the answer to the next `replace` call.
    pub fn push_replace_result(&self, result: Result<BrokerOrder, SubmitError<FakeError>>) {
        lock(&self.state).replace_results.push_back(result);
    }

    /// Replaces the orders returned by `open_orders`.
    pub fn set_open_orders(&self, orders: Vec<BrokerOrder>) {
        lock(&self.state).open_orders = orders;
    }

    /// Replaces the positions returned by `positions`.
    pub fn set_positions(&self, positions: Vec<BrokerPosition>) {
        lock(&self.state).positions = positions;
    }

    /// Makes only `order_by_client_id` fail, leaving the other reads working.
    pub fn fail_lookups(&self, msg: &str) {
        lock(&self.state).fail_lookups = Some(msg.to_owned());
    }

    /// Sets how many open orders `open_orders` reports as not representable.
    pub fn set_unrepresentable(&self, count: u32) {
        lock(&self.state).unrepresentable = count;
    }

    /// Makes `order_by_client_id` find this order (keyed by its client order id).
    pub fn insert_order(&self, order: BrokerOrder) {
        lock(&self.state)
            .orders_by_client_id
            .insert(order.client_order_id.as_str().to_owned(), order);
    }

    /// Makes the read methods fail with `FakeError(message)` until the state is changed.
    pub fn fail_reads(&self, message: impl Into<String>) {
        lock(&self.state).fail_reads = Some(message.into());
    }

    /// The error to return from a read, if reads are set to fail.
    fn read_failure(&self) -> Option<FakeError> {
        lock(&self.state).fail_reads.clone().map(FakeError)
    }
}

impl Broker for FakeBroker {
    type Error = FakeError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<FakeError>> {
        push(&self.log, Call::Submit(intent.client_order_id));
        let scripted = lock(&self.state).submit_results.pop_front();
        scripted.unwrap_or_else(|| Ok(accepted_from(intent)))
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), FakeError> {
        push(&self.log, Call::Cancel(venue_order_id));
        Ok(())
    }

    async fn cancel_all(&self) -> Result<(), FakeError> {
        push(&self.log, Call::CancelAll);
        Ok(())
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &nisse_core::ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<FakeError>> {
        push(
            &self.log,
            Call::Replace(venue_order_id, request.new_client_order_id),
        );
        let scripted = lock(&self.state).replace_results.pop_front();
        scripted.unwrap_or_else(|| {
            Err(SubmitError::Rejected(FakeError(
                "no scripted replace result".to_owned(),
            )))
        })
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, FakeError> {
        push(&self.log, Call::OrderByClientId(client_order_id));
        if let Some(err) = self.read_failure() {
            return Err(err);
        }
        if let Some(msg) = lock(&self.state).fail_lookups.clone() {
            return Err(FakeError(msg));
        }
        Ok(lock(&self.state)
            .orders_by_client_id
            .get(client_order_id.as_str())
            .copied())
    }

    async fn open_orders(&self) -> Result<OpenOrders, FakeError> {
        push(&self.log, Call::OpenOrders);
        if let Some(err) = self.read_failure() {
            return Err(err);
        }
        let state = lock(&self.state);
        Ok(OpenOrders {
            orders: state.open_orders.clone(),
            unrepresentable: state.unrepresentable,
        })
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, FakeError> {
        push(&self.log, Call::Positions);
        if let Some(err) = self.read_failure() {
            return Err(err);
        }
        Ok(lock(&self.state).positions.clone())
    }

    async fn account(&self) -> Result<AccountSnapshot, FakeError> {
        push(&self.log, Call::Account);
        if let Some(err) = self.read_failure() {
            return Err(err);
        }
        Ok(AccountSnapshot {
            equity: money("100000.00 USD"),
            cash: money("100000.00 USD"),
            buying_power: money("100000.00 USD"),
            trading_blocked: false,
        })
    }
}

#[derive(Debug, Default)]
struct IntentLogState {
    intents: Vec<OrderIntent>,
    fail: Option<String>,
}

/// An in-memory [`IntentLog`].
///
/// Every `record_intent` call is pushed to the shared [`CallLog`], including failed ones. A
/// successful write stores the intent; while failing fail (see [`fail_with`](Self::fail_with))
/// nothing is stored.
#[derive(Debug, Clone)]
pub struct MemoryIntentLog {
    log: CallLog,
    state: Arc<Mutex<IntentLogState>>,
}

impl MemoryIntentLog {
    #[must_use]
    pub fn new(log: CallLog) -> Self {
        Self {
            log,
            state: Arc::new(Mutex::new(IntentLogState::default())),
        }
    }

    /// Makes every later `record_intent` fail with `FakeError(message)`, without storing.
    pub fn fail_with(&self, message: impl Into<String>) {
        lock(&self.state).fail = Some(message.into());
    }

    /// The intents stored so far, in write order.
    #[must_use]
    pub fn intents(&self) -> Vec<OrderIntent> {
        lock(&self.state).intents.clone()
    }
}

impl IntentLog for MemoryIntentLog {
    type Error = FakeError;

    async fn record_intent(&self, intent: &OrderIntent) -> Result<(), FakeError> {
        push(&self.log, Call::RecordIntent(intent.client_order_id));
        let mut state = lock(&self.state);
        if let Some(message) = &state.fail {
            return Err(FakeError(message.clone()));
        }
        state.intents.push(*intent);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct SinkState {
    records: Vec<StoreRecord>,
    fail: Option<String>,
}

/// An in-memory [`RecordSink`] that keeps every record in the order it was emitted.
///
/// While failing (see [`fail_with`](Self::fail_with)) `emit` returns `FakeError` and records
/// nothing. Clones share the same records.
#[derive(Clone, Default)]
pub struct MemorySink {
    state: Arc<Mutex<SinkState>>,
}

impl MemorySink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The records emitted so far, in emit order.
    #[must_use]
    pub fn records(&self) -> Vec<StoreRecord> {
        lock(&self.state).records.clone()
    }

    /// Makes every later `emit` fail with `FakeError(msg)`, without recording.
    pub fn fail_with(&self, msg: &str) {
        lock(&self.state).fail = Some(msg.to_owned());
    }
}

impl RecordSink for MemorySink {
    type Error = FakeError;

    async fn emit(&self, record: StoreRecord) -> Result<(), FakeError> {
        let mut state = lock(&self.state);
        if let Some(message) = &state.fail {
            return Err(FakeError(message.clone()));
        }
        state.records.push(record);
        Ok(())
    }

    async fn flush(&self) -> Result<(), FakeError> {
        match &lock(&self.state).fail {
            Some(message) => Err(FakeError(message.clone())),
            None => Ok(()),
        }
    }
}

/// A limit (when `limit_px` is `Some`) or market day order for SPY, buy side, decision 1,
/// formed at `ts` 1_000. `qty` and `limit_px` are decimal strings, e.g. `("10", Some("500.10"))`.
#[must_use]
pub fn intent(id: &str, qty: &str, limit_px: Option<&str>) -> OrderIntent {
    OrderIntent {
        decision_id: DecisionId(1),
        client_order_id: ClientOrderId::new(id),
        symbol: Symbol::new("SPY"),
        side: OrderSide::Buy,
        order_type: if limit_px.is_some() {
            OrderType::Limit
        } else {
            OrderType::Market
        },
        qty: Quantity::from(qty),
        limit_px: limit_px.map(price),
        time_in_force: TimeInForce::Day,
        ts: UnixNanos::from(1_000_u64),
    }
}

/// The broker's `Accepted` view of `intent`, with nothing filled.
#[must_use]
pub fn accepted_from(intent: &OrderIntent) -> BrokerOrder {
    snapshot(intent, OrderState::Accepted, "0", None)
}

/// The broker's view of `intent` in `state`, with `cum` filled at average price `avg`.
/// The venue order id is `venue-<client order id>` and the broker timestamp is one nanosecond
/// after the intent's.
#[must_use]
pub fn snapshot(
    intent: &OrderIntent,
    state: OrderState,
    cum: &str,
    avg: Option<&str>,
) -> BrokerOrder {
    BrokerOrder {
        client_order_id: intent.client_order_id,
        venue_order_id: VenueOrderId::new(format!("venue-{}", intent.client_order_id)),
        symbol: intent.symbol,
        side: intent.side,
        order_type: intent.order_type,
        qty: intent.qty,
        limit_px: intent.limit_px,
        time_in_force: intent.time_in_force,
        state,
        cum_filled_qty: Quantity::from(cum),
        avg_fill_px: avg.map(price),
        ts: UnixNanos::from(intent.ts.as_u64() + 1),
    }
}

/// Parses a decimal price string. Panics on malformed test input.
fn price(s: &str) -> Price {
    s.parse::<Price>().expect("valid test price")
}

/// Parses a money string such as `"100000.00 USD"`. Panics on malformed test input.
fn money(s: &str) -> Money {
    s.parse::<Money>().expect("valid test money")
}
