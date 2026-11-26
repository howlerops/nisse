//! Acceptance tests for the trading loop (phase 3, EN1): bars in, decisions and orders out.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

mod support;

use std::collections::BTreeMap;
use std::sync::Mutex;

use nisse_core::{
    AccountSnapshot, Action, Broker, BrokerOrder, BrokerPosition, Clock, Decision, DecisionId,
    FillDetail, KillSwitchSource, ManualClock, OpenOrders, OrderIntent, OrderState, OrderUpdate,
    ReplaceRequest, RiskCheck, RiskEvent, RiskEventKind, StoreRecord, StrategyVersionId,
    SubmitError,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId,
        InstrumentId, OrderSide, OrderType, Price, PriceType, Quantity, Symbol, TimeInForce,
        TradeId, UnixNanos, Venue, VenueOrderId,
    },
};
use nisse_engine::strategies::{TrendParams, TrendVolTarget, feature};
use nisse_engine::{
    CostModel, EngineError, MemoryStore, OrderBookState, OrderGateway, RebalanceReport, SimBroker,
    SimError, Strategy, SyncOptions, Trader, TraderConfig,
};
use nisse_risk::{DrawdownMode, rule};
use rust_decimal::Decimal;
use support::{
    Call, FakeBroker, FakeError, MemorySink, accepted_from, calls, intent, new_call_log, snapshot,
};

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

type Engine<'a, C = ManualClock> =
    Trader<TrendVolTarget, Probe<'a>, &'a MemoryStore, &'a MemoryStore, &'a MemoryStore, C>;

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

/// A clock just after the first week of test bars, for tests that feed bars without `feed`.
fn after_the_bars() -> ManualClock {
    ManualClock::new(UnixNanos::from(9 * 86_400_000_000_000_u64))
}

/// The clock starts before the first bar; `feed` moves it to each session's close.
fn clock() -> ManualClock {
    ManualClock::new(UnixNanos::from(1_000_u64))
}

/// A trader over the given universe (sorted here), with a fresh simulated account of `cash`
/// and the given clock.
fn engine_with<'a, C: Clock>(
    store: &'a MemoryStore,
    clock: C,
    symbols: &[&str],
    cash: i64,
) -> Engine<'a, C> {
    let mut sorted: Vec<Symbol> = symbols.iter().map(|s| sym(s)).collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let probe = Probe {
        inner: SimBroker::new(
            Decimal::from(cash),
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
        clock,
        config(sorted),
        OrderBookState::default(),
    )
    .unwrap()
}

/// A trader over the given universe (sorted here), with a fresh simulated account of 100,000.
fn engine<'a>(store: &'a MemoryStore, symbols: &[&str]) -> Engine<'a> {
    engine_with(store, clock(), symbols, 100_000)
}

/// A test clock that can be moved through a shared reference.
trait SetClock {
    fn set_to(&self, now: UnixNanos);
}

impl SetClock for ManualClock {
    fn set_to(&self, now: UnixNanos) {
        self.set(now);
    }
}

impl SetClock for &ManualClock {
    fn set_to(&self, now: UnixNanos) {
        self.set(now);
    }
}

/// What the backtest loop does for one session: fills, then the updates, then the bars.
///
/// The clock moves to the session's close first, as it does in a backtest and in life: a bar
/// stamped after the clock has no known age, and the gate would block buys as stale.
async fn feed<C: Clock + SetClock>(trader: &mut Engine<'_, C>, session: &[Bar]) {
    if let Some(bar) = session.first()
        && bar.ts_event > trader.clock().now()
    {
        trader.clock().set_to(bar.ts_event);
    }
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
        after_the_bars(),
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

#[tokio::test]
async fn a_repeated_rebalance_before_the_fills_orders_nothing_twice() {
    let store = MemoryStore::new();
    let (mut trader, first) = enter_both(&store).await;
    assert_eq!(first.orders_submitted, 2);
    let intents_before = store.intents().len();

    // No session in between: both orders are still queued for the next open.
    let second = trader.rebalance().await.unwrap();

    assert!(
        second
            .decisions
            .iter()
            .all(|(_, _, action)| *action == Action::Hold),
        "got {:?}",
        second.decisions
    );
    assert_eq!(second.orders_submitted, 0);
    assert_eq!(store.intents().len(), intents_before);
    // The stored position feature is what was held plus what was on order.
    let first_qty = store.decisions()[0].intended_qty;
    let repeated = &store.decisions()[2];
    assert_eq!(
        repeated.features.get("position_qty").copied(),
        Some(first_qty)
    );
}

#[tokio::test]
async fn a_bar_that_is_not_newer_is_rejected_and_changes_nothing() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB]);
    let all = sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0);
    for session in &all {
        feed(&mut trader, session).await;
    }
    let last = all.last().unwrap();
    let earlier = &all[all.len() - 2];

    // The same bar again, and an older one: both refused.
    for stale in [&last[0], &earlier[0]] {
        assert!(matches!(
            trader.on_bar(stale),
            Err(EngineError::InvalidBar(_))
        ));
    }

    // The decisions are the ones a trader that never saw the stale bars makes.
    let report = trader.rebalance().await.unwrap();
    let clean_store = MemoryStore::new();
    let (_, clean_report) = enter_both(&clean_store).await;
    assert_eq!(report, clean_report);
    assert_eq!(store.decisions(), clean_store.decisions());
}

#[tokio::test]
async fn orders_the_engine_did_not_send_block_the_rebalance() {
    // An open order at the broker that the tracker does not know.
    let store = MemoryStore::new();
    let broker = FakeBroker::new(new_call_log());
    let stranger = intent("someone-else-1", "5", None);
    broker.set_open_orders(vec![accepted_from(&stranger)]);
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
    assert!(matches!(
        trader.rebalance().await,
        Err(EngineError::ForeignOrders(1))
    ));
    assert!(store.decisions().is_empty());

    // Open orders of a kind the engine never sends.
    let store = MemoryStore::new();
    let broker = FakeBroker::new(new_call_log());
    broker.set_unrepresentable(2);
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
    assert!(matches!(
        trader.rebalance().await,
        Err(EngineError::ForeignOrders(2))
    ));
    assert!(store.decisions().is_empty());
}

#[tokio::test]
async fn a_symbol_whose_bars_lag_the_universe_is_skipped() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB]);
    let all = sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0);
    let (last, before) = all.split_last().unwrap();
    for session in before {
        feed(&mut trader, session).await;
    }
    // The last session arrives for AAA only: BBB's newest close is a day old.
    let only_aaa: Vec<Bar> = last
        .iter()
        .filter(|bar| bar.bar_type.instrument_id().symbol == sym(AAA))
        .copied()
        .collect();
    feed(&mut trader, &only_aaa).await;

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.decisions[0].2, Action::Enter);
    assert_eq!(report.decisions[1].2, Action::Skip);
    assert_eq!(report.orders_submitted, 1);
    let skipped = &store.decisions()[1];
    assert!(!skipped.features.contains_key(feature::CLOSE));
    assert!(skipped.features.contains_key(feature::POSITION_QTY));
}

// ---- The risk gate: every order is checked, recorded and obeyed ----------------------------

const HOUR_NANOS: u64 = 3_600 * 1_000_000_000;
/// Closes that are nearly flat: a tiny daily variance, so each buy is capped at `max_weight`.
const LOW: [&str; 7] = [
    "100.0", "100.1", "100.3", "100.4", "100.6", "100.7", "100.9",
];
const FLAT: [&str; 7] = ["50", "50", "50", "50", "50", "50", "50"];
const NINE: [&str; 9] = ["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9"];

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// A broker wrapper for the gate tests: forwards to a `FakeBroker`, can refuse `cancel_all`,
/// and can report the account as blocked.
#[derive(Debug, Clone)]
struct Gated {
    inner: FakeBroker,
    fail_cancel: bool,
    blocked: bool,
}

fn gated(broker: &FakeBroker) -> Gated {
    Gated {
        inner: broker.clone(),
        fail_cancel: false,
        blocked: false,
    }
}

impl Broker for Gated {
    type Error = FakeError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<FakeError>> {
        self.inner.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), FakeError> {
        self.inner.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), FakeError> {
        if self.fail_cancel {
            return Err(FakeError("cancel refused".to_owned()));
        }
        self.inner.cancel_all().await
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<FakeError>> {
        self.inner.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, FakeError> {
        self.inner.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, FakeError> {
        self.inner.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, FakeError> {
        self.inner.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, FakeError> {
        let mut account = self.inner.account().await?;
        account.trading_blocked = self.blocked;
        Ok(account)
    }
}

type GatedTrader<'a> =
    Trader<TrendVolTarget, Gated, &'a MemoryStore, &'a MemoryStore, MemorySink, ManualClock>;

/// A trader over `Gated` whose records go to `sink`, with the given state.
fn gated_trader<'a>(
    store: &'a MemoryStore,
    sink: MemorySink,
    broker: Gated,
    symbols: &[&str],
    state: OrderBookState,
) -> GatedTrader<'a> {
    let mut sorted: Vec<Symbol> = symbols.iter().map(|s| sym(s)).collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Trader::new(
        strategy(),
        OrderGateway::new(broker, store),
        store,
        sink,
        after_the_bars(),
        config(sorted),
        state,
    )
    .unwrap()
}

/// The risk rows for one decision in a record stream, in emit order.
fn rows_in(records: &[StoreRecord], id: DecisionId) -> Vec<RiskCheck> {
    records
        .iter()
        .filter_map(|record| match record {
            StoreRecord::Risk(check) if check.decision_id == id => Some(check.clone()),
            _ => None,
        })
        .collect()
}

/// The risk rows the store holds for one decision, in emit order.
fn risk_rows(store: &MemoryStore, id: DecisionId) -> Vec<RiskCheck> {
    rows_in(&store.records(), id)
}

/// The risk events in a record stream, in emit order.
fn risk_events(records: &[StoreRecord]) -> Vec<RiskEvent> {
    records
        .iter()
        .filter_map(|record| match record {
            StoreRecord::RiskEvent(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

/// A source that reads `Some(engaged)`, or fails when `None`.
struct Source(Option<bool>);

impl KillSwitchSource for Source {
    type Error = FakeError;

    async fn kill_switch_engaged(&self) -> Result<bool, FakeError> {
        self.0
            .ok_or_else(|| FakeError("kill switch unreadable".to_owned()))
    }
}

#[tokio::test]
async fn an_approved_order_has_eight_checks_before_its_first_order_record() {
    let store = MemoryStore::new();
    let (_trader, report) = enter_both(&store).await;
    assert_eq!(report.orders_submitted, 2);
    assert_eq!(report.orders_blocked, 0);

    let id = DecisionId(1);
    let records = store.records();
    let checks: Vec<(usize, RiskCheck)> = records
        .iter()
        .enumerate()
        .filter_map(|(i, record)| match record {
            StoreRecord::Risk(check) if check.decision_id == id => Some((i, check.clone())),
            _ => None,
        })
        .collect();
    let rules: Vec<&str> = checks.iter().map(|(_, c)| c.rule.as_str()).collect();
    assert_eq!(rules, rule::ALL);
    assert!(checks.iter().all(|(_, c)| c.passed));

    // The order's own row (written after the submit) comes after all eight checks.
    let first_order = records
        .iter()
        .position(|r| matches!(r, StoreRecord::Order { event, .. } if event.decision_id == id))
        .unwrap();
    assert!(checks.last().unwrap().0 < first_order);
}

#[tokio::test]
async fn an_engaged_kill_switch_blocks_every_order_and_records_why() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB]);
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        feed(&mut trader, &session).await;
    }
    trader.set_kill_switch(true);

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.decisions.len(), 2);
    assert_eq!(report.orders_submitted, 0);
    let non_zero = store
        .decisions()
        .iter()
        .filter(|d| !d.intended_qty.is_zero())
        .count();
    assert_eq!(non_zero, 2);
    assert_eq!(report.orders_blocked as usize, non_zero);
    assert!(store.intents().is_empty());
    assert_eq!(store.decisions().len(), 2);
    for (id, _, _) in &report.decisions {
        let rows = risk_rows(&store, *id);
        assert_eq!(rows.len(), 8);
        let kill = rows.iter().find(|r| r.rule == rule::KILL_SWITCH).unwrap();
        assert!(!kill.passed);
    }
}

#[tokio::test]
async fn engaging_the_kill_switch_cancels_open_orders_and_records_it() {
    let store = MemoryStore::new();
    let log = new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    let sink = MemorySink::new();
    let mut trader = gated_trader(
        &store,
        sink.clone(),
        gated(&broker),
        &[AAA, BBB],
        OrderBookState::default(),
    );
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        for bar in &session {
            trader.on_bar(bar).unwrap();
        }
    }
    // Two accepted orders are now open at the broker.
    assert_eq!(trader.rebalance().await.unwrap().orders_submitted, 2);
    assert!(!trader.risk().state().kill_switch_engaged);

    trader.engage_kill_switch("test").await.unwrap();

    assert!(calls(&log).contains(&Call::CancelAll));
    assert!(trader.risk().state().kill_switch_engaged);
    let events = risk_events(&sink.records());
    let kill: Vec<&RiskEvent> = events
        .iter()
        .filter(|e| e.kind == RiskEventKind::KillSwitch)
        .collect();
    assert_eq!(kill.len(), 1);
    assert_eq!(
        kill[0].detail,
        serde_json::json!({"engaged": true, "by": "test"})
    );
}

#[tokio::test]
async fn a_failed_cancel_is_returned_and_the_flag_stays_set() {
    let store = MemoryStore::new();
    let broker = FakeBroker::new(new_call_log());
    let mut trader = gated_trader(
        &store,
        MemorySink::new(),
        Gated {
            fail_cancel: true,
            ..gated(&broker)
        },
        &[AAA],
        OrderBookState::default(),
    );

    let result = trader.engage_kill_switch("test").await;

    assert!(matches!(result, Err(EngineError::Broker(_))));
    assert!(trader.risk().state().kill_switch_engaged);
}

#[tokio::test]
async fn a_failed_record_still_cancels_and_the_flag_is_set() {
    let store = MemoryStore::new();
    let log = new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    let sink = MemorySink::new();
    let mut trader = gated_trader(
        &store,
        sink.clone(),
        gated(&broker),
        &[AAA],
        OrderBookState::default(),
    );
    sink.fail_with("store gone");

    let result = trader.engage_kill_switch("test").await;

    assert!(matches!(result, Err(EngineError::Store(_))));
    assert!(calls(&log).contains(&Call::CancelAll));
    assert!(trader.risk().state().kill_switch_engaged);
}

#[tokio::test]
async fn refresh_reads_the_switch_and_engages_it_when_unreadable() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA]);

    assert!(trader.refresh_kill_switch(&Source(Some(true))).await);
    assert!(trader.risk().state().kill_switch_engaged);
    assert!(!trader.refresh_kill_switch(&Source(Some(false))).await);
    assert!(!trader.risk().state().kill_switch_engaged);
    assert!(trader.refresh_kill_switch(&Source(None)).await);
    assert!(trader.risk().state().kill_switch_engaged);
    // Refreshing never cancels or records: the store holds no risk events.
    assert!(risk_events(&store.records()).is_empty());
}

#[tokio::test]
async fn observe_equity_emits_peaks_and_mode_changes() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA]);

    assert_eq!(trader.observe_equity(d("100000")).await.unwrap(), None);
    let events = risk_events(&store.records());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, RiskEventKind::EquityPeak);
    assert_eq!(events[0].detail["peak"].as_str(), Some("100000"));

    assert_eq!(
        trader.observe_equity(d("90000")).await.unwrap(),
        Some((DrawdownMode::Normal, DrawdownMode::Halved))
    );
    let events = risk_events(&store.records());
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, RiskEventKind::DrawdownMode);
    assert_eq!(events[1].detail["mode"].as_str(), Some("halved"));
    assert_eq!(events[1].detail["equity"].as_str(), Some("90000"));
    assert_eq!(events[1].detail["reset"].as_bool(), Some(false));

    // No change, no event.
    let before = store.records().len();
    assert_eq!(trader.observe_equity(d("90000")).await.unwrap(), None);
    assert_eq!(store.records().len(), before);

    // A 15% fall from the peak is flat.
    assert_eq!(
        trader.observe_equity(d("85000")).await.unwrap(),
        Some((DrawdownMode::Halved, DrawdownMode::Flat))
    );
    let events = risk_events(&store.records());
    assert_eq!(events.last().unwrap().detail["mode"].as_str(), Some("flat"));
    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
}

/// One rebalance of the two-symbol rising scenario at 90,000 of equity, with the drawdown
/// state either fresh (`Normal`) or stepped down from 100,000 (`Halved`).
async fn round_at_90k(store: &MemoryStore, halved: bool) -> Vec<Decision> {
    let mut trader = engine_with(store, clock(), &[AAA, BBB], 90_000);
    if halved {
        trader.observe_equity(d("100000")).await.unwrap();
    }
    assert_eq!(
        trader.observe_equity(d("90000")).await.unwrap().is_some(),
        halved
    );
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        feed(&mut trader, &session).await;
    }
    trader.rebalance().await.unwrap();
    store.decisions()
}

#[tokio::test]
async fn halved_mode_scales_the_sizing_by_one_half() {
    let normal = round_at_90k(&MemoryStore::new(), false).await;
    let halved = round_at_90k(&MemoryStore::new(), true).await;

    assert_eq!(normal.len(), 2);
    for (n, h) in normal.iter().zip(&halved) {
        assert_eq!(n.features[feature::RISK_SCALE], Decimal::ONE);
        assert_eq!(h.features[feature::RISK_SCALE], d("0.5"));
        assert!(n.intended_qty > Decimal::ZERO);
        // Each quantity is truncated to 6 dp after the scale, so the halved one is within one
        // unit of 1e-6 of half the normal one.
        assert!((h.intended_qty * Decimal::TWO - n.intended_qty).abs() <= d("0.000002"));
    }
}

#[tokio::test]
async fn flat_mode_exits_held_positions_and_buys_nothing() {
    let store = MemoryStore::new();
    let mut trader = engine(&store, &[AAA, BBB, ZZZ]);
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B), (ZZZ, &FLAT)], 0) {
        feed(&mut trader, &session).await;
    }
    trader.rebalance().await.unwrap();
    for session in sessions(&[(AAA, &["109"]), (BBB, &["58"]), (ZZZ, &["50"])], 7) {
        feed(&mut trader, &session).await;
    }
    assert!(trader.state().positions.get(sym(AAA)).qty > Decimal::ZERO);
    assert!(trader.state().positions.get(sym(BBB)).qty > Decimal::ZERO);

    // A new high-water mark far above the account: the rebalance's own equity is then a fall of
    // more than 15%, so the mode goes flat.
    trader.observe_equity(d("200000")).await.unwrap();
    let before = store.intents().len();
    let report = trader.rebalance().await.unwrap();

    assert_eq!(trader.risk().state().drawdown.mode, DrawdownMode::Flat);
    let actions: Vec<Action> = report.decisions.iter().map(|(_, _, a)| *a).collect();
    assert_eq!(actions, vec![Action::Exit, Action::Exit, Action::Hold]);
    assert_eq!(report.orders_submitted, 2);
    assert_eq!(report.orders_blocked, 0);
    let round = &store.intents()[before..];
    assert_eq!(round.len(), 2);
    assert!(round.iter().all(|i| i.side == OrderSide::Sell));
    assert!(round.iter().all(|i| i.symbol != sym(ZZZ)));
    for (id, _, _) in report.decisions.iter().take(2) {
        assert!(risk_rows(&store, *id).iter().all(|r| r.passed));
    }
}

#[tokio::test]
async fn stale_data_blocks_buys_and_lets_sells_through() {
    let store = MemoryStore::new();
    let clock = ManualClock::new(UnixNanos::from(1_000_u64));
    let mut trader = engine_with(&store, &clock, &[AAA, BBB, ZZZ], 100_000);
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B), (ZZZ, &FLAT)], 0) {
        feed(&mut trader, &session).await;
    }
    trader.rebalance().await.unwrap();
    // Day 7: the first orders fill. Days 8 to 13: AAA falls out of the trend, ZZZ starts to rise.
    let falling_a = ["108", "106", "104", "102", "100", "98"];
    let rising_b = ["59", "60", "61", "62", "63", "64"];
    let rising_z = ["53", "55", "57", "59", "61", "63"];
    feed(
        &mut trader,
        &sessions(&[(AAA, &["109"]), (BBB, &["58"]), (ZZZ, &["51"])], 7)[0],
    )
    .await;
    for session in sessions(&[(AAA, &falling_a), (BBB, &rising_b), (ZZZ, &rising_z)], 8) {
        feed(&mut trader, &session).await;
    }
    // The clock is 97 hours past the newest bar (day 13); the limit is 96.
    clock.set(UnixNanos::from(13 * DAY_NANOS + 97 * HOUR_NANOS));
    let before = store.intents().len();

    let report = trader.rebalance().await.unwrap();

    let zzz = report
        .decisions
        .iter()
        .find(|(_, s, _)| *s == sym(ZZZ))
        .unwrap();
    assert_eq!(zzz.2, Action::Enter);
    let zzz_stale = risk_rows(&store, zzz.0)
        .into_iter()
        .find(|r| r.rule == rule::STALE_DATA)
        .unwrap();
    assert!(!zzz_stale.passed);
    assert_eq!(zzz_stale.actual_value, d("349200"));
    assert_eq!(zzz_stale.limit_value, d("345600"));
    assert!(report.orders_blocked >= 1);

    let round = &store.intents()[before..];
    assert!(round.iter().all(|i| i.side == OrderSide::Sell));
    assert!(round.iter().any(|i| i.symbol == sym(AAA)));
    assert!(round.iter().all(|i| i.symbol != sym(ZZZ)));
}

#[tokio::test]
async fn a_blocked_account_blocks_every_order() {
    let store = MemoryStore::new();
    let broker = FakeBroker::new(new_call_log());
    let sink = MemorySink::new();
    let mut trader = gated_trader(
        &store,
        sink.clone(),
        Gated {
            blocked: true,
            ..gated(&broker)
        },
        &[AAA, BBB],
        OrderBookState::default(),
    );
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        for bar in &session {
            trader.on_bar(bar).unwrap();
        }
    }

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 0);
    assert_eq!(report.orders_blocked, 2);
    assert!(store.intents().is_empty());
    // The checks went to the sink this trader was given.
    let records = sink.records();
    for (id, _, _) in &report.decisions {
        let rows = rows_in(&records, *id);
        let blocked = rows
            .iter()
            .find(|r| r.rule == rule::ACCOUNT_BLOCKED)
            .unwrap();
        assert!(!blocked.passed);
        assert_eq!(blocked.actual_value, Decimal::ONE);
    }
}

#[tokio::test]
async fn buys_in_one_rebalance_count_toward_the_exposure_cap() {
    // Nine symbols with nearly flat closes: each buy is capped at max_weight (12.5% of equity).
    // Eight of them make 100% of equity, the cap is 111.1%, and the ninth takes it to 112.5%.
    let store = MemoryStore::new();
    let mut trader = engine(&store, &NINE);
    let series: Vec<(&str, &[&str])> = NINE.iter().map(|s| (*s, &LOW[..])).collect();
    for session in sessions(&series, 0) {
        feed(&mut trader, &session).await;
    }

    let report = trader.rebalance().await.unwrap();

    assert_eq!(report.orders_submitted, 8);
    assert_eq!(report.orders_blocked, 1);
    let cap = d("1.111111");
    let first = risk_rows(&store, report.decisions[0].0);
    let first_exposure = first
        .iter()
        .find(|r| r.rule == rule::KELLY_EXPOSURE)
        .unwrap();
    assert!(first_exposure.passed);
    assert!((first_exposure.actual_value - d("0.125")).abs() < d("0.001"));

    let ninth = risk_rows(&store, report.decisions[8].0);
    let exposure = ninth
        .iter()
        .find(|r| r.rule == rule::KELLY_EXPOSURE)
        .unwrap();
    assert!(!exposure.passed);
    assert_eq!(exposure.limit_value, cap);
    // The earlier eight buys are in the figure: 9 × 12.5% of equity.
    assert!((exposure.actual_value - d("1.125")).abs() < d("0.001"));
}

#[tokio::test]
async fn a_failing_sink_while_recording_checks_sends_nothing() {
    let store = MemoryStore::new();
    let log = new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    let sink = MemorySink::new();
    let mut trader = gated_trader(
        &store,
        sink.clone(),
        gated(&broker),
        &[AAA, BBB],
        OrderBookState::default(),
    );
    for session in sessions(&[(AAA, &RISING_A), (BBB, &RISING_B)], 0) {
        for bar in &session {
            trader.on_bar(bar).unwrap();
        }
    }
    // The equity peak is recorded while the sink still works; the checks are not.
    trader.observe_equity(d("100000")).await.unwrap();
    sink.fail_with("store gone");

    let result = trader.rebalance().await;

    assert!(matches!(result, Err(EngineError::Store(_))));
    assert!(!calls(&log).iter().any(|c| matches!(c, Call::Submit(_))));
    assert!(store.intents().is_empty());
}

#[tokio::test]
async fn a_round_trip_in_one_day_is_a_day_trade() {
    let store = MemoryStore::new();
    let buy = intent("nisse-1-0", "10", None);
    let sell = OrderIntent {
        client_order_id: ClientOrderId::new("nisse-2-0"),
        side: OrderSide::Sell,
        ..buy
    };
    let mut state = OrderBookState::default();
    state.tracker.track_intent(buy).unwrap();
    state.tracker.track_intent(sell).unwrap();
    let mut trader = gated_trader(
        &store,
        MemorySink::new(),
        gated(&FakeBroker::new(new_call_log())),
        &[AAA],
        state,
    );
    let now = UnixNanos::from(1_000_u64);
    let fill = |order: OrderIntent, ts: u64| OrderUpdate {
        order: snapshot(&order, OrderState::Filled, "10", Some("100")),
        fill: Some(FillDetail {
            trade_id: TradeId::new(format!("trade-{ts}")),
            px: Price::from("100"),
            qty: Quantity::from("10"),
        }),
        ts: UnixNanos::from(ts),
    };

    trader.on_order_update(&fill(buy, 1_000)).await.unwrap();
    assert_eq!(trader.risk().day_trades().count_in_window(now), 0);
    trader.on_order_update(&fill(sell, 2_000)).await.unwrap();

    assert_eq!(trader.risk().day_trades().count_in_window(now), 1);
}

#[tokio::test]
async fn a_fill_of_an_untracked_order_is_not_a_day_trade() {
    let store = MemoryStore::new();
    let mut trader = gated_trader(
        &store,
        MemorySink::new(),
        gated(&FakeBroker::new(new_call_log())),
        &[AAA],
        OrderBookState::default(),
    );
    let stranger = intent("someone-else-1", "10", None);
    let update = OrderUpdate {
        order: snapshot(&stranger, OrderState::Filled, "10", Some("100")),
        fill: Some(FillDetail {
            trade_id: TradeId::new("trade-x"),
            px: Price::from("100"),
            qty: Quantity::from("10"),
        }),
        ts: UnixNanos::from(1_000_u64),
    };

    trader.on_order_update(&update).await.unwrap();

    assert_eq!(
        trader
            .risk()
            .day_trades()
            .count_in_window(UnixNanos::from(1_000_u64)),
        0
    );
}
