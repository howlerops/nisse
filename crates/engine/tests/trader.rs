//! Acceptance tests for the trading loop (phase 3, EN1): bars in, decisions and orders out.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

mod support;

use std::collections::BTreeMap;
use std::sync::Mutex;

use nisse_core::{
    AccountSnapshot, Action, Broker, BrokerOrder, BrokerPosition, DecisionId, ManualClock,
    OpenOrders, OrderIntent, ReplaceRequest, StoreRecord, StrategyVersionId, SubmitError,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId,
        InstrumentId, OrderSide, OrderType, Price, PriceType, Quantity, Symbol, TimeInForce,
        UnixNanos, Venue, VenueOrderId,
    },
};
use nisse_engine::strategies::{TrendParams, TrendVolTarget, feature};
use nisse_engine::{
    CostModel, EngineError, MemoryStore, OrderBookState, OrderGateway, RebalanceReport, SimBroker,
    SimError, Strategy, SyncOptions, Trader, TraderConfig,
};
use rust_decimal::Decimal;
use support::{Call, FakeBroker, FakeError, calls, intent, new_call_log};

const AAA: &str = "AAA";
const BBB: &str = "BBB";
const ZZZ: &str = "ZZZ";
const DAY_NANOS: u64 = 86_400 * 1_000_000_000;

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

/// The probe records, at every submit, how many intents the store already held. The
/// gateway writes the intent first, so each submit must see its own intent counted.
struct Probe<'a> {
    inner: SimBroker,
    store: &'a MemoryStore,
    seen: Mutex<Vec<usize>>,
}

impl Probe<'_> {
    fn on_session(&self, bars: &[Bar]) -> Result<(), EngineError> {
        self.inner.on_session(bars)
    }

    fn drain_updates(&self) -> Vec<nisse_core::OrderUpdate> {
        self.inner.drain_updates()
    }

    /// Intents in the store at each submit, in submit order.
    fn seen(&self) -> Vec<usize> {
        self.seen.lock().unwrap().clone()
    }
}

impl Broker for Probe<'_> {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        let held = self.store.intents().len();
        self.seen.lock().unwrap().push(held);
        self.inner.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
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

type Engine<'a> = Trader<
    TrendVolTarget,
    Probe<'a>,
    &'a MemoryStore,
    &'a MemoryStore,
    &'a MemoryStore,
    ManualClock,
>;

fn config(symbols: Vec<Symbol>) -> TraderConfig {
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
    }
}

fn clock() -> ManualClock {
    ManualClock::new(UnixNanos::from(1_000_u64))
}

/// A trader over the given universe (sorted here), with a fresh simulated account of 100,000.
fn engine<'a>(store: &'a MemoryStore, symbols: &[&str]) -> Engine<'a> {
    let mut sorted: Vec<Symbol> = symbols.iter().map(|s| sym(s)).collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let probe = Probe {
        inner: SimBroker::new(
            Decimal::from(100_000),
            CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
        ),
        store,
        seen: Mutex::new(Vec::new()),
    };
    Trader::new(
        strategy(),
        OrderGateway::new(probe, store),
        store,
        store,
        clock(),
        config(sorted),
        OrderBookState::default(),
    )
    .unwrap()
}

/// What the backtest loop does for one session: fills, then the updates, then the bars.
async fn feed(trader: &mut Engine<'_>, session: &[Bar]) {
    trader.broker().on_session(session).unwrap();
    let updates = trader.broker().drain_updates();
    for update in &updates {
        trader.on_order_update(update).await.unwrap();
    }
    for bar in session {
        trader.on_bar(bar).unwrap();
    }
}

/// Seven sessions of rising closes for AAA and BBB, then the first rebalance (which enters
/// both).
async fn enter_both(store: &MemoryStore) -> (Engine<'_>, RebalanceReport) {
    let mut trader = engine(store, &[AAA, BBB]);
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        feed(&mut trader, &session).await;
    }
    let report = trader.rebalance().await.unwrap();
    (trader, report)
}

#[test]
fn new_rejects_an_empty_universe() {
    let store = MemoryStore::new();
    let result = Trader::new(
        strategy(),
        OrderGateway::new(
            SimBroker::new(
                Decimal::from(1),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            &store,
        ),
        &store,
        &store,
        clock(),
        config(Vec::new()),
        OrderBookState::default(),
    );
    assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
}

#[test]
fn new_rejects_an_unsorted_universe() {
    let store = MemoryStore::new();
    let result = Trader::new(
        strategy(),
        OrderGateway::new(
            SimBroker::new(
                Decimal::from(1),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            &store,
        ),
        &store,
        &store,
        clock(),
        config(vec![sym(BBB), sym(AAA)]),
        OrderBookState::default(),
    );
    assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
}

#[test]
fn new_rejects_a_duplicated_universe() {
    let store = MemoryStore::new();
    let result = Trader::new(
        strategy(),
        OrderGateway::new(
            SimBroker::new(
                Decimal::from(1),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            &store,
        ),
        &store,
        &store,
        clock(),
        config(vec![sym(AAA), sym(AAA)]),
        OrderBookState::default(),
    );
    assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
}

#[tokio::test]
async fn on_bar_ignores_a_symbol_outside_the_universe() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB]);
    for session in sessions(&[(ZZZ, &RISING_A)], 0) {
        for bar in &session {
            trader.on_bar(bar).unwrap();
        }
    }
    let report = trader.rebalance().await.unwrap();
    assert!(
        report
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Skip)
    );
    assert_eq!(report.decisions.len(), 2);
    assert_eq!(report.orders_submitted, 0);
    assert!(store.intents().is_empty());
}

#[test]
fn on_bar_rejects_a_non_positive_close_for_a_universe_symbol() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA]);
    let result = trader.on_bar(&bar(AAA, 0, "0", "0"));
    assert!(matches!(result, Err(EngineError::InvalidBar(_))));
}

#[tokio::test]
async fn rebalance_before_enough_history_skips_every_symbol() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB]);
    for session in sessions(&[(AAA, &RISING_A[..3]), (BBB, &RISING_B[..3])], 0) {
        feed(&mut trader, &session).await;
    }
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
    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_failed, 0);
}

#[tokio::test]
async fn rising_history_enters_both_symbols_from_one_equity() {
    let store = MemoryStore::new();
    let (trader, report) = enter_both(&store).await;

    assert_eq!(
        report.decisions,
        vec![
            (DecisionId(1), sym(AAA), Action::Enter),
            (DecisionId(2), sym(BBB), Action::Enter),
        ]
    );
    assert_eq!(report.orders_submitted, 2);
    assert_eq!(report.orders_failed, 0);

    // Seeds count from the configured base, one per decision.
    let decisions = store.decisions();
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].rng_seed, 100);
    assert_eq!(decisions[1].rng_seed, 101);

    // Both sized from the same equity, and each quantity is the strategy's own answer.
    let strat = strategy();
    assert_eq!(
        decisions[0].features[feature::EQUITY],
        decisions[1].features[feature::EQUITY]
    );
    for decision in &decisions {
        let expected = strat
            .decide(&decision.features, decision.rng_seed)
            .intended_qty;
        assert_eq!(decision.intended_qty, expected);
        assert!(decision.intended_qty > Decimal::ZERO);
    }

    // Intents reached the store before the broker saw them.
    let intents = store.intents();
    assert_eq!(intents.len(), 2);
    assert_eq!(trader.broker().seen(), vec![1, 2]);
    assert_eq!(intents[0].client_order_id.as_str(), "nisse-1-0");
    assert_eq!(intents[1].client_order_id.as_str(), "nisse-2-0");
    for (intent, decision) in intents.iter().zip(&decisions) {
        assert_eq!(intent.side, OrderSide::Buy);
        assert_eq!(intent.order_type, OrderType::Market);
        assert_eq!(intent.time_in_force, TimeInForce::Day);
        assert_eq!(intent.limit_px, None);
        assert_eq!(intent.qty.as_decimal(), decision.intended_qty);
        assert_eq!(intent.symbol, decision.symbol);
    }

    let open = trader.broker().open_orders().await.unwrap();
    assert_eq!(open.orders.len(), 2);
}

#[tokio::test]
async fn filled_orders_match_the_broker_positions() {
    let store = MemoryStore::new();
    let (mut trader, _) = enter_both(&store).await;
    // Session 7: the orders fill at its open.
    for session in sessions(&[(AAA, &["109"]), (BBB, &["58"])], 7) {
        feed(&mut trader, &session).await;
    }

    let broker_positions: BTreeMap<Symbol, Decimal> = trader
        .broker()
        .positions()
        .await
        .unwrap()
        .iter()
        .map(|p| (p.symbol, p.qty.as_decimal()))
        .collect();
    assert_eq!(broker_positions.len(), 2);
    assert_eq!(trader.state().positions.quantities(), broker_positions);

    let records = store.records();
    let fills = records
        .iter()
        .filter(|r| matches!(r, StoreRecord::Order { fill: Some(_), .. }))
        .count();
    let positions = records
        .iter()
        .filter(|r| matches!(r, StoreRecord::Position(_)))
        .count();
    assert_eq!(fills, 2);
    assert_eq!(positions, 2);
}

#[tokio::test]
async fn second_rebalance_with_unchanged_prices_holds() {
    let store = MemoryStore::new();
    let (mut trader, _) = enter_both(&store).await;
    for session in sessions(&[(AAA, &["109"]), (BBB, &["58"])], 7) {
        feed(&mut trader, &session).await;
    }
    let intents_before = store.intents().len();

    let report = trader.rebalance().await.unwrap();

    assert!(
        report
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Hold)
    );
    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_failed, 0);
    assert_eq!(store.intents().len(), intents_before);
    let seeds: Vec<u64> = store.decisions()[2..].iter().map(|d| d.rng_seed).collect();
    assert_eq!(seeds, vec![102, 103]);
}

#[tokio::test]
async fn falling_asset_is_exited_and_flat_after_fill() {
    let store = MemoryStore::new();
    let (mut trader, _) = enter_both(&store).await;
    for session in sessions(&[(AAA, &["109"]), (BBB, &["58"])], 7) {
        feed(&mut trader, &session).await;
    }
    // AAA falls for long enough that its momentum is negative; BBB keeps rising.
    let falling = ["108", "106", "104", "102", "100", "98"];
    let rising = ["59", "60", "61", "62", "63", "64"];
    for session in sessions(&[(AAA, &falling), (BBB, &rising)], 8) {
        feed(&mut trader, &session).await;
    }

    let before = store.intents().len();
    let report = trader.rebalance().await.unwrap();

    let aaa_position = trader.state().positions.get(sym(AAA)).qty;
    assert!(aaa_position > Decimal::ZERO);
    let aaa_decision = store
        .decisions()
        .into_iter()
        .rev()
        .find(|d| d.symbol == sym(AAA))
        .unwrap();
    assert_eq!(aaa_decision.action, Action::Exit);
    assert_eq!(aaa_decision.intended_qty, -aaa_position);
    // The exit is the first order of the round: sells go before buys.
    let round = &store.intents()[before..];
    assert_eq!(round[0].symbol, sym(AAA));
    assert_eq!(round[0].side, OrderSide::Sell);
    assert_eq!(round[0].qty.as_decimal(), aaa_position);
    assert_eq!(report.orders_submitted, u32::try_from(round.len()).unwrap());

    feed(
        &mut trader,
        &sessions(&[(AAA, &["97"]), (BBB, &["65"])], 14)[0],
    )
    .await;
    assert_eq!(trader.state().positions.get(sym(AAA)).qty, Decimal::ZERO);
    let broker_aaa = trader
        .broker()
        .positions()
        .await
        .unwrap()
        .iter()
        .any(|p| p.symbol == sym(AAA));
    assert!(!broker_aaa);
}

#[tokio::test]
async fn sells_are_submitted_before_buys() {
    let store = MemoryStore::new();
    // ZZZ (sorts last) is bought first; AAA (sorts first) is bought later, when ZZZ falls.
    let mut trader = engine(&store, &[AAA, ZZZ]);
    let falling_a = ["60", "59", "58", "57", "56", "55", "54"];
    for session in sessions(&[(AAA, &falling_a), (ZZZ, &RISING_B)], 0) {
        feed(&mut trader, &session).await;
    }
    trader.rebalance().await.unwrap();
    feed(
        &mut trader,
        &sessions(&[(AAA, &["54"]), (ZZZ, &["58"])], 7)[0],
    )
    .await;
    let zzz_falls = ["57", "56", "55", "54", "53", "52"];
    let aaa_rises = ["55", "57", "59", "61", "63", "65"];
    for session in sessions(&[(AAA, &aaa_rises), (ZZZ, &zzz_falls)], 8) {
        feed(&mut trader, &session).await;
    }
    let before = store.intents().len();

    let report = trader.rebalance().await.unwrap();

    let actions: Vec<Action> = report.decisions.iter().map(|(_, _, a)| *a).collect();
    assert_eq!(actions, vec![Action::Enter, Action::Exit]);
    let intents = store.intents();
    let round = &intents[before..];
    assert_eq!(round.len(), 2);
    assert_eq!(round[0].symbol, sym(ZZZ));
    assert_eq!(round[0].side, OrderSide::Sell);
    assert_eq!(round[1].symbol, sym(AAA));
    assert_eq!(round[1].side, OrderSide::Buy);
}

#[tokio::test]
async fn rejected_order_is_counted_and_the_rest_go_out() {
    let store = MemoryStore::new();
    let log = new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    broker.push_submit_result(Err(SubmitError::Rejected(FakeError(
        "insufficient buying power".to_owned(),
    ))));
    let mut trader = Trader::new(
        strategy(),
        OrderGateway::new(broker.clone(), &store),
        &store,
        &store,
        clock(),
        config(vec![sym(AAA), sym(BBB)]),
        OrderBookState::default(),
    )
    .unwrap();
    for day in 0..RISING_A.len() {
        trader
            .on_bar(&bar(AAA, day as u64, RISING_A[day], RISING_A[day]))
            .unwrap();
        trader
            .on_bar(&bar(BBB, day as u64, RISING_B[day], RISING_B[day]))
            .unwrap();
    }

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_failed, 1);
    assert_eq!(report.orders_submitted, 1);
    let submitted: Vec<Call> = calls(&log)
        .into_iter()
        .filter(|c| matches!(c, Call::Submit(_)))
        .collect();
    assert_eq!(
        submitted,
        vec![
            Call::Submit(ClientOrderId::new("nisse-1-0")),
            Call::Submit(ClientOrderId::new("nisse-2-0")),
        ]
    );
}

#[tokio::test]
async fn unresolved_order_blocks_the_rebalance() {
    let store = MemoryStore::new();
    let broker = FakeBroker::new(new_call_log());
    let mut state = OrderBookState::default();
    state
        .tracker
        .track_intent(intent("nisse-0-0", "1", None))
        .unwrap();
    broker.fail_lookups("timeout");
    let mut trader = Trader::new(
        strategy(),
        OrderGateway::new(broker.clone(), &store),
        &store,
        &store,
        clock(),
        config(vec![sym(AAA), sym(BBB)]),
        state,
    )
    .unwrap();

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::NotReconciled(1))));
    assert!(store.decisions().is_empty());
}
