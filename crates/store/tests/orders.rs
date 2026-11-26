//! Acceptance tests for the `order_event` and `fill` tables (Phase 2, task S2).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, TradeId, UnixNanos,
    VenueOrderId,
};
use nisse_core::{DecisionId, Fill, OrderEvent, OrderIntent, OrderState, RestoredOrder};
use nisse_store::StoreError;
use nisse_store::tables::{fill, order_event};
use nisse_store::testing::ScratchDb;
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
async fn latest_is_highest_event_ts_then_highest_id() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let decision = decision_id(&client).await;

    let intent = limit_intent(decision, "order-1", BASE);
    put(&client, &OrderEvent::intent(&intent)).await;
    // Inserted in this order: the later-timestamp rows come first, so the id tie-break decides.
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
            OrderState::Accepted,
            "0",
            None,
            BASE + 5,
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
