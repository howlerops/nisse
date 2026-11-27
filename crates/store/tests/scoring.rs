//! Acceptance tests for the scoring reads (`tables::scoring`, task DB2b) against a real
//! Postgres (`TEST_DATABASE_URL`).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, TradeId, UnixNanos,
    VenueOrderId,
};
use nisse_core::{
    DecisionId, Fill, OrderEvent, OrderIntent, OrderState, StrategyVersionId, Trial, TrialKind,
};
use nisse_store::StoreError;
use nisse_store::tables::scoring::{self, DecisionFill, DecisionOrders, TrialWithResult};
use nisse_store::tables::{fill, order_event, trial_registry, trial_result};
use nisse_store::testing::ScratchDb;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// A whole-microsecond base time (2022-04-19), so timestamps round-trip exactly.
const BASE: u64 = 1_650_390_305_000_000;
const DAY_MICROS: u64 = 86_400 * 1_000_000;

/// The eight rule names of `nisse_risk::rule::ALL`, the checks the gate writes for every order.
const RULES: [&str; 8] = [
    "kill_switch",
    "account_blocked",
    "drawdown_flat",
    "stale_data",
    "no_short",
    "risk_per_trade",
    "kelly_exposure",
    "day_trade_limit",
];

fn micros(us: u64) -> UnixNanos {
    UnixNanos::from(us * 1_000)
}

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn spy() -> Symbol {
    Symbol::new_checked("SPY").unwrap()
}

/// Inserts a strategy version whose git SHA and config hash are unique to `n` (1-9).
async fn version(client: &Client, n: u8) -> StrategyVersionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES ($1, $2, '{}', '2026-01-02T00:00:00Z') RETURNING id",
            &[&n.to_string().repeat(40), &vec![n; 32]],
        )
        .await
        .unwrap()
        .get(0);
    StrategyVersionId(id)
}

/// Inserts one decision for `symbol` with the given features.
async fn decision(
    client: &Client,
    version: StrategyVersionId,
    symbol: &str,
    features: Value,
) -> DecisionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                                   feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
             VALUES ($1, $2, 'enter', 1.5, $3, decode(repeat('ab', 32), 'hex'), 'snap-1', \
                     42, 'trend_up', '2026-01-05T21:00:00Z') RETURNING id",
            &[&version.0, &symbol, &features],
        )
        .await
        .unwrap()
        .get(0);
    DecisionId(id)
}

/// Inserts a decision on SPY with no features.
async fn spy_decision(client: &Client, version: StrategyVersionId) -> DecisionId {
    decision(client, version, "SPY", json!({})).await
}

/// A market order intent for `symbol`, formed at `ts` microseconds after `BASE`.
fn intent(
    decision: DecisionId,
    client_order_id: &str,
    symbol: &str,
    side: OrderSide,
    qty: &str,
    ts: u64,
) -> OrderIntent {
    OrderIntent {
        decision_id: decision,
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        symbol: Symbol::new_checked(symbol).unwrap(),
        side,
        order_type: OrderType::Market,
        qty: Quantity::from(qty),
        limit_px: None,
        time_in_force: TimeInForce::Day,
        ts: micros(BASE + ts),
    }
}

/// A SPY order: see [`order_on`].
async fn order(
    client: &Client,
    decision: DecisionId,
    client_order_id: &str,
    side: OrderSide,
    qty: &str,
    states: &[(OrderState, &str)],
) -> OrderIntent {
    order_on(client, decision, "SPY", client_order_id, side, qty, states).await
}

/// Writes the intent row of an order on `symbol`, then one row per `(state, cumulative
/// fill)` pair.
async fn order_on(
    client: &Client,
    decision: DecisionId,
    symbol: &str,
    client_order_id: &str,
    side: OrderSide,
    qty: &str,
    states: &[(OrderState, &str)],
) -> OrderIntent {
    let order = intent(decision, client_order_id, symbol, side, qty, 0);
    order_event::insert(client, &OrderEvent::intent(&order))
        .await
        .unwrap();
    for (i, (state, cum)) in states.iter().enumerate() {
        let event = OrderEvent::of(
            &order,
            *state,
            Some(VenueOrderId::from("V-1")),
            Quantity::from(*cum),
            micros(BASE + 1 + i as u64),
        );
        order_event::insert(client, &event).await.unwrap();
    }
    order
}

async fn put_fill(
    client: &Client,
    client_order_id: &str,
    trade: &str,
    px: &str,
    qty: &str,
    ts: u64,
) {
    let fill = Fill {
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        trade_id: TradeId::new_checked(trade).unwrap(),
        px: px.parse::<Price>().unwrap(),
        qty: Quantity::from(qty),
        ts: micros(ts),
    };
    fill::insert(client, &fill).await.unwrap();
}

/// One `risk_check` row for `decision`.
async fn check(client: &Client, decision: DecisionId, rule: &str, actual: Decimal, passed: bool) {
    client
        .execute(
            "INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts) \
             VALUES ($1, $2, 1000, $3::numeric, $4, '2026-01-05T21:00:00Z')",
            &[&decision.0, &rule, &actual, &passed],
        )
        .await
        .unwrap();
}

/// One `eval_score` row for a decision.
async fn score(client: &Client, decision: DecisionId, metric: &str, value: f64) {
    client
        .execute(
            "INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts) \
             VALUES ($1, 'decision', $2, $3::float8, 'v1', '2026-01-05T21:00:00Z')",
            &[&decision.0, &metric, &value],
        )
        .await
        .unwrap();
}

/// Registers a trial of `kind` with an optional result stored on the registry row.
async fn register(
    client: &Client,
    version: StrategyVersionId,
    kind: TrialKind,
    result: Option<Value>,
    ts: u64,
) -> i64 {
    let trial = Trial {
        strategy_version_id: version,
        kind,
        data_start: micros(BASE),
        data_end: micros(BASE + DAY_MICROS),
        result,
        ts: micros(ts),
    };
    trial_registry::insert(client, &trial).await.unwrap()
}

#[tokio::test]
async fn fills_carry_their_orders_decision_symbol_and_side() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let d1 = spy_decision(&client, v).await;
    let d2 = decision(&client, v, "QQQ", json!({})).await;

    let buy = order(
        &client,
        d1,
        "buy-1",
        OrderSide::Buy,
        "5",
        &[(OrderState::Filled, "5")],
    )
    .await;
    assert_eq!(buy.decision_id, d1);
    order_on(
        &client,
        d2,
        "QQQ",
        "sell-1",
        OrderSide::Sell,
        "1",
        &[(OrderState::Filled, "1")],
    )
    .await;

    put_fill(&client, "buy-1", "T-1", "500.10", "2", BASE + 10).await;
    put_fill(&client, "buy-1", "T-2", "500.25", "3", BASE + 11).await;
    put_fill(&client, "sell-1", "T-3", "400.5", "1", BASE + 12).await;

    let fills = scoring::fills(&client).await.unwrap();
    assert_eq!(
        fills,
        vec![
            DecisionFill {
                decision_id: d1,
                symbol: spy(),
                side: OrderSide::Buy,
                qty: dec("2"),
                px: dec("500.10"),
                fee: Decimal::ZERO,
                ts: micros(BASE + 10),
            },
            DecisionFill {
                decision_id: d1,
                symbol: spy(),
                side: OrderSide::Buy,
                qty: dec("3"),
                px: dec("500.25"),
                fee: Decimal::ZERO,
                ts: micros(BASE + 11),
            },
            DecisionFill {
                decision_id: d2,
                symbol: Symbol::new_checked("QQQ").unwrap(),
                side: OrderSide::Sell,
                qty: dec("1"),
                px: dec("400.5"),
                fee: Decimal::ZERO,
                ts: micros(BASE + 12),
            },
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn fills_is_empty_without_fills_and_an_orphan_fill_is_a_data_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    assert!(scoring::fills(&client).await.unwrap().is_empty());

    put_fill(&client, "ghost-1", "T-9", "10", "1", BASE).await;
    let result = scoring::fills(&client).await;
    assert!(matches!(result, Err(StoreError::Data(_))));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn decision_orders_summarise_each_decision() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    // d1: one order, intent -> accepted -> filled.
    let d1 = spy_decision(&client, v).await;
    order(
        &client,
        d1,
        "o-1",
        OrderSide::Buy,
        "5",
        &[(OrderState::Accepted, "0"), (OrderState::Filled, "5")],
    )
    .await;
    // d2: one order still accepted.
    let d2 = spy_decision(&client, v).await;
    order(
        &client,
        d2,
        "o-2",
        OrderSide::Buy,
        "4",
        &[(OrderState::Accepted, "0")],
    )
    .await;
    // d3: only the intent row.
    let d3 = spy_decision(&client, v).await;
    order(&client, d3, "o-3", OrderSide::Buy, "2", &[]).await;
    // d4: two orders, one filled and one canceled.
    let d4 = spy_decision(&client, v).await;
    order(
        &client,
        d4,
        "o-4a",
        OrderSide::Buy,
        "3",
        &[(OrderState::Filled, "3")],
    )
    .await;
    order(
        &client,
        d4,
        "o-4b",
        OrderSide::Buy,
        "2",
        &[(OrderState::Canceled, "0")],
    )
    .await;
    // d5: a decision with no orders does not appear.
    spy_decision(&client, v).await;
    // d6 and d7: rejected and expired both count as terminal.
    let d6 = spy_decision(&client, v).await;
    order(
        &client,
        d6,
        "o-6",
        OrderSide::Buy,
        "1",
        &[(OrderState::Rejected, "0")],
    )
    .await;
    let d7 = spy_decision(&client, v).await;
    order(
        &client,
        d7,
        "o-7",
        OrderSide::Sell,
        "1",
        &[(OrderState::Expired, "0")],
    )
    .await;

    let summary = scoring::decision_orders(&client).await.unwrap();
    let spy_sym = spy();
    // `last_change` is the newest order row: `order_on` writes its states at BASE + 1 + i, and
    // the intent at BASE. No fills in this test.
    let expected =
        |decision_id, ordered_qty: &str, all_terminal, sent, last_change: u64| DecisionOrders {
            decision_id,
            symbol: spy_sym,
            side: OrderSide::Buy,
            ordered_qty: dec(ordered_qty),
            all_terminal,
            sent,
            last_change: micros(BASE + last_change),
        };
    assert_eq!(
        summary,
        vec![
            expected(d1, "5", true, true, 2),
            expected(d2, "4", false, true, 1),
            expected(d3, "2", false, false, 0),
            expected(d4, "5", true, true, 1),
            expected(d6, "1", true, true, 1),
            DecisionOrders {
                side: OrderSide::Sell,
                ..expected(d7, "1", true, true, 1)
            },
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn arrival_prices_read_the_close_feature_exactly() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    let d1 = decision(&client, v, "SPY", json!({"close": "101.25", "vol": "1"})).await;
    let d2 = decision(&client, v, "SPY", json!({"vol": "1"})).await;
    let d3 = decision(&client, v, "SPY", json!({"close": "not-a-decimal"})).await;
    let d4 = decision(&client, v, "SPY", json!({"close": "99"})).await;
    let d5 = decision(&client, v, "SPY", json!({"close": null})).await;

    let prices = scoring::arrival_prices(&client, &[d5, d4, d3, d2, d1])
        .await
        .unwrap();
    assert_eq!(prices.len(), 2);
    assert_eq!(prices[0].0, d1);
    assert_eq!(prices[0].1.to_string(), "101.25");
    assert_eq!(prices[1], (d4, dec("99")));

    assert!(
        scoring::arrival_prices(&client, &[])
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn entry_risks_take_the_newest_positive_risk_per_trade() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    let d1 = spy_decision(&client, v).await;
    check(&client, d1, "risk_per_trade", dec("50"), true).await;
    check(&client, d1, "risk_per_trade", dec("75"), true).await;

    let zero = spy_decision(&client, v).await;
    check(&client, zero, "risk_per_trade", dec("0"), true).await;

    let negative = spy_decision(&client, v).await;
    check(&client, negative, "risk_per_trade", dec("-1"), true).await;

    let other_rule = spy_decision(&client, v).await;
    check(&client, other_rule, "max_position", dec("500"), true).await;

    // Newest value is 0 after a positive one: the newest row decides, so it is left out.
    let reset = spy_decision(&client, v).await;
    check(&client, reset, "risk_per_trade", dec("40"), true).await;
    check(&client, reset, "risk_per_trade", dec("0"), true).await;

    let d6 = spy_decision(&client, v).await;
    check(&client, d6, "risk_per_trade", dec("0"), true).await;
    check(&client, d6, "risk_per_trade", dec("30"), false).await;

    let risks = scoring::entry_risks(&client).await.unwrap();
    assert_eq!(risks, vec![(d1, dec("75")), (d6, dec("30"))]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn backtest_results_return_only_backtests_with_a_result() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    let nested = json!({"sharpe": 0.4, "trades": 37, "equity": {"points": [1, 2, 3]}});
    let with_result = register(&client, v, TrialKind::Backtest, None, BASE + 1).await;
    trial_result::insert(&client, with_result, &nested, micros(BASE + 2))
        .await
        .unwrap();

    // Registered, never finished.
    register(&client, v, TrialKind::Backtest, None, BASE + 3).await;

    // A shadow run with a result is not a backtest.
    let shadow = register(&client, v, TrialKind::Shadow, None, BASE + 4).await;
    trial_result::insert(&client, shadow, &json!({"sharpe": 9}), micros(BASE + 5))
        .await
        .unwrap();

    // A holdout trial, marked on the registry row.
    let holdout = register(
        &client,
        v,
        TrialKind::Backtest,
        Some(json!({"holdout": true})),
        BASE + 6,
    )
    .await;
    trial_result::insert(&client, holdout, &json!({"sharpe": 2}), micros(BASE + 7))
        .await
        .unwrap();

    let results = scoring::backtest_results(&client).await.unwrap();
    assert_eq!(
        results,
        vec![
            TrialWithResult {
                trial_id: with_result,
                strategy_version_id: v,
                holdout: false,
                result: nested,
                ts: micros(BASE + 1),
            },
            TrialWithResult {
                trial_id: holdout,
                strategy_version_id: v,
                holdout: true,
                result: json!({"sharpe": 2}),
                ts: micros(BASE + 6),
            },
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn replay_failures_count_the_newest_replay_match_per_decision() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let a = version(&client, 1).await;
    let b = version(&client, 2).await;

    let d1 = spy_decision(&client, a).await;
    score(&client, d1, "replay_match", 1.0).await;

    let d2 = spy_decision(&client, a).await;
    score(&client, d2, "replay_match", 1.0).await;
    score(&client, d2, "replay_match", 0.0).await;

    let d3 = spy_decision(&client, a).await;
    score(&client, d3, "other_metric", 0.0).await;

    let other = spy_decision(&client, b).await;
    score(&client, other, "replay_match", 0.0).await;

    assert_eq!(scoring::replay_failures(&client, a).await.unwrap(), (1, 2));
    assert_eq!(scoring::replay_failures(&client, b).await.unwrap(), (1, 1));

    // A later passing replay supersedes the failure.
    score(&client, d2, "replay_match", 1.0).await;
    assert_eq!(scoring::replay_failures(&client, a).await.unwrap(), (0, 2));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn replay_failures_of_a_version_without_decisions_is_zero() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let a = version(&client, 1).await;
    assert_eq!(scoring::replay_failures(&client, a).await.unwrap(), (0, 0));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_breaches_count_sent_orders_of_failed_decisions_once_each() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let a = version(&client, 1).await;
    let b = version(&client, 2).await;

    // A failed check with only the intent row: not a breach.
    let d1 = spy_decision(&client, a).await;
    check(&client, d1, "max_position", dec("900"), false).await;
    order(&client, d1, "o-1", OrderSide::Buy, "5", &[]).await;
    assert_eq!(scoring::risk_breaches(&client, a).await.unwrap(), 0);

    // A failed check and an order that reached accepted: one breach.
    let d2 = spy_decision(&client, a).await;
    check(&client, d2, "max_position", dec("900"), false).await;
    order(
        &client,
        d2,
        "o-2",
        OrderSide::Buy,
        "5",
        &[(OrderState::Accepted, "0")],
    )
    .await;
    assert_eq!(scoring::risk_breaches(&client, a).await.unwrap(), 1);

    // All eight gate checks passed and the order was sent: not a breach. (Before the full-set
    // rule, one passed check was enough; a decision with fewer than eight distinct rules is now a
    // breach, so this decision writes all eight.)
    let d3 = spy_decision(&client, a).await;
    for rule in RULES {
        check(&client, d3, rule, dec("100"), true).await;
    }
    order(
        &client,
        d3,
        "o-3",
        OrderSide::Buy,
        "5",
        &[(OrderState::Accepted, "0")],
    )
    .await;
    assert_eq!(scoring::risk_breaches(&client, a).await.unwrap(), 1);

    // Several failed checks and several rows per order: each order counts once.
    let d4 = spy_decision(&client, a).await;
    check(&client, d4, "max_position", dec("900"), false).await;
    check(&client, d4, "risk_per_trade", dec("9"), false).await;
    order(
        &client,
        d4,
        "o-4a",
        OrderSide::Buy,
        "5",
        &[(OrderState::Submitted, "0"), (OrderState::Accepted, "0")],
    )
    .await;
    order(
        &client,
        d4,
        "o-4b",
        OrderSide::Buy,
        "5",
        &[(OrderState::Accepted, "0"), (OrderState::Filled, "5")],
    )
    .await;
    assert_eq!(scoring::risk_breaches(&client, a).await.unwrap(), 3);

    // Another version's breach is not counted.
    let other = spy_decision(&client, b).await;
    check(&client, other, "max_position", dec("900"), false).await;
    order(
        &client,
        other,
        "o-5",
        OrderSide::Buy,
        "5",
        &[(OrderState::Accepted, "0")],
    )
    .await;
    assert_eq!(scoring::risk_breaches(&client, a).await.unwrap(), 3);
    assert_eq!(scoring::risk_breaches(&client, b).await.unwrap(), 1);
    db.drop_db().await.unwrap();
}

/// Appends a row moving `order` to `state` with `cum` filled, at `BASE` plus `at` microseconds.
async fn state_at(client: &Client, order: &OrderIntent, state: OrderState, cum: &str, at: u64) {
    let event = OrderEvent::of(
        order,
        state,
        Some(VenueOrderId::from("V-1")),
        Quantity::from(cum),
        micros(BASE + at),
    );
    order_event::insert(client, &event).await.unwrap();
}

#[tokio::test]
async fn fills_come_in_event_time_order_then_id_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let d1 = spy_decision(&client, v).await;
    order(
        &client,
        d1,
        "buy-1",
        OrderSide::Buy,
        "5",
        &[(OrderState::Filled, "5")],
    )
    .await;
    order(
        &client,
        d1,
        "sell-1",
        OrderSide::Sell,
        "1",
        &[(OrderState::Filled, "1")],
    )
    .await;

    // The sell is stored first but happened later; the buy is a backfill stored after it.
    put_fill(&client, "sell-1", "T-S", "400.5", "1", BASE + 20).await;
    put_fill(&client, "buy-1", "T-B", "500.10", "1", BASE + 10).await;
    // Two fills with the same event time come back in the order they were stored.
    put_fill(&client, "buy-1", "T-X", "501", "2", BASE + 30).await;
    put_fill(&client, "buy-1", "T-Y", "502", "3", BASE + 30).await;

    let fills = scoring::fills(&client).await.unwrap();
    let fill = |side, qty: &str, px: &str, at: u64| DecisionFill {
        decision_id: d1,
        symbol: spy(),
        side,
        qty: dec(qty),
        px: dec(px),
        fee: Decimal::ZERO,
        ts: micros(BASE + at),
    };
    assert_eq!(
        fills,
        vec![
            fill(OrderSide::Buy, "1", "500.10", 10),
            fill(OrderSide::Sell, "1", "400.5", 20),
            fill(OrderSide::Buy, "2", "501", 30),
            fill(OrderSide::Buy, "3", "502", 30),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn last_change_is_the_latest_order_row_or_fill() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    // d1: order rows at 100 and 200, a fill at 300 (later than both): 300.
    let d1 = spy_decision(&client, v).await;
    let o1 = intent(d1, "o-1", "SPY", OrderSide::Buy, "5", 0);
    order_event::insert(&client, &OrderEvent::intent(&o1))
        .await
        .unwrap();
    state_at(&client, &o1, OrderState::Accepted, "0", 100).await;
    state_at(&client, &o1, OrderState::Filled, "5", 200).await;
    put_fill(&client, "o-1", "T-1", "100", "5", BASE + 300).await;

    // d2: order rows at 100 and 200 and no fill: the newest order row, 200.
    let d2 = spy_decision(&client, v).await;
    let o2 = intent(d2, "o-2", "SPY", OrderSide::Buy, "5", 0);
    order_event::insert(&client, &OrderEvent::intent(&o2))
        .await
        .unwrap();
    state_at(&client, &o2, OrderState::Accepted, "0", 100).await;
    state_at(&client, &o2, OrderState::Filled, "5", 200).await;

    // d3: an order row at 200 and a fill at 150, older than the row: the row's time, 200.
    let d3 = spy_decision(&client, v).await;
    let o3 = intent(d3, "o-3", "SPY", OrderSide::Buy, "5", 0);
    order_event::insert(&client, &OrderEvent::intent(&o3))
        .await
        .unwrap();
    state_at(&client, &o3, OrderState::Accepted, "0", 200).await;
    put_fill(&client, "o-3", "T-3", "100", "1", BASE + 150).await;

    let changes: Vec<(DecisionId, UnixNanos)> = scoring::decision_orders(&client)
        .await
        .unwrap()
        .iter()
        .map(|order| (order.decision_id, order.last_change))
        .collect();
    assert_eq!(
        changes,
        vec![
            (d1, micros(BASE + 300)),
            (d2, micros(BASE + 200)),
            (d3, micros(BASE + 200)),
        ]
    );
    db.drop_db().await.unwrap();
}

/// Writes one decision on a fresh version `n` with `checks` as `(rule, passed)` rows and one
/// order (sent, or intent only), and returns the version's risk breaches.
async fn breaches_of(client: &Client, n: u8, checks: &[(&str, bool)], sent: bool) -> i64 {
    let v = version(client, n).await;
    let d = spy_decision(client, v).await;
    for (rule, passed) in checks {
        check(client, d, rule, dec("1"), *passed).await;
    }
    let states: &[(OrderState, &str)] = if sent {
        &[(OrderState::Accepted, "0")]
    } else {
        &[]
    };
    order(client, d, &format!("o-v{n}"), OrderSide::Buy, "5", states).await;
    scoring::risk_breaches(client, v).await.unwrap()
}

#[tokio::test]
async fn risk_breaches_need_all_eight_distinct_rules() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    // All eight passed, sent: clean.
    let all: Vec<(&str, bool)> = RULES.iter().map(|r| (*r, true)).collect();
    assert_eq!(breaches_of(&client, 3, &all, true).await, 0);

    // Only seven distinct rules, all passed, sent: a breach.
    let seven: Vec<(&str, bool)> = RULES[..7].iter().map(|r| (*r, true)).collect();
    assert_eq!(breaches_of(&client, 4, &seven, true).await, 1);

    // No risk_check rows at all, sent: a breach.
    assert_eq!(breaches_of(&client, 5, &[], true).await, 1);

    // All eight rules, one failed, sent: a breach.
    let mut one_failed = all.clone();
    one_failed[3].1 = false;
    assert_eq!(breaches_of(&client, 6, &one_failed, true).await, 1);

    // Eight rows but only seven distinct rules (one repeated): duplicates do not count, a breach.
    let mut repeated = seven.clone();
    repeated.push((RULES[0], true));
    assert_eq!(breaches_of(&client, 7, &repeated, true).await, 1);

    // Intent only, no checks: not sent, so not a breach.
    assert_eq!(breaches_of(&client, 8, &[], false).await, 0);
    db.drop_db().await.unwrap();
}
