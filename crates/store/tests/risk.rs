#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

//! Acceptance tests S2: `risk_event` and `heartbeat` against a real Postgres
//! (`TEST_DATABASE_URL`).

use nisse_core::nautilus::{Symbol, UnixNanos};
use nisse_core::{
    KillSwitchSource, PositionEvent, RecordSink, RiskEvent, RiskEventKind, StoreRecord,
};
use nisse_store::{
    Store, StoreError, StoreWriter, WriterConfig,
    tables::{heartbeat, risk_event},
    testing::ScratchDb,
};
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds (the store keeps microseconds).
const T0: u64 = 1_767_600_000_123_456_000;
const MICRO: u64 = 1_000;
const ONE_SECOND: u64 = 1_000_000_000;

fn event(kind: RiskEventKind, detail: Value, ts: u64) -> RiskEvent {
    RiskEvent {
        kind,
        detail,
        ts: UnixNanos::from(ts),
    }
}

/// One event of each kind, distinct details and timestamps.
fn four_events() -> Vec<RiskEvent> {
    vec![
        event(RiskEventKind::EquityPeak, json!({"peak": "101250.75"}), T0),
        event(
            RiskEventKind::DrawdownMode,
            json!({"mode": "halved", "equity": "92000.10", "reset": false}),
            T0 + ONE_SECOND,
        ),
        event(
            RiskEventKind::KillSwitch,
            json!({"engaged": true, "by": "owner"}),
            T0 + 2 * ONE_SECOND,
        ),
        event(
            RiskEventKind::LimitsLoaded,
            json!({"hash": "ab12cd34", "limits": {"max_position": 100}}),
            T0 + 3 * ONE_SECOND,
        ),
    ]
}

async fn count(client: &Client, table: &str) -> i64 {
    client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn insert_many_then_load_all_returns_the_same_events_in_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let batch = four_events();
    risk_event::insert_many(&client, &batch).await.unwrap();
    assert_eq!(risk_event::load_all(&client).await.unwrap(), batch);

    let second = vec![event(
        RiskEventKind::KillSwitch,
        json!({"engaged": false, "by": "owner"}),
        T0 + 4 * ONE_SECOND,
    )];
    risk_event::insert_many(&client, &second).await.unwrap();

    let mut expected = batch;
    expected.extend(second);
    assert_eq!(risk_event::load_all(&client).await.unwrap(), expected);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn empty_slice_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    risk_event::insert_many(&client, &[]).await.unwrap();

    assert_eq!(count(&client, "risk_event").await, 0);
    assert!(risk_event::load_all(&client).await.unwrap().is_empty());
    assert!(!risk_event::kill_switch_engaged(&client).await.unwrap());

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn kill_switch_follows_the_newest_kill_switch_row() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    assert!(!risk_event::kill_switch_engaged(&client).await.unwrap());

    risk_event::insert_many(
        &client,
        &[event(
            RiskEventKind::KillSwitch,
            json!({"engaged": true, "by": "owner"}),
            T0,
        )],
    )
    .await
    .unwrap();
    assert!(risk_event::kill_switch_engaged(&client).await.unwrap());

    // Other kinds in between do not change the switch.
    risk_event::insert_many(
        &client,
        &[
            event(
                RiskEventKind::EquityPeak,
                json!({"peak": "1"}),
                T0 + ONE_SECOND,
            ),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "flat", "equity": "0.5", "reset": false}),
                T0 + 2 * ONE_SECOND,
            ),
        ],
    )
    .await
    .unwrap();
    assert!(risk_event::kill_switch_engaged(&client).await.unwrap());

    risk_event::insert_many(
        &client,
        &[event(
            RiskEventKind::KillSwitch,
            json!({"engaged": false, "by": "owner"}),
            T0 + 3 * ONE_SECOND,
        )],
    )
    .await
    .unwrap();
    assert!(!risk_event::kill_switch_engaged(&client).await.unwrap());

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn newest_kill_switch_row_without_a_boolean_is_an_error() {
    for bad in [json!({"engaged": "yes"}), json!({})] {
        let db = ScratchDb::create().await;
        let client = db.client().await.unwrap();

        risk_event::insert_many(
            &client,
            &[event(
                RiskEventKind::KillSwitch,
                json!({"engaged": false}),
                T0,
            )],
        )
        .await
        .unwrap();
        risk_event::insert_many(
            &client,
            &[event(
                RiskEventKind::KillSwitch,
                bad.clone(),
                T0 + ONE_SECOND,
            )],
        )
        .await
        .unwrap();

        let result = risk_event::kill_switch_engaged(&client).await;
        assert!(
            matches!(result, Err(StoreError::Data(_))),
            "detail {bad}: {result:?}"
        );

        db.drop_db().await.unwrap();
    }
}

#[tokio::test]
async fn an_old_malformed_kill_switch_row_does_not_affect_the_newest() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    risk_event::insert_many(
        &client,
        &[
            event(RiskEventKind::KillSwitch, json!({"engaged": "yes"}), T0),
            event(
                RiskEventKind::KillSwitch,
                json!({"engaged": true}),
                T0 + ONE_SECOND,
            ),
        ],
    )
    .await
    .unwrap();

    assert!(risk_event::kill_switch_engaged(&client).await.unwrap());

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn store_methods_agree_with_the_table_functions() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());

    let engage = event(
        RiskEventKind::KillSwitch,
        json!({"engaged": true, "by": "owner"}),
        T0,
    );
    store.record_risk_event(&engage).await.unwrap();
    store
        .record_risk_event(&event(
            RiskEventKind::EquityPeak,
            json!({"peak": "100000"}),
            T0 + ONE_SECOND,
        ))
        .await
        .unwrap();

    let loaded = store.load_risk_events().await.unwrap();
    assert_eq!(loaded, risk_event::load_all(store.client()).await.unwrap());
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0], engage);

    assert!(store.kill_switch_engaged().await.unwrap());
    assert_eq!(
        store.kill_switch_engaged().await.unwrap(),
        risk_event::kill_switch_engaged(store.client())
            .await
            .unwrap()
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn heartbeat_latest_is_the_newest_per_source() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let t1 = UnixNanos::from(T0);
    let t2 = UnixNanos::from(T0 + ONE_SECOND);
    let t3 = UnixNanos::from(T0 + 2 * ONE_SECOND);

    assert_eq!(heartbeat::latest(&client, "engine").await.unwrap(), None);

    heartbeat::insert(&client, "engine", t1).await.unwrap();
    heartbeat::insert(&client, "engine", t2).await.unwrap();
    heartbeat::insert(&client, "other", t3).await.unwrap();

    assert_eq!(
        heartbeat::latest(&client, "engine").await.unwrap(),
        Some(t2)
    );
    assert_eq!(heartbeat::latest(&client, "other").await.unwrap(), Some(t3));

    // An older time inserted later does not move the newest.
    heartbeat::insert(&client, "engine", t1).await.unwrap();
    assert_eq!(
        heartbeat::latest(&client, "engine").await.unwrap(),
        Some(t2)
    );
    assert_eq!(heartbeat::latest(&client, "unknown").await.unwrap(), None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn heartbeat_with_an_empty_source_is_an_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let result = heartbeat::insert(&client, "", UnixNanos::from(T0)).await;
    assert!(matches!(result, Err(StoreError::Db(_))), "{result:?}");
    assert_eq!(count(&client, "heartbeat").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn store_heartbeat_writes_the_row() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());

    store
        .heartbeat("engine", UnixNanos::from(T0))
        .await
        .unwrap();
    assert_eq!(
        heartbeat::latest(store.client(), "engine").await.unwrap(),
        Some(UnixNanos::from(T0))
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn writer_stores_risk_events_alone_and_in_a_mixed_batch() {
    let db = ScratchDb::create().await;
    let writer_client = db.client().await.unwrap();
    let client = db.client().await.unwrap();

    let (writer, handle) = StoreWriter::spawn(writer_client, WriterConfig::default());
    let first = four_events();
    for row in &first {
        writer
            .emit(StoreRecord::RiskEvent(row.clone()))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();
    assert_eq!(risk_event::load_all(&client).await.unwrap(), first);

    // A risk event and a position in one batch: both commit.
    let mixed = event(
        RiskEventKind::EquityPeak,
        json!({"peak": "102000"}),
        T0 + 10 * ONE_SECOND,
    );
    writer
        .emit(StoreRecord::RiskEvent(mixed.clone()))
        .await
        .unwrap();
    writer
        .emit(StoreRecord::Position(PositionEvent {
            symbol: Symbol::new_checked("SPY").unwrap(),
            qty_after: "10".parse().unwrap(),
            avg_cost: "500.00".parse().unwrap(),
            ts: UnixNanos::from(T0 + 10 * ONE_SECOND),
        }))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    assert_eq!(count(&client, "position_event").await, 1);
    let mut expected = first;
    expected.push(mixed);
    assert_eq!(risk_event::load_all(&client).await.unwrap(), expected);

    drop(writer);
    handle.await.unwrap().unwrap();
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn timestamps_round_trip_exactly_at_microsecond_precision() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    // Whole microseconds: the first, a value just under one second later, and a small one.
    let stamps = [T0, T0 + 999_999 * MICRO, 1_000 * MICRO];
    let rows: Vec<RiskEvent> = stamps
        .iter()
        .map(|ts| event(RiskEventKind::LimitsLoaded, json!({"hash": "00"}), *ts))
        .collect();
    risk_event::insert_many(&client, &rows).await.unwrap();

    let loaded = risk_event::load_all(&client).await.unwrap();
    let read: Vec<u64> = loaded.iter().map(|row| row.ts.as_u64()).collect();
    assert_eq!(read, stamps.to_vec());

    // Heartbeats keep the same precision.
    heartbeat::insert(&client, "engine", UnixNanos::from(T0 + MICRO))
        .await
        .unwrap();
    assert_eq!(
        heartbeat::latest(&client, "engine").await.unwrap(),
        Some(UnixNanos::from(T0 + MICRO))
    );

    db.drop_db().await.unwrap();
}
