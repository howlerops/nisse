//! Regression tests for the risk handling fixed after the first phase-4 review: the sell
//! sizing against unfilled buys, the kill switch's durability and latch, the flush before an
//! order, drawdown flushes, positions outside the universe, stale and future bars, duplicate
//! fills, the day-trade count after a restart, the limits record, `needs_flatten`, and the
//! backtest loop while flat. Every assertion is exact; a failing scenario is marked
//! `#[ignore = "finding: ..."]` rather than weakened.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

mod support;

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nisse_core::{
    AccountSnapshot, Action, Broker, BrokerOrder, BrokerPosition, Clock, Decision, DecisionId,
    Fill, FillDetail, KillSwitchSource, ManualClock, MarketCalendar, OpenOrders, OrderIntent,
    OrderState, OrderUpdate, RecordSink, ReplaceRequest, RiskCheck, RiskEvent, RiskEventKind,
    StoreRecord, StrategyVersionId, SubmitError,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId,
        InstrumentId, Money, OrderSide, OrderType, PositionSide, Price, PriceType, Quantity,
        Symbol, TimeInForce, TradeId, UnixNanos, Venue, VenueOrderId,
    },
};
use nisse_engine::strategies::{TrendParams, TrendVolTarget, feature};
use nisse_engine::{
    BacktestConfig, CostModel, EngineError, MemoryStore, OrderBookState, OrderGateway, SimBroker,
    SimError, SyncOptions, Trader, TraderConfig, run_backtest,
};
use nisse_risk::{DrawdownMode, Limits, RiskState, rule};
use rust_decimal::Decimal;
use serde_json::Value;
use support::snapshot;

const AAA: &str = "AAA";
const BBB: &str = "BBB";
const XYZ: &str = "XYZ";
const DAY_NANOS: u64 = 86_400 * 1_000_000_000;
const HOUR_NANOS: u64 = 3_600 * 1_000_000_000;

/// Closes that rise and vary, so momentum is positive and the variance is non-zero.
const RISING_A: [&str; 7] = ["100", "101", "103", "104", "106", "107", "109"];
const RISING_B: [&str; 7] = ["50", "51", "52", "54", "55", "57", "58"];

// ---- helpers: values -----------------------------------------------------------------------

fn strategy() -> TrendVolTarget {
    TrendVolTarget::new(TrendParams {
        lookback_days: 5,
        skip_days: 1,
        vol_window: 3,
        target_vol: Decimal::new(9, 2),
        max_weight: Decimal::new(125, 3),
        min_trade_notional: Decimal::new(25, 0),
        sessions_per_year: 252,
    })
    .unwrap()
}

fn sym(s: &str) -> Symbol {
    Symbol::new(s)
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// A daily bar for `symbol` on `day` (days count from 0; the timestamp is `day` × 86,400 s).
fn bar(symbol: &str, day: u64, open: &str, close: &str) -> Bar {
    let open_px: Decimal = open.parse().unwrap();
    let close_px: Decimal = close.parse().unwrap();
    let (high, low) = if open_px >= close_px {
        (open, close)
    } else {
        (close, open)
    };
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let ts = UnixNanos::from(day * DAY_NANOS);
    Bar::new(
        bar_type,
        Price::from(open),
        Price::from(high),
        Price::from(low),
        Price::from(close),
        Quantity::from("1000"),
        ts,
        ts,
    )
}

/// One session per index of `series` (all series the same length), on day `start_day + index`.
fn sessions(series: &[(&str, &[&str])], start_day: u64) -> Vec<Vec<Bar>> {
    let len = series[0].1.len();
    (0..len)
        .map(|i| {
            series
                .iter()
                .map(|(symbol, closes)| {
                    let px = closes[i];
                    bar(symbol, start_day + i as u64, px, px)
                })
                .collect()
        })
        .collect()
}

/// A clock just after the test bars (days 0 to 8), so the newest bar has a known, small age.
fn clock() -> ManualClock {
    ManualClock::new(UnixNanos::from(9 * DAY_NANOS))
}

/// The reviewed limits file in the repository.
fn limits() -> Limits {
    Limits::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    )))
    .unwrap()
}

/// The reviewed limits file with `flatten_on_kill` set as given.
fn limits_flattening(flatten: bool) -> Limits {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    ))
    .unwrap();
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["flatten_on_kill"] = Value::Bool(flatten);
    Limits::from_json(&value).unwrap()
}

fn config(symbols: Vec<Symbol>, limits: Limits) -> TraderConfig {
    TraderConfig {
        strategy_version_id: StrategyVersionId(1),
        symbols,
        window: strategy().window_spec(),
        data_snapshot_id: "snap".into(),
        seed: 100,
        sync: SyncOptions {
            never_arrived_grace_nanos: 0,
            never_arrived_recheck_nanos: 0,
        },
        limits,
        risk: RiskState::default(),
    }
}

/// An order for AAA with `id`, formed at a fixed time.
fn order(id: &str, side: OrderSide, qty: &str) -> OrderIntent {
    OrderIntent {
        decision_id: DecisionId(1),
        client_order_id: ClientOrderId::new(id),
        symbol: sym(AAA),
        side,
        order_type: OrderType::Market,
        qty: Quantity::from(qty),
        limit_px: None,
        time_in_force: TimeInForce::Day,
        ts: UnixNanos::from(1_000_u64),
    }
}

/// A broker update for `order` at `state`, with `cum` filled, a fill of `fill_qty` at 100 with
/// trade id `trade`, and broker time `ts`.
fn fill_update(
    order: &OrderIntent,
    state: OrderState,
    cum: &str,
    fill_qty: &str,
    trade: &str,
    ts: u64,
) -> OrderUpdate {
    OrderUpdate {
        order: snapshot(order, state, cum, Some("100")),
        fill: Some(FillDetail {
            trade_id: TradeId::new(trade),
            px: Price::from("100"),
            qty: Quantity::from(fill_qty),
        }),
        ts: UnixNanos::from(ts),
    }
}

// ---- helpers: the broker -------------------------------------------------------------------

/// What the test doubles saw, and what the test sets on them. Shared between the broker, the
/// sink and the test, so the sink can read how many orders the broker had when it flushed.
#[derive(Debug, Default)]
struct Calls {
    /// Every submit call, in order.
    submits: Vec<ClientOrderId>,
    cancel_all: u32,
    /// The equity `account` reports, when set (otherwise the simulated account's own).
    equity: Option<Decimal>,
    /// Positions the broker reports on top of the simulated book.
    extra_positions: Vec<BrokerPosition>,
}

/// A handle on the shared state. Clones share it.
#[derive(Debug, Clone, Default)]
struct Handle(Arc<Mutex<Calls>>);

fn new_calls() -> Handle {
    Handle::default()
}

/// Locks the shared state, ignoring poisoning.
fn calls_lock(calls: &Handle) -> MutexGuard<'_, Calls> {
    calls.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every submit call so far.
fn submits(calls: &Handle) -> Vec<ClientOrderId> {
    calls_lock(calls).submits.clone()
}

fn set_equity(calls: &Handle, equity: &str) {
    calls_lock(calls).equity = Some(d(equity));
}

/// A `SimBroker` (fills at the next session's open, no costs) that records its calls.
#[derive(Debug)]
struct Sim {
    inner: SimBroker,
    calls: Handle,
}

impl Sim {
    fn new(calls: Handle) -> Self {
        Self {
            inner: SimBroker::new(
                Decimal::from(100_000),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            calls,
        }
    }

    fn on_session(&self, bars: &[Bar]) -> Result<(), EngineError> {
        self.inner.on_session(bars)
    }

    fn drain_updates(&self) -> Vec<OrderUpdate> {
        self.inner.drain_updates()
    }
}

impl Broker for Sim {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        calls_lock(&self.calls).submits.push(intent.client_order_id);
        self.inner.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.inner.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        calls_lock(&self.calls).cancel_all += 1;
        self.inner.cancel_all().await
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.inner.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.inner.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.inner.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        let mut positions = self.inner.positions().await?;
        let extra = calls_lock(&self.calls).extra_positions.clone();
        positions.extend(extra);
        Ok(positions)
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        let equity = calls_lock(&self.calls).equity;
        let mut account = self.inner.account().await?;
        if let Some(equity) = equity {
            account.equity = format!("{equity} USD")
                .parse::<Money>()
                .expect("valid test money");
        }
        Ok(account)
    }
}

// ---- helpers: the sink ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct SinkError(String);

/// The sink's state at one flush: how many records it held, and how many orders the broker
/// had received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flush {
    records: usize,
    orders_at_broker: usize,
}

#[derive(Debug, Default)]
struct SinkState {
    records: Vec<StoreRecord>,
    flushes: Vec<Flush>,
    fail_flush: bool,
}

/// An in-memory sink that can fail `emit` and `flush` separately, and counts every flush
/// together with the broker's order count at that moment. Clones share state.
#[derive(Debug, Clone)]
struct Rec {
    state: Arc<Mutex<SinkState>>,
    calls: Handle,
}

impl Rec {
    fn new(calls: Handle) -> Self {
        Self {
            state: Arc::new(Mutex::new(SinkState::default())),
            calls,
        }
    }

    fn lock(&self) -> MutexGuard<'_, SinkState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn records(&self) -> Vec<StoreRecord> {
        self.lock().records.clone()
    }

    fn flushes(&self) -> Vec<Flush> {
        self.lock().flushes.clone()
    }

    fn set_fail_flush(&self, fail: bool) {
        self.lock().fail_flush = fail;
    }
}

impl RecordSink for Rec {
    type Error = SinkError;

    async fn emit(&self, record: StoreRecord) -> Result<(), SinkError> {
        self.lock().records.push(record);
        Ok(())
    }

    async fn flush(&self) -> Result<(), SinkError> {
        let orders_at_broker = calls_lock(&self.calls).submits.len();
        let mut state = self.lock();
        if state.fail_flush {
            return Err(SinkError("flush refused".to_owned()));
        }
        let records = state.records.len();
        state.flushes.push(Flush {
            records,
            orders_at_broker,
        });
        Ok(())
    }
}

/// A kill-switch source that always answers the same.
struct Fixed(bool);

impl KillSwitchSource for Fixed {
    type Error = Infallible;

    async fn kill_switch_engaged(&self) -> Result<bool, Infallible> {
        Ok(self.0)
    }
}

// ---- helpers: the trader -------------------------------------------------------------------

type Engine<'a, C> = Trader<TrendVolTarget, Sim, &'a MemoryStore, &'a MemoryStore, Rec, C>;

/// A trader over `symbols` (sorted here), with `broker`, decisions and intents in `store`,
/// records in `sink` and the given clock and starting order book.
fn start<'a, C: Clock>(
    store: &'a MemoryStore,
    sink: &Rec,
    broker: Sim,
    clock: C,
    symbols: &[&str],
    limits: Limits,
    state: OrderBookState,
) -> Engine<'a, C> {
    let mut sorted: Vec<Symbol> = symbols.iter().map(|s| sym(s)).collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Trader::new(
        strategy(),
        OrderGateway::new(broker, store),
        store,
        sink.clone(),
        clock,
        config(sorted, limits),
        state,
    )
    .unwrap()
}

/// What the backtest loop does for one session: fills, then the updates, then the bars.
async fn feed<C: Clock>(trader: &mut Engine<'_, C>, session: &[Bar]) {
    trader.broker().on_session(session).unwrap();
    for update in trader.broker().drain_updates() {
        trader.on_order_update(&update).await.unwrap();
    }
    for bar in session {
        trader.on_bar(bar).unwrap();
    }
}

/// Feeds the seven rising sessions of AAA alone, starting at day 0.
async fn feed_rising_a<C: Clock>(trader: &mut Engine<'_, C>) {
    for session in sessions(&[(AAA, &RISING_A)], 0) {
        feed(trader, &session).await;
    }
}

/// Feeds the seven rising sessions of AAA and BBB, starting at day 0.
async fn feed_rising_ab<C: Clock>(trader: &mut Engine<'_, C>) {
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        feed(trader, &session).await;
    }
}

/// The risk rows for one decision, in emit order.
fn risk_rows(sink: &Rec, id: DecisionId) -> Vec<RiskCheck> {
    sink.records()
        .into_iter()
        .filter_map(|record| match record {
            StoreRecord::Risk(check) if check.decision_id == id => Some(check),
            _ => None,
        })
        .collect()
}

/// The rules that failed for one decision, in emit order.
fn failed_rules(sink: &Rec, id: DecisionId) -> Vec<String> {
    risk_rows(sink, id)
        .into_iter()
        .filter(|row| !row.passed)
        .map(|row| row.rule)
        .collect()
}

/// The risk events in the sink, in emit order.
fn risk_events(sink: &Rec) -> Vec<RiskEvent> {
    sink.records()
        .into_iter()
        .filter_map(|record| match record {
            StoreRecord::RiskEvent(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// The client order id the trader gives decision `id`.
fn client_id_of(id: DecisionId) -> ClientOrderId {
    ClientOrderId::new(format!("nisse-{}-0", id.0))
}

// ---- 1 and 2: no short through an unfilled buy ---------------------------------------------

/// Enters AAA (the buy is left unfilled), then drops the account 20% so the mode is flat.
/// Returns the trader, the on-order buy quantity and the buy's client order id.
async fn enter_then_go_flat<'a>(
    store: &'a MemoryStore,
    sink: &Rec,
    calls: &Handle,
    clk: &'a ManualClock,
    limits: Limits,
) -> (Engine<'a, &'a ManualClock>, Decimal, ClientOrderId) {
    let mut trader = start(
        store,
        sink,
        Sim::new(calls.clone()),
        clk,
        &[AAA],
        limits,
        OrderBookState::default(),
    );
    feed_rising_a(&mut trader).await;
    // The first rebalance observes 100,000: the peak.
    let entered = trader.rebalance().await.unwrap();
    assert_eq!(entered.decisions[0].2, Action::Enter);
    assert_eq!(entered.orders_submitted, 1);
    let buy = store.intents()[0];
    // The buy is not fed: it stays unfilled.
    set_equity(calls, "80000");
    assert_eq!(
        trader.observe_equity(d("80000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );
    (trader, buy.qty.as_decimal(), buy.client_order_id)
}

#[tokio::test]
async fn s1_a_sell_is_not_approved_against_shares_only_on_order() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let (mut trader, on_order, buy_id) =
        enter_then_go_flat(&store, &sink, &calls, &clk, limits()).await;

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.decisions.len(), 1);
    let (id, _, action) = report.decisions[0];
    assert_eq!(action, Action::Exit);
    let decision = store.decisions().last().cloned().unwrap();
    assert_eq!(decision.intended_qty, -on_order);
    assert_eq!(decision.features[feature::POSITION_QTY], on_order);
    let no_short = risk_rows(&sink, id)
        .into_iter()
        .find(|row| row.rule == rule::NO_SHORT)
        .unwrap();
    assert!(!no_short.passed);
    assert_eq!(no_short.limit_value, Decimal::ZERO);
    assert_eq!(no_short.actual_value, on_order);
    assert_eq!(failed_rules(&sink, id), vec![rule::NO_SHORT.to_owned()]);
    assert_eq!(report.orders_blocked, 1);
    assert_eq!(report.orders_submitted, 0);
    assert_eq!(submits(&calls), vec![buy_id]);
    assert!(trader.needs_flatten());

    // Going flat also cancelled the waiting buy, so it never fills: at the next session the
    // account holds nothing, nothing is on order, and there is nothing left to exit.
    feed(&mut trader, &sessions(&[(AAA, &["109"])], 7)[0]).await;
    assert_eq!(trader.state().positions.get(sym(AAA)).qty, Decimal::ZERO);
    let after = trader.rebalance().await.unwrap();

    assert_eq!(after.orders_blocked, 0);
    assert_eq!(after.orders_submitted, 0);
    assert_eq!(after.decisions[0].2, Action::Hold);
    assert_eq!(submits(&calls), vec![buy_id]);
    assert!(!trader.needs_flatten());
}

#[tokio::test]
async fn s2_a_sell_against_shares_on_order_fails_kill_switch_and_no_short() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let (mut trader, on_order, buy_id) =
        enter_then_go_flat(&store, &sink, &calls, &clk, limits_flattening(true)).await;
    // The kill switch is set without cancelling the open buy, which must stay unfilled.
    trader.set_kill_switch(true);

    let report = trader.rebalance().await.unwrap();

    let (id, _, _) = report.decisions[0];
    let decision = store.decisions().last().cloned().unwrap();
    assert_eq!(decision.intended_qty, -on_order);
    assert_eq!(
        failed_rules(&sink, id),
        vec![rule::KILL_SWITCH.to_owned(), rule::NO_SHORT.to_owned()]
    );
    assert_eq!(report.orders_blocked, 1);
    assert_eq!(submits(&calls), vec![buy_id]);
}

// ---- 3: a sell cannot count on shares already promised to another sell -------------------

#[tokio::test]
async fn s3_a_working_sell_is_counted_so_no_second_sell_goes_out() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let (mut trader, _, _) = enter_then_go_flat(&store, &sink, &calls, &clk, limits()).await;
    // The buy fills; the account is still flat, so the first exit goes out.
    feed(&mut trader, &sessions(&[(AAA, &["109"])], 7)[0]).await;
    let held = trader.state().positions.get(sym(AAA)).qty;
    assert!(held > Decimal::ZERO);
    let first = trader.rebalance().await.unwrap();
    assert_eq!(first.orders_submitted, 1);
    assert_eq!(first.decisions[0].2, Action::Exit);
    let submitted = submits(&calls).len();
    assert_eq!(submitted, 2);
    // The sell is not fed: it stays open at the broker.

    let second = trader.rebalance().await.unwrap();

    // Held shares plus the working sell net to zero, so there is nothing left to exit.
    assert_eq!(second.decisions[0].2, Action::Hold);
    assert!(store.decisions().last().unwrap().intended_qty.is_zero());
    assert_eq!(
        store.decisions().last().unwrap().features[feature::POSITION_QTY],
        Decimal::ZERO
    );
    assert_eq!(second.orders_submitted, 0);
    assert_eq!(second.orders_blocked, 0);
    assert_eq!(submits(&calls).len(), submitted);
}

// ---- 4: kill switch durability and latch --------------------------------------------------

#[tokio::test]
async fn s4a_engaging_the_kill_switch_records_the_event() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );

    trader.engage_kill_switch("me").await.unwrap();

    let events = risk_events(&sink);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, RiskEventKind::KillSwitch);
    assert_eq!(events[0].detail["engaged"].as_bool(), Some(true));
    assert_eq!(events[0].detail["by"].as_str(), Some("me"));
    assert!(trader.risk().state().kill_switch_engaged);
    assert_eq!(calls_lock(&calls).cancel_all, 1);
    assert!(!sink.flushes().is_empty());
}

#[tokio::test]
async fn s4b_a_failed_flush_is_a_store_error_but_the_flag_and_the_cancel_stand() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    sink.set_fail_flush(true);

    let result = trader.engage_kill_switch("me").await;

    assert!(matches!(result, Err(EngineError::Store(_))));
    assert!(trader.risk().state().kill_switch_engaged);
    assert_eq!(calls_lock(&calls).cancel_all, 1);
}

#[tokio::test]
async fn s4c_a_locally_engaged_switch_survives_a_source_that_says_off() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.engage_kill_switch("me").await.unwrap();

    assert!(trader.refresh_kill_switch(&Fixed(false)).await);
    assert!(trader.risk().state().kill_switch_engaged);
}

#[tokio::test]
async fn s4d_release_clears_the_flag_records_it_and_the_source_can_then_say_off() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.engage_kill_switch("me").await.unwrap();

    trader.release_kill_switch("me").await.unwrap();

    assert!(!trader.risk().state().kill_switch_engaged);
    let release = risk_events(&sink).pop().unwrap();
    assert_eq!(release.kind, RiskEventKind::KillSwitch);
    assert_eq!(release.detail["engaged"].as_bool(), Some(false));
    assert_eq!(release.detail["by"].as_str(), Some("me"));
    assert!(!trader.refresh_kill_switch(&Fixed(false)).await);
}

#[tokio::test]
async fn s4e_a_failed_release_leaves_the_switch_engaged() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.engage_kill_switch("me").await.unwrap();
    sink.set_fail_flush(true);

    let result = trader.release_kill_switch("me").await;

    assert!(matches!(result, Err(EngineError::Store(_))));
    assert!(trader.risk().state().kill_switch_engaged);
    assert!(trader.refresh_kill_switch(&Fixed(false)).await);
}

#[tokio::test]
async fn s4f_a_switch_set_only_from_the_source_is_lowered_by_the_source() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );

    assert!(trader.refresh_kill_switch(&Fixed(true)).await);
    assert!(trader.risk().state().kill_switch_engaged);
    assert!(!trader.refresh_kill_switch(&Fixed(false)).await);
    assert!(!trader.risk().state().kill_switch_engaged);
}

// ---- 5: risk rows are flushed before the order is sent ------------------------------------

#[tokio::test]
async fn s5_an_approved_order_is_preceded_by_a_flush_after_its_risk_rows() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA, BBB],
        limits(),
        OrderBookState::default(),
    );
    feed_rising_ab(&mut trader).await;

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 2);
    let sent = submits(&calls);
    let records = sink.records();
    let flushes = sink.flushes();
    let approved: Vec<DecisionId> = report
        .decisions
        .iter()
        .filter(|(_, _, action)| *action == Action::Enter)
        .map(|(id, _, _)| *id)
        .collect();
    assert_eq!(approved.len(), 2);
    for id in approved {
        let rows = risk_rows(&sink, id);
        assert_eq!(rows.len(), 8);
        // The index of this decision's last risk row in the record stream.
        let last_row = records
            .iter()
            .rposition(|r| matches!(r, StoreRecord::Risk(c) if c.decision_id == id))
            .unwrap();
        let position = sent.iter().position(|c| *c == client_id_of(id)).unwrap();
        // A flush after the rows, when exactly `position` orders had reached the broker:
        // so it happened before this order was sent.
        assert!(
            flushes
                .iter()
                .any(|f| f.records > last_row && f.orders_at_broker == position),
            "no flush between the risk rows and the order for {id:?}: {flushes:?}"
        );
    }
}

// ---- 6: a drawdown mode change is flushed --------------------------------------------------

#[tokio::test]
async fn s6_a_mode_change_is_flushed_and_an_unchanged_observation_is_not() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );

    // A new peak is flushed too: a restart must not come back with a lower one.
    trader.observe_equity(d("100000")).await.unwrap();
    assert_eq!(sink.flushes().len(), 1);
    trader.observe_equity(d("100000")).await.unwrap();
    assert_eq!(sink.flushes().len(), 1, "an unchanged peak must not flush");

    assert_eq!(
        trader.observe_equity(d("80000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );
    let mode_index = sink
        .records()
        .iter()
        .position(
            |r| matches!(r, StoreRecord::RiskEvent(e) if e.kind == RiskEventKind::DrawdownMode),
        )
        .unwrap();
    let flushes = sink.flushes();
    assert!(!flushes.is_empty());
    assert!(
        flushes.iter().any(|f| f.records > mode_index),
        "no flush after the mode event: {flushes:?}"
    );

    let before = sink.flushes().len();
    assert_eq!(trader.observe_equity(d("80000")).await.unwrap(), None);
    assert_eq!(
        sink.flushes().len(),
        before,
        "an unchanged observation must not flush"
    );
}

// ---- 7: a position outside the universe ----------------------------------------------------

#[tokio::test]
async fn s7_a_position_outside_the_universe_blocks_every_buy_as_unknown_exposure() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    // XYZ is held by the book and by the broker, and is not in the universe.
    let mut state = OrderBookState::default();
    let _ = state.positions.apply_fill(
        sym(XYZ),
        OrderSide::Buy,
        d("50"),
        d("10"),
        UnixNanos::from(1_000_u64),
    );
    calls_lock(&calls).extra_positions.push(BrokerPosition {
        symbol: sym(XYZ),
        side: PositionSide::Long,
        qty: Quantity::from("50"),
        avg_entry_px: Price::from("10"),
    });
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA, BBB],
        limits(),
        state,
    );
    feed_rising_ab(&mut trader).await;

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(submits(&calls).is_empty());
    for (id, _, action) in &report.decisions {
        assert_eq!(*action, Action::Enter);
        assert_eq!(
            failed_rules(&sink, *id),
            vec![rule::KELLY_EXPOSURE.to_owned()]
        );
        let kelly = risk_rows(&sink, *id)
            .into_iter()
            .find(|row| row.rule == rule::KELLY_EXPOSURE)
            .unwrap();
        assert_eq!(kelly.actual_value, Decimal::NEGATIVE_ONE);
    }
    // The position survived the reconciliation.
    assert_eq!(trader.state().positions.get(sym(XYZ)).qty, d("50"));
}

// ---- 8: a bar stamped in the future --------------------------------------------------------

#[tokio::test]
async fn s8_a_bar_after_the_clock_blocks_every_buy_as_stale() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA, BBB],
        limits(),
        OrderBookState::default(),
    );
    feed_rising_ab(&mut trader).await;
    // The newest bar is on day 6; the clock is on day 5.
    clk.set(UnixNanos::from(5 * DAY_NANOS));

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(submits(&calls).is_empty());
    for (id, _, action) in &report.decisions {
        assert_eq!(*action, Action::Enter);
        assert!(failed_rules(&sink, *id).contains(&rule::STALE_DATA.to_owned()));
        let stale = risk_rows(&sink, *id)
            .into_iter()
            .find(|row| row.rule == rule::STALE_DATA)
            .unwrap();
        assert_eq!(stale.actual_value, Decimal::NEGATIVE_ONE);
    }
}

// ---- 9: a redelivered fill on a later day -------------------------------------------------

#[tokio::test]
async fn s9_a_redelivered_buy_fill_on_a_later_day_is_not_a_new_fill() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let buy = order("nisse-1-0", OrderSide::Buy, "10");
    let mut state = OrderBookState::default();
    state.tracker.track_intent(buy).unwrap();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        state,
    );
    let day = 7 * DAY_NANOS;

    trader
        .on_order_update(&fill_update(
            &buy,
            OrderState::Filled,
            "10",
            "10",
            "trade-1",
            day + HOUR_NANOS,
        ))
        .await
        .unwrap();
    // The same message, redelivered with its time moved to the next day.
    trader
        .on_order_update(&fill_update(
            &buy,
            OrderState::Filled,
            "10",
            "10",
            "trade-1",
            day + DAY_NANOS + HOUR_NANOS,
        ))
        .await
        .unwrap();

    let next_day = UnixNanos::from(day + DAY_NANOS + 2 * HOUR_NANOS);
    let days = trader.risk().day_trades();
    assert!(!days.would_day_trade(sym(AAA), OrderSide::Sell, next_day));
    assert_eq!(days.count_in_window(next_day), 0);
}

#[tokio::test]
async fn s9b_a_buy_and_a_sell_filled_on_one_day_are_one_day_trade() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let buy = order("nisse-1-0", OrderSide::Buy, "10");
    let sell = order("nisse-2-0", OrderSide::Sell, "10");
    let mut state = OrderBookState::default();
    state.tracker.track_intent(buy).unwrap();
    state.tracker.track_intent(sell).unwrap();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        state,
    );
    let day = 7 * DAY_NANOS;

    trader
        .on_order_update(&fill_update(
            &buy,
            OrderState::Filled,
            "10",
            "10",
            "trade-1",
            day + HOUR_NANOS,
        ))
        .await
        .unwrap();
    trader
        .on_order_update(&fill_update(
            &sell,
            OrderState::Filled,
            "10",
            "10",
            "trade-2",
            day + 2 * HOUR_NANOS,
        ))
        .await
        .unwrap();

    let same_day = UnixNanos::from(day + 3 * HOUR_NANOS);
    assert_eq!(trader.risk().day_trades().count_in_window(same_day), 1);
}

// ---- 10: record_past_fill restores the count ----------------------------------------------

#[tokio::test]
async fn s10_fills_fed_after_a_restart_restore_the_day_trade_count() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    let day = UnixNanos::from(7 * DAY_NANOS + HOUR_NANOS);

    trader.record_past_fill(sym(AAA), OrderSide::Buy, day);
    trader.record_past_fill(
        sym(AAA),
        OrderSide::Sell,
        UnixNanos::from(7 * DAY_NANOS + 2 * HOUR_NANOS),
    );

    assert_eq!(
        trader
            .risk()
            .day_trades()
            .count_in_window(UnixNanos::from(7 * DAY_NANOS + 3 * HOUR_NANOS)),
        1
    );
}

// ---- 11: the limits record -----------------------------------------------------------------

#[tokio::test]
async fn s11_start_up_records_one_limits_loaded_event_with_the_hash_and_the_limits() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );

    trader.record_limits_loaded().await.unwrap();

    let loaded: Vec<RiskEvent> = risk_events(&sink)
        .into_iter()
        .filter(|e| e.kind == RiskEventKind::LimitsLoaded)
        .collect();
    assert_eq!(loaded.len(), 1);
    let expected = limits();
    let hash = expected.hash().to_string();
    assert_eq!(loaded[0].detail["hash"].as_str(), Some(hash.as_str()));
    assert_eq!(loaded[0].detail["limits"], expected.to_json());
}

// ---- 12: needs_flatten ---------------------------------------------------------------------

#[tokio::test]
async fn s12_needs_flatten_is_false_in_normal_mode_with_a_position() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    feed_rising_a(&mut trader).await;
    trader.rebalance().await.unwrap();
    feed(&mut trader, &sessions(&[(AAA, &["109"])], 7)[0]).await;
    assert!(trader.state().positions.get(sym(AAA)).qty > Decimal::ZERO);

    assert!(!trader.needs_flatten());
}

#[tokio::test]
async fn s12_needs_flatten_is_true_when_flat_with_a_position() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    feed_rising_a(&mut trader).await;
    trader.rebalance().await.unwrap();
    feed(&mut trader, &sessions(&[(AAA, &["109"])], 7)[0]).await;
    trader.observe_equity(d("80000")).await.unwrap();

    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
    assert!(trader.needs_flatten());
}

#[tokio::test]
async fn s12_needs_flatten_is_false_when_flat_with_no_position_and_no_order() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.observe_equity(d("100000")).await.unwrap();
    trader.observe_equity(d("80000")).await.unwrap();

    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
    assert!(!trader.needs_flatten());
}

// ---- 13: the backtest keeps exiting while flat (variant: flat scenario, no unfilled buy) ---

const ASSET: &str = "ASSET";
/// 2023-01-02, a Monday, as days since 1970-01-01.
const JAN_2_2023: u64 = 19_359;
const SESSIONS: usize = 130;
/// The session the price falls by 35%: 2023-03-01 (index 42).
const CRASH: usize = 42;

/// The close of the given UTC day, at 21:00 UTC.
fn close_on(day: u64) -> UnixNanos {
    UnixNanos::from((day * 86_400 + 21 * 3_600) * 1_000_000_000)
}

/// `SESSIONS` weekday closes from 2023-01-02.
fn closes() -> Vec<UnixNanos> {
    (JAN_2_2023..)
        .filter(|day| (day - JAN_2_2023) % 7 < 5)
        .take(SESSIONS)
        .map(close_on)
        .collect()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// A price at exactly 4 decimal places.
fn px(value: Decimal) -> Price {
    let mut at_4dp = value.round_dp(4);
    at_4dp.rescale(4);
    Price::from(at_4dp.to_string().as_str())
}

/// One bar per close. `changes[i]` moves the close of session `i`; the open is the previous close
/// times 1.002 and the close alternates by 0.1% either way.
fn shaped(closes: &[UnixNanos], start_px: Decimal, changes: &[Decimal]) -> Vec<Bar> {
    let wobble = dec("0.001");
    let open_gap = dec("1.002");
    let instrument_id = InstrumentId::new(Symbol::new(ASSET), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let mut previous = start_px;
    closes
        .iter()
        .zip(changes.iter().copied())
        .enumerate()
        .map(|(i, (&ts, daily_change))| {
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

/// One asset that rises 1% a session until `CRASH`, falls by 35% there, and rises 1% after.
fn crash_path(closes: &[UnixNanos]) -> Vec<Bar> {
    let changes: Vec<Decimal> = (0..closes.len())
        .map(|i| match i.cmp(&CRASH) {
            std::cmp::Ordering::Less => dec("0.01"),
            std::cmp::Ordering::Equal => dec("-0.35"),
            std::cmp::Ordering::Greater => dec("0.01"),
        })
        .collect();
    shaped(closes, dec("100"), &changes)
}

/// The strategy the backtest scenario runs: the parameters of `crates/engine/tests/backtest.rs`.
/// Its position is half of equity, so a 35% fall takes the account past the flat threshold.
fn backtest_strategy() -> TrendVolTarget {
    TrendVolTarget::new(TrendParams {
        lookback_days: 20,
        skip_days: 2,
        vol_window: 10,
        target_vol: dec("0.09"),
        max_weight: dec("0.5"),
        min_trade_notional: Decimal::from(25),
        sessions_per_year: 252,
    })
    .unwrap()
}

fn backtest_config() -> BacktestConfig {
    BacktestConfig {
        strategy_version_id: StrategyVersionId(1),
        window: backtest_strategy().window_spec(),
        starting_cash: Decimal::from(100_000),
        costs: CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
        data_snapshot_id: "snap-risk-regressions".to_owned(),
        seed: 7,
        limits: limits(),
    }
}

/// The signed shares a fill moved: buys positive, sells negative.
fn signed_fill_qty(decisions: &[Decision], fill: &Fill) -> Decimal {
    let index: usize = fill
        .client_order_id
        .as_str()
        .strip_prefix("nisse-")
        .and_then(|rest| rest.strip_suffix("-0"))
        .and_then(|n| n.parse().ok())
        .unwrap();
    let qty = fill.qty.as_decimal();
    if decisions[index - 1].intended_qty < Decimal::ZERO {
        -qty
    } else {
        qty
    }
}

#[tokio::test]
async fn s13_once_flat_the_backtest_rebalances_only_at_month_ends() {
    // Variant: the flat scenario of `crates/engine/tests/backtest.rs`, not an unfilled buy. A buy
    // cannot be unfilled when the account goes flat with daily bars: a buy is submitted at a
    // close and fills at the next open, which is before that session's equity is observed.
    let closes = closes();
    let calendar = MarketCalendar::from_closes(&closes).unwrap();
    let bars = crash_path(&closes);

    let run = run_backtest(backtest_strategy(), &bars, &calendar, backtest_config())
        .await
        .unwrap();

    let exit = closes[CRASH + 1];
    assert_eq!(run.final_mode, DrawdownMode::Flat);
    let exit_fills: Vec<&Fill> = run.fills.iter().filter(|f| f.ts == exit).collect();
    assert_eq!(exit_fills.len(), 1);
    assert!(signed_fill_qty(&run.decisions, exit_fills[0]) < Decimal::ZERO);

    // The position reconstructed from fills is exactly zero at the end.
    let held: Decimal = run
        .fills
        .iter()
        .map(|f| signed_fill_qty(&run.decisions, f))
        .sum();
    assert_eq!(held, Decimal::ZERO);

    // After the exit, decisions happen only at month ends: nothing keeps exiting.
    let month_ends: BTreeSet<u64> = closes
        .iter()
        .filter(|ts| **ts >= exit)
        .filter(|ts| {
            calendar
                .session_at(UnixNanos::from(ts.as_u64() - 1))
                .and_then(|session| calendar.is_last_session_of_month(session.date))
                == Some(true)
        })
        .map(|ts| ts.as_u64())
        .collect();
    let decided: BTreeSet<u64> = run
        .decisions
        .iter()
        .filter(|decision| decision.ts >= exit)
        .map(|decision| decision.ts.as_u64())
        .collect();
    assert!(!month_ends.is_empty());
    assert_eq!(decided, month_ends);
}

// ---- the second review: fills, sells, flattening and the kill switch -----------------------

/// Cancel requests the broker received, by venue order id, in order.
type CancelLog = Arc<Mutex<Vec<VenueOrderId>>>;

fn new_cancel_log() -> CancelLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn cancels_of(log: &CancelLog) -> Vec<VenueOrderId> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A `Sim` that logs every cancel request. With `fill_on_submit` it answers each submit with the
/// order already fully filled at 100, as a broker may, and does not queue it at the simulated
/// exchange. Everything else is the `Sim`.
#[derive(Debug)]
struct Probe {
    inner: Sim,
    fill_on_submit: bool,
    cancels: CancelLog,
}

impl Broker for Probe {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        if !self.fill_on_submit {
            return self.inner.submit(intent).await;
        }
        calls_lock(&self.inner.calls)
            .submits
            .push(intent.client_order_id);
        Ok(snapshot(
            intent,
            OrderState::Filled,
            &intent.qty.to_string(),
            Some("100"),
        ))
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(venue_order_id);
        self.inner.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        self.inner.cancel_all().await
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.inner.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.inner.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.inner.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.inner.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        self.inner.account().await
    }
}

type ProbeEngine<'a, C> = Trader<TrendVolTarget, Probe, &'a MemoryStore, &'a MemoryStore, Rec, C>;

/// A trader over AAA with a `Probe` broker and no starting book.
fn probe_engine<'a>(
    store: &'a MemoryStore,
    sink: &Rec,
    calls: &Handle,
    cancels: &CancelLog,
    clk: &'a ManualClock,
    fill_on_submit: bool,
) -> ProbeEngine<'a, &'a ManualClock> {
    let probe = Probe {
        inner: Sim::new(calls.clone()),
        fill_on_submit,
        cancels: Arc::clone(cancels),
    };
    Trader::new(
        strategy(),
        OrderGateway::new(probe, store),
        store,
        sink.clone(),
        clk,
        config(vec![sym(AAA)], limits()),
        OrderBookState::default(),
    )
    .unwrap()
}

/// What the backtest loop does for one session with a `Probe` broker.
async fn feed_probe<C: Clock>(trader: &mut ProbeEngine<'_, C>, session: &[Bar]) {
    trader.broker().inner.on_session(session).unwrap();
    for update in trader.broker().inner.drain_updates() {
        trader.on_order_update(&update).await.unwrap();
    }
    for bar in session {
        trader.on_bar(bar).unwrap();
    }
}

/// T2-1. A fill in the broker's answer to a submit counts toward day trades, once.
#[tokio::test]
async fn t2_1_a_fill_in_the_submit_answer_counts_toward_day_trades_once() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let cancels = new_cancel_log();
    let clk = clock();
    let mut trader = probe_engine(&store, &sink, &calls, &cancels, &clk, true);
    for session in sessions(&[(AAA, &RISING_A)], 0) {
        feed_probe(&mut trader, &session).await;
    }

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 1);
    let buy = store.intents()[0];
    let now = clk.now();
    assert_eq!(
        trader.state().positions.get(sym(AAA)).qty,
        buy.qty.as_decimal()
    );
    let days = trader.risk().day_trades();
    assert!(days.would_day_trade(sym(AAA), OrderSide::Sell, now));
    assert_eq!(days.count_in_window(now), 0);

    // The stream then repeats the same fill: it is not a second fill.
    let repeat = fill_update(
        &buy,
        OrderState::Filled,
        &buy.qty.to_string(),
        &buy.qty.to_string(),
        "trade-1",
        now.as_u64() + 1,
    );
    trader.on_order_update(&repeat).await.unwrap();

    let days = trader.risk().day_trades();
    assert!(days.would_day_trade(sym(AAA), OrderSide::Sell, now));
    assert_eq!(days.count_in_window(now), 0);
    assert_eq!(
        trader.state().positions.get(sym(AAA)).qty,
        buy.qty.as_decimal()
    );
}

/// T2-2. Fills that a reconciliation pass finds count toward day trades even when the pass
/// then ends with `ForeignOrders`.
#[tokio::test]
async fn t2_2_fills_found_by_a_pass_that_ends_in_foreign_orders_still_count() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let cancels = new_cancel_log();
    let clk = clock();
    let mut trader = probe_engine(&store, &sink, &calls, &cancels, &clk, false);
    for session in sessions(&[(AAA, &RISING_A)], 0) {
        feed_probe(&mut trader, &session).await;
    }
    // The buy goes out at the close of day 6 and is not yet filled.
    clk.set(UnixNanos::from(6 * DAY_NANOS + HOUR_NANOS));
    trader.rebalance().await.unwrap();
    let buy = store.intents()[0];
    // Day 7 opens and the broker fills the buy. Its update is never fed to the engine: the next
    // pass has to find the fill itself.
    trader
        .broker()
        .inner
        .on_session(&sessions(&[(AAA, &["109"])], 7)[0])
        .unwrap();
    // Someone else's order is open at the broker, which makes the next pass end in an error.
    let foreign = order("foreign-1", OrderSide::Buy, "1");
    trader.broker().inner.submit(&foreign).await.unwrap();
    clk.set(UnixNanos::from(7 * DAY_NANOS + 2 * HOUR_NANOS));

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::ForeignOrders(1))));
    assert_eq!(
        trader
            .state()
            .tracker
            .get(buy.client_order_id)
            .unwrap()
            .state,
        OrderState::Filled
    );
    let now = clk.now();
    let days = trader.risk().day_trades();
    assert!(days.would_day_trade(sym(AAA), OrderSide::Sell, now));
    assert_eq!(days.count_in_window(now), 0);
}

/// T2-3. Going flat with shares held and a buy still waiting sells what is held and cancels the
/// buy.
#[tokio::test]
async fn t2_3_flat_with_shares_held_and_a_buy_waiting_sells_the_held_and_cancels_the_buy() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let cancels = new_cancel_log();
    let clk = clock();
    let mut trader = probe_engine(&store, &sink, &calls, &cancels, &clk, false);
    for session in sessions(&[(AAA, &RISING_A)], 0) {
        feed_probe(&mut trader, &session).await;
    }
    // The first buy fills at the open of day 7: AAA is held.
    set_equity(&calls, "100000");
    trader.rebalance().await.unwrap();
    feed_probe(&mut trader, &sessions(&[(AAA, &["109"])], 7)[0]).await;
    let held = trader.state().positions.get(sym(AAA)).qty;
    assert_eq!(held, store.intents()[0].qty.as_decimal());
    // Equity rises to 120,000, so the target grows and a further buy goes out, unfilled.
    set_equity(&calls, "120000");
    let grown = trader.rebalance().await.unwrap();
    assert_eq!(grown.orders_submitted, 1);
    let waiting = store.intents()[1];
    assert_eq!(waiting.side, OrderSide::Buy);
    let waiting_venue = trader
        .state()
        .tracker
        .get(waiting.client_order_id)
        .unwrap()
        .venue_order_id
        .unwrap();
    // Equity falls 20% below the 120,000 peak: flat.
    set_equity(&calls, "96000");
    assert_eq!(
        trader.observe_equity(d("96000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );

    let report = trader.rebalance().await.unwrap();

    let (id, _, action) = report.decisions[0];
    assert_eq!(action, Action::Exit);
    let decision = store.decisions().last().cloned().unwrap();
    assert_eq!(decision.intended_qty, -(held + waiting.qty.as_decimal()));
    let sell = store.intents()[2];
    assert_eq!(sell.side, OrderSide::Sell);
    assert_eq!(sell.qty.as_decimal(), held);
    let rows = risk_rows(&sink, id);
    assert_eq!(rows.len(), 8);
    assert!(rows.iter().all(|row| row.passed), "{rows:?}");
    let no_short = rows.iter().find(|row| row.rule == rule::NO_SHORT).unwrap();
    assert_eq!(no_short.actual_value, held);
    assert_eq!(report.orders_submitted, 1);
    assert_eq!(report.orders_blocked, 0);
    assert_eq!(cancels_of(&cancels), vec![waiting_venue]);
}

/// T2-4. Going flat with nothing held and only a buy waiting cancels that buy.
#[tokio::test]
async fn t2_4_flat_with_only_a_buy_waiting_cancels_the_buy() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let cancels = new_cancel_log();
    let clk = clock();
    let mut trader = probe_engine(&store, &sink, &calls, &cancels, &clk, false);
    for session in sessions(&[(AAA, &RISING_A)], 0) {
        feed_probe(&mut trader, &session).await;
    }
    trader.rebalance().await.unwrap();
    let buy = store.intents()[0];
    let venue = trader
        .state()
        .tracker
        .get(buy.client_order_id)
        .unwrap()
        .venue_order_id
        .unwrap();
    set_equity(&calls, "80000");
    trader.observe_equity(d("80000")).await.unwrap();

    trader.rebalance().await.unwrap();

    assert_eq!(cancels_of(&cancels), vec![venue]);
}

/// T2-5a. `set_kill_switch(false)` does not lower a switch engaged in this process.
#[tokio::test]
async fn t2_5_a_set_kill_switch_off_does_not_lower_a_switch_engaged_here() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls.clone()),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.engage_kill_switch("me").await.unwrap();

    trader.set_kill_switch(false);

    assert!(trader.risk().state().kill_switch_engaged);
}

/// T2-5b. A switch set only by `set_kill_switch(true)` is lowered by `set_kill_switch(false)`.
#[tokio::test]
async fn t2_5_b_set_kill_switch_off_lowers_a_switch_set_only_by_set_kill_switch() {
    let store = MemoryStore::new();
    let calls = new_calls();
    let sink = Rec::new(calls.clone());
    let clk = clock();
    let mut trader = start(
        &store,
        &sink,
        Sim::new(calls),
        &clk,
        &[AAA],
        limits(),
        OrderBookState::default(),
    );
    trader.set_kill_switch(true);
    assert!(trader.risk().state().kill_switch_engaged);

    trader.set_kill_switch(false);

    assert!(!trader.risk().state().kill_switch_engaged);
}
