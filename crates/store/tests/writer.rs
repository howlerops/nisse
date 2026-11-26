#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

//! Acceptance tests for the batched `StoreWriter` (Phase 2, task S4) against a real Postgres.

use std::{future::Future, time::Duration};

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, TradeId, UnixNanos,
    VenueOrderId,
};
use nisse_core::{
    DecisionId, Fill, MarketSnapshot, OrderEvent, OrderIntent, OrderState, PositionEvent,
    RecordSink, RiskCheck, StoreRecord,
};
use nisse_store::testing::ScratchDb;
use nisse_store::{StoreError, StoreWriter, WriterConfig};
use tokio_postgres::Client;

/// A whole-microsecond base time, so timestamps round-trip exactly.
const BASE: u64 = 1_650_390_305_000_000;

/// Every awaited step that touches the writer or the database must finish within this.
const LIMIT: Duration = Duration::from_secs(10);

/// Fails the test instead of hanging when `future` takes longer than `LIMIT`.
async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("step timed out")
}

fn micros(us: u64) -> UnixNanos {
    UnixNanos::from(us * 1_000)
}

fn symbol(text: &str) -> Symbol {
    Symbol::new_checked(text).unwrap()
}

/// Inserts one strategy version and one decision with plain SQL and returns the decision id.
async fn setup_decision(client: &Client) -> DecisionId {
    let version: i64 = within(client.query_one(
        "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
         VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', \
                 '2026-01-02T00:00:00Z') RETURNING id",
        &[],
    ))
    .await
    .unwrap()
    .get(0);
    let id: i64 = within(client.query_one(
        "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                               feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
         VALUES ($1, 'SPY', 'enter', 1.5, '{}', decode(repeat('ab', 32), 'hex'), 'snap-1', \
                 42, 'trend_up', '2026-01-05T21:00:00Z') RETURNING id",
        &[&version],
    ))
    .await
    .unwrap()
    .get(0);
    DecisionId(id)
}

/// A market snapshot for `symbol_text` at `ts` microseconds.
fn snapshot(symbol_text: &str, ts: u64) -> StoreRecord {
    StoreRecord::Snapshot(MarketSnapshot {
        symbol: symbol(symbol_text),
        bid: Some("100.25".parse().unwrap()),
        ask: Some("100.27".parse().unwrap()),
        mid: Some("100.26".parse().unwrap()),
        spread_bps: Some(2.0),
        adv: None,
        vol: None,
        regime_tag: None,
        ts: micros(ts),
    })
}

/// A position of `qty` shares in `symbol_text` at `ts` microseconds.
fn position(symbol_text: &str, qty: &str, ts: u64) -> StoreRecord {
    StoreRecord::Position(PositionEvent {
        symbol: symbol(symbol_text),
        qty_after: qty.parse().unwrap(),
        avg_cost: "100.00".parse().unwrap(),
        ts: micros(ts),
    })
}

/// A passed risk check of `rule` for `decision_id` at `ts` microseconds.
fn risk(decision_id: DecisionId, rule: &str, ts: u64) -> StoreRecord {
    StoreRecord::Risk(RiskCheck {
        decision_id,
        rule: rule.to_string(),
        limit_value: "10".parse().unwrap(),
        actual_value: "5".parse().unwrap(),
        passed: true,
        ts: micros(ts),
    })
}

/// An order update for a limit buy of 5 SPY at 500.10, with an optional fill given as
/// `(trade_id, px, qty)`. Every state but `Intent` carries a broker order id.
fn order_update(
    decision_id: DecisionId,
    client_order_id: &str,
    state: OrderState,
    cum: &str,
    fill: Option<(&str, &str, &str)>,
    ts: u64,
) -> StoreRecord {
    let intent = OrderIntent {
        decision_id,
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        symbol: symbol("SPY"),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        qty: Quantity::from("5"),
        limit_px: Some("500.10".parse::<Price>().unwrap()),
        time_in_force: TimeInForce::Day,
        ts: micros(ts),
    };
    let venue_text = format!("V-{client_order_id}");
    let venue = (state != OrderState::Intent).then(|| VenueOrderId::from(venue_text.as_str()));
    let event = OrderEvent::of(&intent, state, venue, Quantity::from(cum), micros(ts));
    let fill = fill.map(|(trade, px, qty)| Fill {
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        trade_id: TradeId::new_checked(trade).unwrap(),
        px: px.parse::<Price>().unwrap(),
        qty: Quantity::from(qty),
        ts: micros(ts),
    });
    StoreRecord::Order { event, fill }
}

async fn count(client: &Client, table: &str) -> i64 {
    within(client.query_one(&format!("SELECT count(*) FROM {table}"), &[]))
        .await
        .unwrap()
        .get(0)
}

/// One text column from `sql`, one row per value, in the order the query returns them.
async fn texts(client: &Client, sql: &str) -> Vec<String> {
    within(client.query(sql, &[]))
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

#[tokio::test]
async fn every_kind_is_written_on_flush() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();
    let decision = setup_decision(&client).await;

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    within(writer.emit(snapshot("SPY", BASE))).await.unwrap();
    within(writer.emit(snapshot("QQQ", BASE + 1)))
        .await
        .unwrap();
    within(writer.emit(position("SPY", "10.5", BASE + 2)))
        .await
        .unwrap();
    within(writer.emit(risk(decision, "max_position", BASE + 3)))
        .await
        .unwrap();
    within(writer.emit(risk(decision, "max_notional", BASE + 4)))
        .await
        .unwrap();
    within(writer.emit(order_update(
        decision,
        "order-1",
        OrderState::Filled,
        "5",
        Some(("T-1", "500.10", "5")),
        BASE + 5,
    )))
    .await
    .unwrap();
    within(writer.flush()).await.unwrap();

    assert_eq!(count(&client, "market_snapshot").await, 2);
    assert_eq!(count(&client, "position_event").await, 1);
    assert_eq!(count(&client, "risk_check").await, 2);
    assert_eq!(count(&client, "order_event").await, 1);
    assert_eq!(count(&client, "fill").await, 1);
    assert_eq!(
        texts(&client, "SELECT symbol FROM market_snapshot ORDER BY id").await,
        vec!["SPY", "QQQ"]
    );
    assert_eq!(
        texts(
            &client,
            "SELECT qty_after::text FROM position_event ORDER BY id"
        )
        .await,
        vec!["10.5"]
    );
    assert_eq!(
        texts(&client, "SELECT rule FROM risk_check ORDER BY id").await,
        vec!["max_position", "max_notional"]
    );
    assert_eq!(
        texts(&client, "SELECT px::text FROM fill").await,
        vec!["500.10"]
    );

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn order_with_fill_writes_both_rows_and_a_repeated_fill_only_once() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();
    let decision = setup_decision(&client).await;

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    within(writer.emit(order_update(
        decision,
        "order-1",
        OrderState::Filled,
        "5",
        Some(("T-1", "500.10", "5")),
        BASE,
    )))
    .await
    .unwrap();
    // The same execution delivered again: the order row is written, the fill is not.
    within(writer.emit(order_update(
        decision,
        "order-1",
        OrderState::Filled,
        "5",
        Some(("T-1", "500.10", "5")),
        BASE + 1,
    )))
    .await
    .unwrap();
    // An order update without a fill writes only its order row.
    within(writer.emit(order_update(
        decision,
        "order-2",
        OrderState::Accepted,
        "0",
        None,
        BASE + 2,
    )))
    .await
    .unwrap();
    within(writer.flush()).await.unwrap();

    assert_eq!(count(&client, "order_event").await, 3);
    assert_eq!(count(&client, "fill").await, 1);
    assert_eq!(
        texts(
            &client,
            "SELECT client_order_id FROM order_event ORDER BY id"
        )
        .await,
        vec!["order-1", "order-1", "order-2"]
    );
    assert_eq!(
        texts(&client, "SELECT broker_fill_id FROM fill").await,
        vec!["T-1"]
    );

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn rows_within_a_table_follow_emit_order() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    let mut expected = Vec::new();
    for i in 0..20u64 {
        let name = format!("S{i:02}");
        within(writer.emit(snapshot(&name, BASE + i)))
            .await
            .unwrap();
        expected.push(name);
    }
    within(writer.flush()).await.unwrap();

    assert_eq!(
        texts(&client, "SELECT symbol FROM market_snapshot ORDER BY id").await,
        expected
    );

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn back_pressure_loses_nothing_and_never_errors() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let config = WriterConfig {
        capacity: 1,
        max_batch: 4,
    };
    let (writer, handle) = StoreWriter::spawn(writer_client, config);
    for i in 0..200u64 {
        within(writer.emit(position("SPY", &i.to_string(), BASE + i)))
            .await
            .expect("emit must not fail while the store is healthy");
    }
    within(writer.flush()).await.unwrap();

    let expected: Vec<String> = (0..200).map(|i| i.to_string()).collect();
    assert_eq!(count(&client, "position_event").await, 200);
    assert_eq!(
        texts(
            &client,
            "SELECT qty_after::text FROM position_event ORDER BY id"
        )
        .await,
        expected
    );

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

/// Both records are emitted before the writer task first runs (the current-thread runtime only
/// runs it when the test awaits, and `emit` on a queue with room does not yield), so `flush`
/// takes them into one batch and the rollback is deterministic.
#[tokio::test(flavor = "current_thread")]
async fn failed_batch_is_rolled_back_and_stops_the_writer() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let config = WriterConfig {
        capacity: 16,
        max_batch: 16,
    };
    let (writer, handle) = StoreWriter::spawn(writer_client, config);
    within(writer.emit(snapshot("SPY", BASE))).await.unwrap();
    within(writer.emit(risk(DecisionId(999_999), "max_position", BASE + 1)))
        .await
        .unwrap();

    let flushed = within(writer.flush()).await;
    assert!(
        matches!(flushed, Err(StoreError::WriterStopped)),
        "flush of a failed batch: {flushed:?}"
    );

    let outcome = within(handle).await;
    assert!(
        matches!(outcome, Ok(Err(StoreError::Db(_)))),
        "writer task outcome: {outcome:?}"
    );

    assert_eq!(count(&client, "market_snapshot").await, 0);
    assert_eq!(count(&client, "risk_check").await, 0);

    let later_emit = within(writer.emit(snapshot("QQQ", BASE + 2))).await;
    assert!(
        matches!(later_emit, Err(StoreError::WriterStopped)),
        "emit after stop: {later_emit:?}"
    );
    let later_flush = within(writer.flush()).await;
    assert!(
        matches!(later_flush, Err(StoreError::WriterStopped)),
        "flush after stop: {later_flush:?}"
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn dropping_every_handle_drains_the_queue_and_ends_ok() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    for i in 0..10u64 {
        within(writer.emit(position("SPY", &i.to_string(), BASE + i)))
            .await
            .unwrap();
    }
    drop(writer);

    let outcome = within(handle).await;
    assert!(
        matches!(outcome, Ok(Ok(()))),
        "writer task outcome: {outcome:?}"
    );
    assert_eq!(count(&client, "position_event").await, 10);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn flush_on_an_idle_writer_returns_promptly() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    tokio::time::timeout(Duration::from_secs(1), writer.flush())
        .await
        .expect("flush on an idle writer took over a second")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), writer.flush())
        .await
        .expect("second flush on an idle writer took over a second")
        .unwrap();

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloned_handles_emitting_from_two_tasks_all_arrive() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let config = WriterConfig {
        capacity: 8,
        max_batch: 16,
    };
    let (writer, handle) = StoreWriter::spawn(writer_client, config);
    let mut tasks = Vec::new();
    for name in ["ALPHA", "BETA"] {
        let clone = writer.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..50u64 {
                clone
                    .emit(position(name, &i.to_string(), BASE + i))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        within(task).await.unwrap();
    }
    within(writer.flush()).await.unwrap();

    assert_eq!(count(&client, "position_event").await, 100);
    let symbols = texts(&client, "SELECT symbol FROM position_event ORDER BY id").await;
    assert_eq!(symbols.iter().filter(|s| *s == "ALPHA").count(), 50);
    assert_eq!(symbols.iter().filter(|s| *s == "BETA").count(), 50);

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn flush_covers_only_what_was_emitted_before_it() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    for i in 0..3u64 {
        within(writer.emit(snapshot("SPY", BASE + i)))
            .await
            .unwrap();
    }
    within(writer.flush()).await.unwrap();
    assert_eq!(count(&client, "market_snapshot").await, 3);

    for i in 3..5u64 {
        within(writer.emit(snapshot("SPY", BASE + i)))
            .await
            .unwrap();
    }
    within(writer.flush()).await.unwrap();
    assert_eq!(count(&client, "market_snapshot").await, 5);

    drop(writer);
    within(handle).await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}
