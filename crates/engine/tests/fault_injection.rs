//! Fault-injection tests for the trading loop (phase 4, E4): stale feeds, broker faults, drawdown
//! paths and duplicated broker messages, on synthetic data. Every test asserts that the engine
//! fails closed: an order is not sent unless every risk check passed and the outcome of every
//! order is recorded.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

mod support;

use std::sync::{Mutex, MutexGuard, PoisonError};

use nisse_core::{
    AccountSnapshot, Action, Broker, BrokerOrder, BrokerPosition, Clock, Decision, DecisionId,
    FillDetail, IntentLog, KillSwitchSource, ManualClock, OpenOrders, OrderIntent, OrderState,
    OrderUpdate, ReplaceRequest, RiskCheck, RiskEvent, RiskEventKind, StoreRecord, SubmitError,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId,
        InstrumentId, Money, OrderSide, Price, PriceType, Quantity, Symbol, TradeId, UnixNanos,
        Venue, VenueOrderId,
    },
};
use nisse_engine::strategies::{TrendParams, TrendVolTarget, feature};
use nisse_engine::{
    CostModel, EngineError, MemoryStore, OrderBookState, OrderGateway, RebalanceReport, SimBroker,
    SimError, SyncOptions, Trader, TraderConfig,
};
use nisse_risk::{DrawdownMode, OrderCheck, rule};
use rust_decimal::Decimal;
use support::{MemoryIntentLog, MemorySink, intent, new_call_log, snapshot};

const AAA: &str = "AAA";
const BBB: &str = "BBB";
const DAY_NANOS: u64 = 86_400 * 1_000_000_000;
const HOUR_NANOS: u64 = 3_600 * 1_000_000_000;

/// Closes that rise and vary, so momentum is positive and the variance is non-zero.
const RISING_A: [&str; 7] = ["100", "101", "103", "104", "106", "107", "109"];
const RISING_B: [&str; 7] = ["50", "51", "52", "54", "55", "57", "58"];

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

/// One session per index of `series` (all series the same length): the bars of every symbol
/// at that index, opening and closing at the given close, on day `start_day + index`.
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

/// A fault the wrapper applies to the next `submit` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitFault {
    /// The broker refuses the order definitively. Nothing reaches the simulated exchange.
    Rejected,
    /// The call times out before reaching the exchange: outcome unknown, no order exists.
    UnknownNotSent,
    /// The order reaches the exchange, then the reply is lost: outcome unknown, the order exists.
    UnknownSent,
}

/// The faults a test can switch on, and what the wrapper observed.
#[derive(Debug, Default)]
struct Faults {
    /// `open_orders`, `positions` and `order_by_client_id` fail.
    fail_reads: bool,
    /// `account` fails.
    fail_account: bool,
    /// `cancel_all` fails.
    fail_cancel_all: bool,
    /// The equity `account` reports, when set (otherwise the simulated account's own).
    equity: Option<Decimal>,
    /// The `trading_blocked` flag `account` reports.
    blocked: bool,
    /// Applied to the next `submit` only.
    next_submit: Option<SubmitFault>,
    /// Every `submit` call, faulted or not, in call order.
    submit_calls: Vec<ClientOrderId>,
    /// Every `submit` that reached the simulated exchange, in order.
    sent: Vec<ClientOrderId>,
}

/// A broker wrapper in the style of the trader tests' `Probe`: it delegates everything to a
/// `SimBroker` and fails or alters one method when a fault is set. The fault state is behind a
/// mutex, so a test sets faults through `trader.broker()` while the trader owns the broker.
#[derive(Debug)]
struct Faulty {
    inner: SimBroker,
    faults: Mutex<Faults>,
}

impl Faulty {
    fn new() -> Self {
        Self {
            inner: SimBroker::new(
                Decimal::from(100_000),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            faults: Mutex::new(Faults::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Faults> {
        self.faults.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the faults. The lock is released before the caller can await anything.
    fn set(&self, change: impl FnOnce(&mut Faults)) {
        change(&mut self.lock());
    }

    /// Client order ids of every submit that reached the simulated exchange.
    fn sent(&self) -> Vec<ClientOrderId> {
        self.lock().sent.clone()
    }

    /// Client order ids of every submit call, faulted or not.
    fn submit_calls(&self) -> Vec<ClientOrderId> {
        self.lock().submit_calls.clone()
    }

    fn on_session(&self, bars: &[Bar]) -> Result<(), EngineError> {
        self.inner.on_session(bars)
    }

    fn drain_updates(&self) -> Vec<OrderUpdate> {
        self.inner.drain_updates()
    }

    fn read_check(&self) -> Result<(), SimError> {
        let failing = self.lock().fail_reads;
        if failing {
            Err(SimError("reads are down".to_owned()))
        } else {
            Ok(())
        }
    }

    async fn forward(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.lock().sent.push(intent.client_order_id);
        self.inner.submit(intent).await
    }
}

impl Broker for Faulty {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        let fault = {
            let mut faults = self.lock();
            faults.submit_calls.push(intent.client_order_id);
            faults.next_submit.take()
        };
        match fault {
            None => self.forward(intent).await,
            Some(SubmitFault::Rejected) => Err(SubmitError::Rejected(SimError(
                "insufficient buying power".to_owned(),
            ))),
            Some(SubmitFault::UnknownNotSent) => Err(SubmitError::Unknown(SimError(
                "timed out before the exchange".to_owned(),
            ))),
            Some(SubmitFault::UnknownSent) => {
                self.forward(intent).await?;
                Err(SubmitError::Unknown(SimError(
                    "reply lost after the exchange".to_owned(),
                )))
            }
        }
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.inner.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        let failing = self.lock().fail_cancel_all;
        if failing {
            return Err(SimError("cancel refused".to_owned()));
        }
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
        self.read_check()?;
        self.inner.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.read_check()?;
        self.inner.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.read_check()?;
        self.inner.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        let (failing, equity, blocked) = {
            let faults = self.lock();
            (faults.fail_account, faults.equity, faults.blocked)
        };
        if failing {
            return Err(SimError("account unavailable".to_owned()));
        }
        let mut account = self.inner.account().await?;
        account.trading_blocked = blocked;
        if let Some(equity) = equity {
            account.equity = format!("{equity} USD")
                .parse::<Money>()
                .expect("valid test money");
        }
        Ok(account)
    }
}

type Engine<'a, L = &'a MemoryStore, C = ManualClock> =
    Trader<TrendVolTarget, Faulty, L, &'a MemoryStore, MemorySink, C>;

fn config(symbols: Vec<Symbol>) -> TraderConfig {
    TraderConfig {
        strategy_version_id: nisse_core::StrategyVersionId(1),
        symbols,
        window: strategy().window_spec(),
        data_snapshot_id: "snap".into(),
        seed: 100,
        sync: SyncOptions {
            never_arrived_grace_nanos: 0,
            never_arrived_recheck_nanos: 0,
        },
        limits: limits(),
        risk: nisse_risk::RiskState::default(),
    }
}

/// The reviewed limits file in the repository.
fn limits() -> nisse_risk::Limits {
    nisse_risk::Limits::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    )))
    .unwrap()
}

/// A clock just after the test bars (days 0 to 8), so the newest bar has a known, small
/// age. Tests about stale data move it themselves.
fn clock() -> ManualClock {
    ManualClock::new(UnixNanos::from(9 * 86_400_000_000_000_u64))
}

/// A trader over the given universe (sorted here), with a simulated broker, decisions and
/// intents in `store`, records in `sink` and the given clock and starting order book.
fn build<'a, L: IntentLog, C: Clock>(
    store: &'a MemoryStore,
    sink: &MemorySink,
    intents: L,
    clock: C,
    symbols: &[&str],
    state: OrderBookState,
) -> Engine<'a, L, C> {
    let mut sorted: Vec<Symbol> = symbols.iter().map(|s| sym(s)).collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Trader::new(
        strategy(),
        OrderGateway::new(Faulty::new(), intents),
        store,
        sink.clone(),
        clock,
        config(sorted),
        state,
    )
    .unwrap()
}

/// A trader whose intents go to `store` too.
fn engine<'a, C: Clock>(
    store: &'a MemoryStore,
    sink: &MemorySink,
    clock: C,
    symbols: &[&str],
) -> Engine<'a, &'a MemoryStore, C> {
    build(
        store,
        sink,
        store,
        clock,
        symbols,
        OrderBookState::default(),
    )
}

/// What the backtest loop does for one session: fills, then the updates, then the bars.
async fn feed<L: IntentLog, C: Clock>(trader: &mut Engine<'_, L, C>, session: &[Bar]) {
    trader.broker().on_session(session).unwrap();
    for update in trader.broker().drain_updates() {
        trader.on_order_update(&update).await.unwrap();
    }
    for bar in session {
        trader.on_bar(bar).unwrap();
    }
}

/// Feeds the seven rising sessions of AAA and BBB, starting at day 0.
async fn feed_rising<L: IntentLog, C: Clock>(trader: &mut Engine<'_, L, C>) {
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        feed(trader, &session).await;
    }
}

/// The risk rows for one decision, in emit order.
fn risk_rows(sink: &MemorySink, id: DecisionId) -> Vec<RiskCheck> {
    sink.records()
        .into_iter()
        .filter_map(|record| match record {
            StoreRecord::Risk(check) if check.decision_id == id => Some(check),
            _ => None,
        })
        .collect()
}

/// The rules that failed for one decision, in emit order.
fn failed_rules(sink: &MemorySink, id: DecisionId) -> Vec<String> {
    risk_rows(sink, id)
        .into_iter()
        .filter(|row| !row.passed)
        .map(|row| row.rule)
        .collect()
}

/// The risk events in the sink, in emit order.
fn risk_events(sink: &MemorySink) -> Vec<RiskEvent> {
    sink.records()
        .into_iter()
        .filter_map(|record| match record {
            StoreRecord::RiskEvent(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// The number of order rows in the sink that carry a fill.
fn fill_rows(sink: &MemorySink) -> usize {
    sink.records()
        .iter()
        .filter(|r| matches!(r, StoreRecord::Order { fill: Some(_), .. }))
        .count()
}

/// The number of submits that reached the simulated exchange for `id`.
fn sent_count(trader: &Engine<'_, &MemoryStore, impl Clock>, id: &str) -> usize {
    let wanted = ClientOrderId::new(id);
    trader
        .broker()
        .sent()
        .into_iter()
        .filter(|sent| *sent == wanted)
        .count()
}

/// A source that cannot be read.
struct Unreadable;

impl KillSwitchSource for Unreadable {
    type Error = SimError;

    async fn kill_switch_engaged(&self) -> Result<bool, SimError> {
        Err(SimError("kill switch unreadable".to_owned()))
    }
}

/// A buy of `qty` shares of AAA with id `id`, formed at the fixed test time.
fn buy_of(id: &str, qty: &str) -> OrderIntent {
    OrderIntent {
        symbol: sym(AAA),
        ..intent(id, qty, None)
    }
}

/// A broker update for `order`: its state, cumulative quantity and average price, and a fill of
/// `fill_qty` at 100 with trade id `trade`.
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

// ---- A. Stale feed -------------------------------------------------------------------------

#[tokio::test]
async fn a1_feed_stops_and_every_buy_is_blocked_as_stale() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let clk = clock();
    let mut trader = engine(&store, &sink, &clk, &[AAA, BBB]);
    feed_rising(&mut trader).await;
    // The newest bar is day 6. Five days later the feed is still silent.
    clk.set(UnixNanos::from(11 * DAY_NANOS));

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.decisions.len(), 2);
    assert!(
        report
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Enter)
    );
    assert_eq!(report.orders_blocked, 2);
    assert_eq!(report.orders_submitted, 0);
    assert!(trader.broker().sent().is_empty());
    assert!(store.intents().is_empty());
    assert_eq!(store.decisions().len(), 2);
    for (id, _, _) in &report.decisions {
        assert_eq!(failed_rules(&sink, *id), vec![rule::STALE_DATA.to_owned()]);
    }
}

#[tokio::test]
async fn a2_one_silent_symbol_is_skipped_and_the_live_one_trades() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let clk = clock();
    let mut trader = engine(&store, &sink, &clk, &[AAA, BBB]);
    feed_rising(&mut trader).await;
    // Day 7: only AAA's bar arrives. BBB's newest close is a day old.
    feed(&mut trader, &sessions(&[(AAA, &["110"])], 7)[0]).await;
    clk.set(UnixNanos::from(7 * DAY_NANOS + HOUR_NANOS));

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.decisions[0].2, Action::Enter);
    assert_eq!(report.decisions[1].2, Action::Skip);
    assert_eq!(report.orders_submitted, 1);
    assert_eq!(report.orders_failed, 0);
    let decisions = store.decisions();
    // Decisions are in universe order: AAA, then BBB.
    assert!(!decisions[1].features.contains_key(feature::CLOSE));
    assert!(decisions[0].features.contains_key(feature::CLOSE));
    assert!(decisions[0].features.contains_key(feature::MOMENTUM));
    assert_eq!(store.intents().len(), 1);
    assert_eq!(store.intents()[0].symbol, sym(AAA));
}

#[tokio::test]
async fn a3_no_bar_ever_arrived_skips_everything_without_a_panic() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);

    let report = trader.rebalance().await.unwrap();

    assert_eq!(
        report.decisions,
        vec![
            (DecisionId(1), sym(AAA), Action::Skip),
            (DecisionId(2), sym(BBB), Action::Skip),
        ]
    );
    assert_eq!(store.decisions().len(), 2);
    assert!(store.intents().is_empty());
    assert!(trader.broker().sent().is_empty());
    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 0);
}

/// The decisions of a trader that saw only the seven rising sessions, with no stray bars.
async fn clean_round() -> (Vec<Decision>, RebalanceReport) {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    let report = trader.rebalance().await.unwrap();
    (store.decisions(), report)
}

#[tokio::test]
async fn a4_duplicate_and_older_bars_are_refused_and_change_nothing() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    let all = sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0);
    for session in &all {
        feed(&mut trader, session).await;
    }
    let last = all.last().unwrap();
    let earlier = &all[all.len() - 2];

    // The same bar again, and an older one: both refused, and nothing else is refused.
    for stale in [&last[0], &earlier[0]] {
        assert!(matches!(
            trader.on_bar(stale),
            Err(EngineError::InvalidBar(_))
        ));
    }

    let report = trader.rebalance().await.unwrap();
    let (clean_decisions, clean_report) = clean_round().await;
    assert_eq!(report, clean_report);
    assert_eq!(store.decisions(), clean_decisions);
}

// ---- B. Broker disconnect ------------------------------------------------------------------

#[tokio::test]
async fn b1_reads_fail_during_reconciliation_and_nothing_is_decided() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader.broker().set(|f| f.fail_reads = true);

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::Broker(_))));
    assert!(store.decisions().is_empty());
    assert!(trader.broker().sent().is_empty());
}

#[tokio::test]
async fn b2_account_fails_after_reconciliation_and_nothing_is_decided() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader.broker().set(|f| f.fail_account = true);

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::Broker(_))));
    assert!(store.decisions().is_empty());
    assert!(sink.records().is_empty());
    assert!(trader.broker().sent().is_empty());
}

#[tokio::test]
async fn b3_unknown_outcome_with_the_order_at_the_broker_is_not_sent_twice() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    // The first submit (AAA) reaches the exchange, but its reply is lost.
    trader
        .broker()
        .set(|f| f.next_submit = Some(SubmitFault::UnknownSent));

    let first = trader.rebalance().await.unwrap();

    assert_eq!(first.orders_failed, 1);
    assert_eq!(first.orders_submitted, 1);
    // The write-ahead intent is stored for both orders.
    let intent_ids: Vec<String> = store
        .intents()
        .iter()
        .map(|i| i.client_order_id.as_str().to_owned())
        .collect();
    assert_eq!(intent_ids, vec!["nisse-1-0", "nisse-2-0"]);

    // No fill. The unresolved order is open at the broker, so reconciliation adopts it and it
    // counts as working: the second rebalance holds and sends nothing.
    let second = trader.rebalance().await.unwrap();

    assert_eq!(second.orders_submitted, 0);
    assert_eq!(second.orders_failed, 0);
    assert!(
        second
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Hold),
        "got {:?}",
        second.decisions
    );
    assert_eq!(sent_count(&trader, "nisse-1-0"), 1);
    assert_eq!(sent_count(&trader, "nisse-2-0"), 1);
    assert_eq!(trader.broker().submit_calls().len(), 2);
}

#[tokio::test]
async fn b3b_unknown_outcome_that_never_reached_the_broker_is_not_resent_within_the_recheck_window()
{
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader
        .broker()
        .set(|f| f.next_submit = Some(SubmitFault::UnknownNotSent));

    let first = trader.rebalance().await.unwrap();

    assert_eq!(first.orders_failed, 1);
    assert_eq!(first.orders_submitted, 1);
    assert_eq!(sent_count(&trader, "nisse-1-0"), 0);

    // The lookup by client order id finds nothing, so the order is judged never-arrived
    // (provisionally).
    let second = trader.rebalance().await.unwrap();

    // The provisional never-arrived verdict keeps the order counted as working until the recheck
    // window has passed, so AAA is held and nothing is re-sent. Nothing is live at the broker.
    assert_eq!(second.orders_failed, 0);
    assert_eq!(second.orders_submitted, 0);
    assert_eq!(sent_count(&trader, "nisse-1-0"), 0);
    assert_eq!(sent_count(&trader, "nisse-3-0"), 0);
    let aaa_decision = second
        .decisions
        .iter()
        .find(|(_, s, _)| *s == sym(AAA))
        .unwrap();
    assert_eq!(aaa_decision.2, Action::Hold);
    let open = trader.broker().open_orders().await.unwrap();
    assert!(open.orders.iter().all(|o| o.symbol != sym(AAA)));
}

#[tokio::test]
async fn b4_definite_rejection_is_counted_recorded_and_the_rest_go_out() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader
        .broker()
        .set(|f| f.next_submit = Some(SubmitFault::Rejected));

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_failed, 1);
    assert_eq!(report.orders_submitted, 1);
    assert_eq!(sent_count(&trader, "nisse-1-0"), 0);
    assert_eq!(sent_count(&trader, "nisse-2-0"), 1);
    let rejected = sink.records().iter().any(|r| {
        matches!(
            r,
            StoreRecord::Order { event, .. }
                if event.client_order_id == ClientOrderId::new("nisse-1-0")
                    && event.state == OrderState::Rejected
        )
    });
    assert!(rejected, "no rejected order row for nisse-1-0");
}

#[tokio::test]
async fn b5_intent_not_written_means_the_order_is_not_sent() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let intents = MemoryIntentLog::new(new_call_log());
    intents.fail_with("disk full");
    let mut trader = build(
        &store,
        &sink,
        intents.clone(),
        clock(),
        &[AAA, BBB],
        OrderBookState::default(),
    );
    feed_rising(&mut trader).await;

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::IntentNotRecorded { .. })));
    assert!(trader.broker().submit_calls().is_empty());
    assert!(trader.broker().sent().is_empty());
    assert!(
        trader
            .broker()
            .open_orders()
            .await
            .unwrap()
            .orders
            .is_empty()
    );
    assert!(intents.intents().is_empty());
}

#[tokio::test]
async fn b6_failing_record_sink_stops_the_round_before_any_order() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    // The equity peak is recorded while the sink still works; the risk rows are not.
    trader.observe_equity(d("100000")).await.unwrap();
    sink.fail_with("store gone");

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::Store(_))));
    assert!(trader.broker().submit_calls().is_empty());
    assert!(store.intents().is_empty());
}

#[tokio::test]
async fn b7_kill_switch_with_the_broker_down_still_blocks_every_order() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader.broker().set(|f| f.fail_cancel_all = true);

    let result = trader.engage_kill_switch("test").await;

    assert!(matches!(result, Err(EngineError::Broker(_))));
    assert!(trader.risk().state().kill_switch_engaged);

    // The broker is back.
    trader.broker().set(|f| f.fail_cancel_all = false);
    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(trader.broker().submit_calls().is_empty());
    for (id, _, _) in &report.decisions {
        assert!(
            failed_rules(&sink, *id).contains(&rule::KILL_SWITCH.to_owned()),
            "decision {id:?} not blocked by the kill switch"
        );
    }
}

#[tokio::test]
async fn b8_unreadable_kill_switch_source_blocks_the_next_round() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;

    assert!(trader.refresh_kill_switch(&Unreadable).await);
    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(trader.broker().submit_calls().is_empty());
    for (id, _, _) in &report.decisions {
        assert!(
            failed_rules(&sink, *id).contains(&rule::KILL_SWITCH.to_owned()),
            "decision {id:?} not blocked by the kill switch"
        );
    }
}

// ---- C. Drawdown paths ---------------------------------------------------------------------

#[tokio::test]
async fn c1_halved_exactly_at_ten_percent_with_peak_then_mode_events() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA]);

    assert_eq!(trader.observe_equity(d("100000")).await.unwrap(), None);
    assert_eq!(trader.observe_equity(d("95000")).await.unwrap(), None);
    assert_eq!(
        trader.observe_equity(d("90000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Halved))
    );

    let events = risk_events(&sink);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, RiskEventKind::EquityPeak);
    assert_eq!(events[0].detail["peak"].as_str(), Some("100000"));
    assert_eq!(events[1].kind, RiskEventKind::DrawdownMode);
    assert_eq!(events[1].detail["mode"].as_str(), Some("halved"));
}

/// Enters AAA and BBB at 100,000, lets the fills land, and returns the trader with both held.
async fn holding_both<'a>(
    store: &'a MemoryStore,
    sink: &MemorySink,
) -> Engine<'a, &'a MemoryStore, ManualClock> {
    let mut trader = engine(store, sink, clock(), &[AAA, BBB]);
    trader.broker().set(|f| f.equity = Some(d("100000")));
    feed_rising(&mut trader).await;
    trader.observe_equity(d("100000")).await.unwrap();
    trader.rebalance().await.unwrap();
    feed(
        &mut trader,
        &sessions(&[(AAA, &["109"]), (BBB, &["58"])], 7)[0],
    )
    .await;
    assert!(trader.state().positions.get(sym(AAA)).qty > Decimal::ZERO);
    assert!(trader.state().positions.get(sym(BBB)).qty > Decimal::ZERO);
    trader
}

#[tokio::test]
async fn c2_flat_sells_held_positions_and_buys_nothing() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = holding_both(&store, &sink).await;
    let held_aaa = trader.state().positions.get(sym(AAA)).qty;
    let held_bbb = trader.state().positions.get(sym(BBB)).qty;

    trader.observe_equity(d("90000")).await.unwrap();
    trader.observe_equity(d("85000")).await.unwrap();
    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
    trader.broker().set(|f| f.equity = Some(d("85000")));
    // Prices rise: a buy would be wanted if the mode allowed it.
    for session in sessions(&[(AAA, &["111", "113"]), (BBB, &["59", "60"])], 8) {
        feed(&mut trader, &session).await;
    }

    let before = store.intents().len();
    let report = trader.rebalance().await.unwrap();

    let round = &store.intents()[before..];
    assert!(!round.is_empty());
    assert!(round.iter().all(|i| i.side == OrderSide::Sell));
    let sold = |s: &str| {
        round
            .iter()
            .find(|i| i.symbol == sym(s))
            .map(|i| i.qty.as_decimal())
    };
    assert_eq!(sold(AAA), Some(held_aaa));
    assert_eq!(sold(BBB), Some(held_bbb));
    assert_eq!(report.orders_blocked, 0);
}

#[tokio::test]
async fn c3_recovery_to_a_new_high_does_not_leave_flat() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    trader.broker().set(|f| f.equity = Some(d("120000")));
    feed_rising(&mut trader).await;

    trader.observe_equity(d("100000")).await.unwrap();
    assert_eq!(
        trader.observe_equity(d("85000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );
    // A new high-water mark: the mode is latched and stays flat.
    assert_eq!(trader.observe_equity(d("120000")).await.unwrap(), None);
    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);

    let report = trader.rebalance().await.unwrap();

    // In flat mode the strategy's target is zero, so no buy is proposed and none reaches the gate.
    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 0);
    assert!(trader.broker().sent().is_empty());
    assert!(
        report
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Hold),
        "got {:?}",
        report.decisions
    );
    assert!(
        store
            .decisions()
            .iter()
            .all(|dec| dec.intended_qty.is_zero())
    );

    // The gate still refuses a buy while latched, asked directly with a fresh-account buy.
    let buy = OrderCheck {
        side: OrderSide::Buy,
        qty: d("10"),
        reference_px: Some(d("100")),
        var_daily: Some(d("0.0001")),
        sellable_qty: Decimal::ZERO,
        equity: d("120000"),
        gross_exposure: Some(Decimal::ZERO),
        data_age_nanos: Some(0),
        account_blocked: false,
        would_day_trade: false,
        day_trades_in_window: 0,
    };
    let verdict = trader.risk().check(&buy);
    assert!(!verdict.approved());
    let failed: Vec<&str> = verdict
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.rule)
        .collect();
    assert!(failed.contains(&rule::DRAWDOWN_FLAT), "{failed:?}");
    assert!(failed.contains(&rule::KELLY_EXPOSURE), "{failed:?}");
}

#[tokio::test]
async fn c4_halved_then_recovery_above_ninety_five_percent_sizes_in_full_again() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    trader.broker().set(|f| f.equity = Some(d("90000")));
    trader.observe_equity(d("100000")).await.unwrap();
    assert_eq!(
        trader.observe_equity(d("90000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Halved))
    );
    // Three bars: too little history, so this round decides Skip and sends nothing.
    for session in sessions(&[(AAA, &RISING_A[..3]), (BBB, &RISING_B[..3])], 0) {
        feed(&mut trader, &session).await;
    }
    let halved = trader.rebalance().await.unwrap();
    assert_eq!(halved.orders_submitted, 0);
    assert_eq!(store.decisions()[0].features[feature::RISK_SCALE], d("0.5"));

    // Back above 95% of the peak: normal again.
    trader.broker().set(|f| f.equity = Some(d("96000")));
    assert_eq!(
        trader.observe_equity(d("96000")).await.unwrap(),
        Some((DrawdownMode::Halved, DrawdownMode::Normal))
    );
    for session in sessions(&[(AAA, &RISING_A[3..]), (BBB, &RISING_B[3..])], 3) {
        feed(&mut trader, &session).await;
    }
    let full = trader.rebalance().await.unwrap();
    let decisions = store.decisions();
    let recovered = &decisions[2..];
    assert!(
        recovered
            .iter()
            .all(|dec| dec.features[feature::RISK_SCALE] == Decimal::ONE)
    );

    // The same book at 96,000 with no halving history sizes the same.
    let reference_store = MemoryStore::new();
    let reference_sink = MemorySink::new();
    let mut reference = engine(&reference_store, &reference_sink, clock(), &[AAA, BBB]);
    reference.broker().set(|f| f.equity = Some(d("96000")));
    reference.observe_equity(d("96000")).await.unwrap();
    feed_rising(&mut reference).await;
    reference.rebalance().await.unwrap();
    let expected = reference_store.decisions();
    assert_eq!(full.orders_submitted, 2);
    assert_eq!(recovered[0].intended_qty, expected[0].intended_qty);
    assert_eq!(recovered[1].intended_qty, expected[1].intended_qty);
}

#[tokio::test]
async fn c5_non_positive_equity_from_the_broker_is_flat_and_buys_nothing() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut direct = engine(&store, &sink, clock(), &[AAA]);
    assert_eq!(
        direct.observe_equity(d("0")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );

    for equity in ["0", "-1000"] {
        let store = MemoryStore::new();
        let sink = MemorySink::new();
        let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
        feed_rising(&mut trader).await;
        trader.broker().set(|f| f.equity = Some(d(equity)));

        let report = trader.rebalance().await.unwrap();

        assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
        assert!(trader.broker().sent().is_empty(), "equity {equity}");
        assert_eq!(report.orders_submitted, 0, "equity {equity}");
        for (id, _, action) in &report.decisions {
            if *action == Action::Enter {
                assert!(
                    failed_rules(&sink, *id).contains(&rule::DRAWDOWN_FLAT.to_owned()),
                    "equity {equity}"
                );
            }
        }
    }
}

#[tokio::test]
async fn c6_a_gap_from_peak_to_flat_in_one_observation() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA]);

    trader.observe_equity(d("100000")).await.unwrap();
    assert_eq!(
        trader.observe_equity(d("80000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Flat))
    );

    let events = risk_events(&sink);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, RiskEventKind::DrawdownMode);
    assert_eq!(events[1].detail["mode"].as_str(), Some("flat"));
    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
}

// ---- D. Duplicate and out-of-order fills ---------------------------------------------------

/// A trader with `order` already tracked as sent, and its own sink.
fn tracking<'a>(
    store: &'a MemoryStore,
    sink: &MemorySink,
    order: OrderIntent,
) -> Engine<'a, &'a MemoryStore, ManualClock> {
    let mut state = OrderBookState::default();
    state.tracker.track_intent(order).unwrap();
    build(store, sink, store, clock(), &[AAA], state)
}

#[tokio::test]
async fn d1_the_same_fill_twice_is_recorded_once_and_counted_once() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let order = buy_of("nisse-1-0", "10");
    let mut trader = tracking(&store, &sink, order);
    let update = fill_update(&order, OrderState::Filled, "10", "10", "trade-1", 2_000);

    trader.on_order_update(&update).await.unwrap();
    trader.on_order_update(&update).await.unwrap();

    assert_eq!(fill_rows(&sink), 1);
    assert_eq!(trader.state().positions.get(sym(AAA)).qty, d("10"));
}

#[tokio::test]
async fn d2_replayed_and_out_of_order_partials_change_nothing() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let order = buy_of("nisse-1-0", "10");
    let mut trader = tracking(&store, &sink, order);
    let partial = fill_update(
        &order,
        OrderState::PartiallyFilled,
        "6",
        "6",
        "trade-1",
        2_000,
    );
    let rest = fill_update(&order, OrderState::Filled, "10", "4", "trade-2", 3_000);
    trader.on_order_update(&partial).await.unwrap();
    trader.on_order_update(&rest).await.unwrap();
    assert_eq!(fill_rows(&sink), 2);

    // The completed order's last message, replayed.
    trader.on_order_update(&rest).await.unwrap();
    // A late partial: its cumulative quantity is below what is already recorded.
    trader.on_order_update(&partial).await.unwrap();

    assert_eq!(fill_rows(&sink), 2);
    assert_eq!(trader.state().positions.get(sym(AAA)).qty, d("10"));
    let tracked = trader.state().tracker.get(order.client_order_id).unwrap();
    assert_eq!(tracked.state, OrderState::Filled);
    assert_eq!(tracked.cum_filled_qty.as_decimal(), d("10"));
}

#[tokio::test]
async fn d3_a_fill_for_an_order_never_sent_is_ignored() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA]);
    let stranger = buy_of("someone-else-1", "10");

    trader
        .on_order_update(&fill_update(
            &stranger,
            OrderState::Filled,
            "10",
            "10",
            "trade-x",
            2_000,
        ))
        .await
        .unwrap();

    assert!(sink.records().is_empty());
    assert!(trader.state().positions.get(sym(AAA)).qty.is_zero());
}

#[tokio::test]
async fn d4_a_fill_found_by_reconciliation_is_not_counted_again_from_the_stream() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let mut trader = engine(&store, &sink, clock(), &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader.rebalance().await.unwrap();

    // Day 7: the orders fill at the broker. The stream's messages are held back.
    let day7 = sessions(&[(AAA, &["109"]), (BBB, &["58"])], 7);
    trader.broker().on_session(&day7[0]).unwrap();
    let held_back = trader.broker().drain_updates();
    for bar in &day7[0] {
        trader.on_bar(bar).unwrap();
    }
    // Reconciliation finds the fills first.
    trader.rebalance().await.unwrap();
    // Then the stream delivers the same messages, late.
    for update in &held_back {
        trader.on_order_update(update).await.unwrap();
    }

    let fills_for = |id: &str| {
        sink.records()
            .iter()
            .filter(|r| {
                matches!(r, StoreRecord::Order { event, fill: Some(_) }
                    if event.client_order_id == ClientOrderId::new(id))
            })
            .count()
    };
    assert_eq!(fills_for("nisse-1-0"), 1);
    assert_eq!(fills_for("nisse-2-0"), 1);
    let broker_positions = trader.broker().positions().await.unwrap();
    for position in broker_positions {
        assert_eq!(
            trader.state().positions.get(position.symbol).qty,
            position.qty.as_decimal()
        );
    }
}

#[tokio::test]
async fn d5_a_duplicated_buy_fill_is_not_a_day_trade() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let order = buy_of("nisse-1-0", "10");
    let mut trader = tracking(&store, &sink, order);
    let update = fill_update(&order, OrderState::Filled, "10", "10", "trade-1", 1_000);

    trader.on_order_update(&update).await.unwrap();
    trader.on_order_update(&update).await.unwrap();

    assert_eq!(
        trader
            .risk()
            .day_trades()
            .count_in_window(UnixNanos::from(1_000_u64)),
        0
    );
}

// ---- E. Combined ---------------------------------------------------------------------------

#[tokio::test]
async fn e_kill_switch_stale_data_and_blocked_account_together() {
    let store = MemoryStore::new();
    let sink = MemorySink::new();
    let clk = clock();
    let mut trader = engine(&store, &sink, &clk, &[AAA, BBB]);
    feed_rising(&mut trader).await;
    trader.broker().set(|f| f.blocked = true);
    trader.set_kill_switch(true);
    clk.set(UnixNanos::from(11 * DAY_NANOS));

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(trader.broker().submit_calls().is_empty());
    for (id, _, action) in &report.decisions {
        assert_eq!(*action, Action::Enter);
        assert_eq!(risk_rows(&sink, *id).len(), 8);
        let failed = failed_rules(&sink, *id);
        for expected in [rule::KILL_SWITCH, rule::ACCOUNT_BLOCKED, rule::STALE_DATA] {
            assert!(failed.contains(&expected.to_owned()), "{failed:?}");
        }
    }
}
