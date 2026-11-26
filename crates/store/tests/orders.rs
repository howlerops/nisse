//! Acceptance tests for the `order_event` and `fill` tables (Phase 2, task S2).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, TradeId, UnixNanos,
    VenueOrderId,
};
use nisse_core::{DecisionId, Fill, OrderEvent, OrderIntent, OrderState, RestoredOrder};
use nisse_store::tables::fill::{self, FillSide};
use nisse_store::tables::order_event;
use nisse_store::testing::ScratchDb;
use nisse_store::{Store, StoreError};
use rust_decimal::Decimal;
use tokio_postgres::Client;

/// A whole-microsecond base time (2022-04-19), so timestamps round-trip exactly.
const BASE: u64 = 1_650_390_305_000_000;

fn micros(us: u64) -> UnixNanos {
    UnixNanos::from(us * 1_000)
}

/// Inserts one strategy version and one decision with plain SQL and returns the decision id.
async fn decision_id(client: &Client) -> DecisionId {
    let version: i64 = client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', \
                     '2026-01-02T00:00:00Z') RETURNING id",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let id: i64 = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                                   feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
             VALUES ($1, 'SPY', 'enter', 1.5, '{}', decode(repeat('ab', 32), 'hex'), 'snap-1', \
                     42, 'trend_up', '2026-01-05T21:00:00Z') RETURNING id",
            &[&version],
        )
        .await
        .unwrap()
        .get(0);
    DecisionId(id)
}

/// A limit buy of 5 SPY at 500.10, day order, formed at `ts` microseconds.
fn limit_intent(decision: DecisionId, client_order_id: &str, ts: u64) -> OrderIntent {
    OrderIntent {
        decision_id: decision,
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        symbol: Symbol::new_checked("SPY").unwrap(),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        qty: Quantity::from("5"),
        limit_px: Some("500.10".parse::<Price>().unwrap()),
        time_in_force: TimeInForce::Day,
        ts: micros(ts),
    }
}

/// One state row for `intent`: `venue` is the broker id (None before the broker has one).
fn row(
    intent: &OrderIntent,
    state: OrderState,
    venue: Option<&str>,
    cum: &str,
    ts: u64,
) -> OrderEvent {
    OrderEvent::of(
        intent,
        state,
        venue.map(VenueOrderId::from),
        Quantity::from(cum),
        micros(ts),
    )
}

fn fill_of(client_order_id: &str, trade: &str, px: &str, qty: &str, ts: u64) -> Fill {
    Fill {
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        trade_id: TradeId::new_checked(trade).unwrap(),
        px: px.parse::<Price>().unwrap(),
        qty: Quantity::from(qty),
        ts: micros(ts),
    }
}

async fn put(client: &Client, event: &OrderEvent) {
    order_event::insert(client, event).await.unwrap();
}

fn restored(
    intent: OrderIntent,
    venue: Option<&str>,
    state: OrderState,
    cum: &str,
    avg: Option<&str>,
    last_ts: u64,
) -> RestoredOrder {
    RestoredOrder {
        intent,
        venue_order_id: venue.map(VenueOrderId::from),
        state,
        cum_filled_qty: Quantity::from(cum),
        avg_fill_px: avg.map(|px| px.parse::<Price>().unwrap()),
        last_update_ts: micros(last_ts),
    }
}

#[tokio::test]
async fn intent_alone_restores_every_field() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(intent, None, OrderState::Intent, "0", None, BASE)]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn latest_row_supplies_state_venue_cumulative_quantity_and_timestamp() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    put(
        &client,
        &row(&intent, OrderState::Submitted, None, "0", BASE + 1),
    )
    .await;
    put(
        &client,
        &row(&intent, OrderState::Accepted, Some("V-1"), "0", BASE + 2),
    )
    .await;
    put(
        &client,
        &row(
            &intent,
            OrderState::PartiallyFilled,
            Some("V-1"),
            "2",
            BASE + 3,
        ),
    )
    .await;
    fill::insert(&client, &fill_of("order-1", "T-1", "10", "2", BASE + 3))
        .await
        .unwrap();

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(
            intent,
            Some("V-1"),
            OrderState::PartiallyFilled,
            "2",
            Some("10"),
            BASE + 3,
        )]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn avg_fill_px_is_quantity_weighted_and_ignores_other_orders() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    put(
        &client,
        &row(
            &intent,
            OrderState::PartiallyFilled,
            Some("V-1"),
            "5",
            BASE + 1,
        ),
    )
    .await;
    let other = limit_intent(decision, "order-2", BASE);
    put(&client, &OrderEvent::intent(&other)).await;
    put(
        &client,
        &row(
            &other,
            OrderState::PartiallyFilled,
            Some("V-2"),
            "1",
            BASE + 1,
        ),
    )
    .await;

    assert!(
        fill::insert(&client, &fill_of("order-1", "T-1", "10", "2", BASE + 1))
            .await
            .unwrap()
    );
    assert!(
        fill::insert(&client, &fill_of("order-1", "T-2", "11", "3", BASE + 1))
            .await
            .unwrap()
    );
    assert!(
        fill::insert(&client, &fill_of("order-2", "T-3", "99", "1", BASE + 1))
            .await
            .unwrap()
    );

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![
            restored(
                intent,
                Some("V-1"),
                OrderState::PartiallyFilled,
                "5",
                Some("10.6"),
                BASE + 1
            ),
            restored(
                other,
                Some("V-2"),
                OrderState::PartiallyFilled,
                "1",
                Some("99"),
                BASE + 1
            ),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn avg_fill_px_rounds_to_nine_places() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    put(
        &client,
        &row(&intent, OrderState::Accepted, Some("V-1"), "3", BASE + 1),
    )
    .await;
    fill::insert(&client, &fill_of("order-1", "T-1", "1", "1", BASE + 1))
        .await
        .unwrap();
    fill::insert(&client, &fill_of("order-1", "T-2", "1", "1", BASE + 1))
        .await
        .unwrap();
    fill::insert(&client, &fill_of("order-1", "T-3", "2", "1", BASE + 1))
        .await
        .unwrap();

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].avg_fill_px,
        Some("1.333333333".parse::<Price>().unwrap())
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn orders_with_terminal_latest_state_are_absent() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let terminal = [
        (OrderState::Filled, "done-filled"),
        (OrderState::Canceled, "done-canceled"),
        (OrderState::Replaced, "done-replaced"),
        (OrderState::Rejected, "done-rejected"),
        (OrderState::Expired, "done-expired"),
    ];
    for (offset, (state, id)) in terminal.iter().enumerate() {
        let intent = limit_intent(decision, id, BASE);
        put(&client, &OrderEvent::intent(&intent)).await;
        put(
            &client,
            &row(&intent, *state, Some("V-x"), "0", BASE + 1 + offset as u64),
        )
        .await;
    }
    let open_intent = limit_intent(decision, "open-1", BASE);
    put(&client, &OrderEvent::intent(&open_intent)).await;
    put(
        &client,
        &row(
            &open_intent,
            OrderState::Accepted,
            Some("V-9"),
            "0",
            BASE + 1,
        ),
    )
    .await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(
            open_intent,
            Some("V-9"),
            OrderState::Accepted,
            "0",
            None,
            BASE + 1,
        )]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn pending_cancel_latest_state_is_returned() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    put(
        &client,
        &row(&intent, OrderState::Accepted, Some("V-1"), "0", BASE + 1),
    )
    .await;
    put(
        &client,
        &row(
            &intent,
            OrderState::PendingCancel,
            Some("V-1"),
            "0",
            BASE + 2,
        ),
    )
    .await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(
            intent,
            Some("V-1"),
            OrderState::PendingCancel,
            "0",
            None,
            BASE + 2,
        )]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn results_are_in_client_order_id_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    for id in ["order-c", "order-a", "order-b"] {
        let intent = limit_intent(decision, id, BASE);
        put(&client, &OrderEvent::intent(&intent)).await;
    }

    let ids: Vec<String> = order_event::load_open(&client)
        .await
        .unwrap()
        .iter()
        .map(|order| order.intent.client_order_id.to_string())
        .collect();
    assert_eq!(ids, ["order-a", "order-b", "order-c"]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn latest_is_the_row_inserted_last_whatever_its_event_time() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    // The last row inserted carries an earlier event time than the ones before it. It is
    // still the latest: insertion order decides, not event time.
    put(
        &client,
        &row(&intent, OrderState::Submitted, None, "0", BASE + 5),
    )
    .await;
    put(
        &client,
        &row(&intent, OrderState::Accepted, Some("V-1"), "0", BASE + 5),
    )
    .await;
    put(
        &client,
        &row(
            &intent,
            OrderState::PendingCancel,
            Some("V-1"),
            "0",
            BASE + 4,
        ),
    )
    .await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(
            intent,
            Some("V-1"),
            OrderState::PendingCancel,
            "0",
            None,
            BASE + 4,
        )]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn venue_id_on_latest_row_only_is_restored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    put(
        &client,
        &row(&intent, OrderState::Accepted, Some("V-7"), "0", BASE + 1),
    )
    .await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![restored(
            intent,
            Some("V-7"),
            OrderState::Accepted,
            "0",
            None,
            BASE + 1,
        )]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_and_limit_orders_both_round_trip() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let limit = limit_intent(decision, "order-limit", BASE);
    let market = OrderIntent {
        decision_id: decision,
        client_order_id: ClientOrderId::new_checked("order-market").unwrap(),
        symbol: Symbol::new_checked("QQQ").unwrap(),
        side: OrderSide::Sell,
        order_type: OrderType::Market,
        qty: Quantity::from("1.5"),
        limit_px: None,
        time_in_force: TimeInForce::Gtc,
        ts: micros(BASE),
    };
    put(&client, &OrderEvent::intent(&limit)).await;
    put(&client, &OrderEvent::intent(&market)).await;

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(
        open,
        vec![
            restored(limit, None, OrderState::Intent, "0", None, BASE),
            restored(market, None, OrderState::Intent, "0", None, BASE),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn fill_insert_is_idempotent_and_exact() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;

    let first = fill_of("order-1", "T-1", "500.10", "0.123456789", BASE + 1);
    assert!(fill::insert(&client, &first).await.unwrap());
    assert!(!fill::insert(&client, &first).await.unwrap());

    let rows = client
        .query("SELECT px::numeric, qty::numeric FROM fill", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let px: Decimal = rows[0].get(0);
    let qty: Decimal = rows[0].get(1);
    assert_eq!(px, "500.10".parse::<Decimal>().unwrap());
    assert_eq!(qty, "0.123456789".parse::<Decimal>().unwrap());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn unknown_decision_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let intent = limit_intent(DecisionId(999_999), "order-1", BASE);

    let err = order_event::insert(&client, &OrderEvent::intent(&intent))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn cumulative_quantity_above_order_quantity_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;

    let bad = row(
        &intent,
        OrderState::PartiallyFilled,
        Some("V-1"),
        "6",
        BASE + 1,
    );
    let err = order_event::insert(&client, &bad).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn fifty_orders_with_three_rows_each_load_in_one_call() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    for n in 0..50u64 {
        let id = format!("order-{n:02}");
        let intent = limit_intent(decision, &id, BASE + n * 10);
        put(&client, &OrderEvent::intent(&intent)).await;
        put(
            &client,
            &row(&intent, OrderState::Submitted, None, "0", BASE + n * 10 + 1),
        )
        .await;
        put(
            &client,
            &row(
                &intent,
                OrderState::Accepted,
                Some("V-1"),
                "0",
                BASE + n * 10 + 2,
            ),
        )
        .await;
    }

    let open = order_event::load_open(&client).await.unwrap();
    assert_eq!(open.len(), 50);
    assert_eq!(open[0].intent.client_order_id.to_string(), "order-00");
    assert_eq!(open[49].intent.client_order_id.to_string(), "order-49");
    assert!(open.iter().all(|order| order.state == OrderState::Accepted));
    db.drop_db().await.unwrap();
}

// Regression tests for the Phase 2 independent review.

#[tokio::test]
async fn latest_row_is_the_one_inserted_last_even_when_clocks_disagree() {
    // The engine clock runs ahead of the broker's: the intent is stamped later than the fill
    // that followed it. The fill row was still inserted last, so it is the latest.
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "skew-1", BASE + 1_000_000);
    order_event::insert(&client, &OrderEvent::intent(&intent))
        .await
        .unwrap();
    let filled = OrderEvent::of(
        &intent,
        OrderState::Filled,
        Some(VenueOrderId::new("brk-1")),
        intent.qty,
        micros(BASE + 900_000),
    );
    order_event::insert(&client, &filled).await.unwrap();

    assert!(
        order_event::load_open(&client).await.unwrap().is_empty(),
        "the order is filled, not an open intent"
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn load_live_also_returns_orders_that_ended_recently() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let open = limit_intent(decision, "live-open", BASE);
    let ended = limit_intent(decision, "live-rejected", BASE + 1);
    for intent in [&open, &ended] {
        order_event::insert(&client, &OrderEvent::intent(intent))
            .await
            .unwrap();
    }
    // A never-arrived verdict: rejected, with no broker order id.
    let rejected = OrderEvent::of(
        &ended,
        OrderState::Rejected,
        None,
        Quantity::from("0"),
        micros(BASE + 5),
    );
    order_event::insert(&client, &rejected).await.unwrap();

    // Without a window only the open order is loaded.
    let only_open = order_event::load_live(&client, None).await.unwrap();
    assert_eq!(only_open.len(), 1);
    assert_eq!(only_open[0].intent.client_order_id, open.client_order_id);

    // With a window that started before the rows were stored, the ended order comes too.
    let long_ago = UnixNanos::from(1_000_000_000_000_000_000);
    let both = order_event::load_live(&client, Some(long_ago))
        .await
        .unwrap();
    assert_eq!(both.len(), 2);
    let restored = both
        .iter()
        .find(|o| o.intent.client_order_id == ended.client_order_id)
        .unwrap();
    assert_eq!(restored.state, OrderState::Rejected);
    assert_eq!(restored.venue_order_id, None);

    // With a window that starts in the future, it does not.
    let far_future = UnixNanos::from(4_000_000_000_000_000_000);
    let none_recent = order_event::load_live(&client, Some(far_future))
        .await
        .unwrap();
    assert_eq!(none_recent.len(), 1);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_second_intent_for_one_client_order_id_is_refused() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "dup-1", BASE);
    order_event::insert(&client, &OrderEvent::intent(&intent))
        .await
        .unwrap();
    let again = order_event::insert(&client, &OrderEvent::intent(&intent)).await;
    assert!(matches!(again, Err(StoreError::Db(_))));
    db.drop_db().await.unwrap();
}

/// The expected `FillSide` for a test fill at `us` microseconds.
fn side_of(symbol: &str, side: OrderSide, us: u64) -> FillSide {
    FillSide {
        symbol: Symbol::new_checked(symbol).unwrap(),
        side,
        ts: micros(us),
    }
}

#[tokio::test]
async fn recent_sides_returns_each_fill_with_its_order_symbol_and_side_in_id_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    // Order 1 is a buy of AAA; order 2 is a sell of BBB. Both have later rows too.
    let buy = OrderIntent {
        symbol: Symbol::new_checked("AAA").unwrap(),
        side: OrderSide::Buy,
        ..limit_intent(decision, "order-1", BASE)
    };
    let sell = OrderIntent {
        symbol: Symbol::new_checked("BBB").unwrap(),
        side: OrderSide::Sell,
        ..limit_intent(decision, "order-2", BASE)
    };
    put(&client, &OrderEvent::intent(&buy)).await;
    put(&client, &OrderEvent::intent(&sell)).await;
    put(
        &client,
        &row(
            &buy,
            OrderState::PartiallyFilled,
            Some("V-1"),
            "2",
            BASE + 1,
        ),
    )
    .await;
    put(
        &client,
        &row(&sell, OrderState::Filled, Some("V-2"), "1", BASE + 2),
    )
    .await;

    // Fills at t1 < t2 < t3, two of them for order 1.
    fill::insert(&client, &fill_of("order-1", "T-1", "10", "1", BASE + 1))
        .await
        .unwrap();
    fill::insert(&client, &fill_of("order-2", "T-2", "20", "1", BASE + 2))
        .await
        .unwrap();
    fill::insert(&client, &fill_of("order-1", "T-3", "11", "1", BASE + 3))
        .await
        .unwrap();

    let sides = fill::recent_sides(&client, micros(BASE)).await.unwrap();
    assert_eq!(
        sides,
        vec![
            side_of("AAA", OrderSide::Buy, BASE + 1),
            side_of("BBB", OrderSide::Sell, BASE + 2),
            side_of("AAA", OrderSide::Buy, BASE + 3),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn recent_sides_since_is_inclusive_and_excludes_earlier_fills() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    for (n, us) in [(1, BASE + 1), (2, BASE + 2), (3, BASE + 3)] {
        fill::insert(
            &client,
            &fill_of("order-1", &format!("T-{n}"), "10", "1", us),
        )
        .await
        .unwrap();
    }

    let from_t2 = fill::recent_sides(&client, micros(BASE + 2)).await.unwrap();
    assert_eq!(
        from_t2,
        vec![
            side_of("SPY", OrderSide::Buy, BASE + 2),
            side_of("SPY", OrderSide::Buy, BASE + 3),
        ]
    );

    let after_t3 = fill::recent_sides(&client, micros(BASE + 4)).await.unwrap();
    assert!(after_t3.is_empty());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn recent_sides_with_no_fills_is_empty() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;

    assert!(
        fill::recent_sides(&client, micros(BASE))
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn recent_sides_refuses_a_fill_whose_order_has_no_event_row() {
    // There is no foreign key from fill to order_event, so the fill row can exist alone.
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    fill::insert(&client, &fill_of("ghost-1", "T-1", "10", "1", BASE + 1))
        .await
        .unwrap();

    let err = fill::recent_sides(&client, micros(BASE)).await.unwrap_err();
    assert!(matches!(err, StoreError::Data(_)), "got {err:?}");
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn recent_sides_takes_symbol_and_side_from_the_first_row_of_the_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = OrderIntent {
        symbol: Symbol::new_checked("AAA").unwrap(),
        side: OrderSide::Buy,
        ..limit_intent(decision, "order-1", BASE)
    };
    put(&client, &OrderEvent::intent(&intent)).await;
    // A later row for the same order that names another symbol and side must not win.
    let later = OrderIntent {
        symbol: Symbol::new_checked("ZZZ").unwrap(),
        side: OrderSide::Sell,
        ..limit_intent(decision, "order-1", BASE)
    };
    put(
        &client,
        &row(&later, OrderState::Accepted, Some("V-1"), "0", BASE + 1),
    )
    .await;
    fill::insert(&client, &fill_of("order-1", "T-1", "10", "1", BASE + 2))
        .await
        .unwrap();

    let sides = fill::recent_sides(&client, micros(BASE)).await.unwrap();
    assert_eq!(sides, vec![side_of("AAA", OrderSide::Buy, BASE + 2)]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn store_recent_fill_sides_agrees_with_the_table_function() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;
    let intent = OrderIntent {
        symbol: Symbol::new_checked("AAA").unwrap(),
        side: OrderSide::Sell,
        ..limit_intent(decision, "order-1", BASE)
    };
    put(&client, &OrderEvent::intent(&intent)).await;
    fill::insert(&client, &fill_of("order-1", "T-1", "10", "1", BASE + 1))
        .await
        .unwrap();

    let store = Store::new(db.client().await.unwrap());
    let via_store = store.recent_fill_sides(micros(BASE)).await.unwrap();
    let direct = fill::recent_sides(&client, micros(BASE)).await.unwrap();
    assert_eq!(via_store, direct);
    assert_eq!(via_store, vec![side_of("AAA", OrderSide::Sell, BASE + 1)]);
    db.drop_db().await.unwrap();
}
