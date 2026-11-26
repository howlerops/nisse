#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

//! Batched inserts into `market_snapshot` and `position_event` against a real Postgres.

use chrono::{DateTime, Utc};
use nisse_core::{
    MarketSnapshot, PositionEvent,
    nautilus::{Symbol, UnixNanos},
};
use nisse_store::{
    StoreError, tables::market_snapshot, tables::position_event, testing::ScratchDb,
};
use rust_decimal::Decimal;
use tokio_postgres::Client;

/// 2026-01-05T12:00:00.123456Z in nanoseconds (whole microseconds, so it round-trips).
const T0: u64 = 1_767_614_400_123_456_000;
const MICRO: u64 = 1_000;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn text(value: Option<Decimal>) -> Option<String> {
    value.map(|d| d.to_string())
}

fn micros(ts: UnixNanos) -> i64 {
    i64::try_from(ts.as_u64() / MICRO).unwrap()
}

fn ts(offset_us: u64) -> UnixNanos {
    UnixNanos::from(T0 + offset_us * MICRO)
}

/// One `market_snapshot` row as read back. Decimals are compared as text so the scale must
/// survive the round trip too.
#[derive(Debug, PartialEq)]
struct SnapRow {
    symbol: String,
    bid: Option<String>,
    ask: Option<String>,
    mid: Option<String>,
    spread_bps: Option<f64>,
    adv: Option<String>,
    vol: Option<f64>,
    regime_tag: Option<String>,
    event_ts: i64,
}

impl SnapRow {
    fn expected(row: &MarketSnapshot) -> Self {
        Self {
            symbol: row.symbol.to_string(),
            bid: text(row.bid),
            ask: text(row.ask),
            mid: text(row.mid),
            spread_bps: row.spread_bps,
            adv: text(row.adv),
            vol: row.vol,
            regime_tag: row.regime_tag.clone(),
            event_ts: micros(row.ts),
        }
    }
}

async fn read_snapshots(client: &Client) -> Vec<SnapRow> {
    client
        .query(
            "SELECT symbol, bid::numeric, ask::numeric, mid::numeric, spread_bps::float8, \
             adv::numeric, vol::float8, regime_tag, event_ts \
             FROM market_snapshot ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| SnapRow {
            symbol: r.get(0),
            bid: text(r.get::<_, Option<Decimal>>(1)),
            ask: text(r.get::<_, Option<Decimal>>(2)),
            mid: text(r.get::<_, Option<Decimal>>(3)),
            spread_bps: r.get(4),
            adv: text(r.get::<_, Option<Decimal>>(5)),
            vol: r.get(6),
            regime_tag: r.get(7),
            event_ts: r.get::<_, DateTime<Utc>>(8).timestamp_micros(),
        })
        .collect()
}

/// One `position_event` row as read back.
#[derive(Debug, PartialEq)]
struct PosRow {
    symbol: String,
    qty_after: String,
    avg_cost: String,
    event_ts: i64,
}

impl PosRow {
    fn expected(row: &PositionEvent) -> Self {
        Self {
            symbol: row.symbol.to_string(),
            qty_after: row.qty_after.to_string(),
            avg_cost: row.avg_cost.to_string(),
            event_ts: micros(row.ts),
        }
    }
}

async fn read_positions(client: &Client) -> Vec<PosRow> {
    client
        .query(
            "SELECT symbol, qty_after::numeric, avg_cost::numeric, event_ts \
             FROM position_event ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| PosRow {
            symbol: r.get(0),
            qty_after: r.get::<_, Decimal>(1).to_string(),
            avg_cost: r.get::<_, Decimal>(2).to_string(),
            event_ts: r.get::<_, DateTime<Utc>>(3).timestamp_micros(),
        })
        .collect()
}

async fn count(client: &Client, table: &str) -> i64 {
    client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per market_snapshot column keeps the fixtures readable"
)]
fn snap(
    symbol: &str,
    bid: Option<&str>,
    ask: Option<&str>,
    mid: Option<&str>,
    spread_bps: Option<f64>,
    adv: Option<&str>,
    vol: Option<f64>,
    regime_tag: Option<&str>,
    at: UnixNanos,
) -> MarketSnapshot {
    MarketSnapshot {
        symbol: Symbol::new(symbol),
        bid: bid.map(dec),
        ask: ask.map(dec),
        mid: mid.map(dec),
        spread_bps,
        adv: adv.map(dec),
        vol,
        regime_tag: regime_tag.map(str::to_owned),
        ts: at,
    }
}

fn three_snapshots() -> Vec<MarketSnapshot> {
    vec![
        // Every column set, exact decimals.
        snap(
            "SPY",
            Some("500.10"),
            Some("500.12"),
            Some("500.11"),
            Some(0.4),
            Some("1234567.89"),
            Some(0.25),
            Some("risk_on"),
            ts(0),
        ),
        // One-sided quote: no bid; a tiny exact decimal in adv.
        snap(
            "SPY",
            None,
            Some("0.1"),
            None,
            None,
            Some("0.1"),
            None,
            None,
            ts(10),
        ),
        // Every optional column None.
        snap("QQQ", None, None, None, None, None, None, None, ts(20)),
    ]
}

fn three_positions() -> Vec<PositionEvent> {
    vec![
        PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: dec("100"),
            avg_cost: dec("500.10"),
            ts: ts(0),
        },
        // A short position: negative quantity, cost with a tiny exact decimal.
        PositionEvent {
            symbol: Symbol::new("QQQ"),
            qty_after: dec("-3.5"),
            avg_cost: dec("0.1"),
            ts: ts(10),
        },
        PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: dec("0"),
            avg_cost: dec("0"),
            ts: ts(20),
        },
    ]
}

// ---------------------------------------------------------------- market_snapshot

#[tokio::test]
async fn market_snapshot_three_rows_read_back_equal() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = three_snapshots();

    market_snapshot::insert_many(&client, &rows).await.unwrap();

    let stored = read_snapshots(&client).await;
    let expected: Vec<SnapRow> = rows.iter().map(SnapRow::expected).collect();
    assert_eq!(stored.len(), 3);
    for (got, want) in stored.iter().zip(&expected) {
        assert_eq!(got, want);
    }
    assert_eq!(stored[0].bid.as_deref(), Some("500.10"));
    assert_eq!(stored[1].ask.as_deref(), Some("0.1"));
    assert_eq!(stored[1].bid, None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_row_order_follows_slice() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    // Timestamps go backwards and symbols are not sorted: only the slice order can explain
    // the id order.
    let rows = vec![
        snap(
            "ZZZ",
            Some("1"),
            Some("2"),
            None,
            None,
            None,
            None,
            None,
            ts(30),
        ),
        snap(
            "AAA",
            Some("1"),
            Some("2"),
            None,
            None,
            None,
            None,
            None,
            ts(20),
        ),
        snap(
            "MMM",
            Some("1"),
            Some("2"),
            None,
            None,
            None,
            None,
            None,
            ts(10),
        ),
    ];

    market_snapshot::insert_many(&client, &rows).await.unwrap();

    let symbols: Vec<String> = read_snapshots(&client)
        .await
        .into_iter()
        .map(|r| r.symbol)
        .collect();
    assert_eq!(symbols, ["ZZZ", "AAA", "MMM"]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_empty_slice_is_ok_and_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    market_snapshot::insert_many(&client, &[]).await.unwrap();

    assert_eq!(count(&client, "market_snapshot").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_crossed_quote_in_second_row_rejects_whole_batch() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = vec![
        snap(
            "SPY",
            Some("500.10"),
            Some("500.12"),
            None,
            None,
            None,
            None,
            None,
            ts(0),
        ),
        snap(
            "SPY",
            Some("500.13"),
            Some("500.12"),
            None,
            None,
            None,
            None,
            None,
            ts(1),
        ),
    ];

    let err = market_snapshot::insert_many(&client, &rows)
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "market_snapshot").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_nan_spread_is_rejected_by_domain() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = vec![snap(
        "SPY",
        None,
        None,
        None,
        Some(f64::NAN),
        None,
        None,
        None,
        ts(0),
    )];

    let err = market_snapshot::insert_many(&client, &rows)
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "market_snapshot").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_infinite_spread_is_rejected_by_domain() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = vec![snap(
        "SPY",
        None,
        None,
        None,
        Some(f64::INFINITY),
        None,
        None,
        None,
        ts(0),
    )];

    let err = market_snapshot::insert_many(&client, &rows)
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "market_snapshot").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_ingest_ts_is_set_by_database() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    market_snapshot::insert_many(&client, &three_snapshots())
        .await
        .unwrap();

    let not_null: i64 = client
        .query_one(
            "SELECT count(*) FROM market_snapshot WHERE ingest_ts IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(not_null, 3);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn market_snapshot_thousand_rows_in_one_call() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows: Vec<MarketSnapshot> = (0..1_000)
        .map(|i| {
            snap(
                "SPY",
                Some("500.10"),
                Some("500.12"),
                None,
                Some(0.4),
                None,
                None,
                None,
                ts(i),
            )
        })
        .collect();

    market_snapshot::insert_many(&client, &rows).await.unwrap();

    assert_eq!(count(&client, "market_snapshot").await, 1_000);
    db.drop_db().await.unwrap();
}

// ---------------------------------------------------------------- position_event

#[tokio::test]
async fn position_event_three_rows_read_back_equal() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = three_positions();

    position_event::insert_many(&client, &rows).await.unwrap();

    let stored = read_positions(&client).await;
    let expected: Vec<PosRow> = rows.iter().map(PosRow::expected).collect();
    assert_eq!(stored.len(), 3);
    for (got, want) in stored.iter().zip(&expected) {
        assert_eq!(got, want);
    }
    assert_eq!(stored[1].qty_after, "-3.5");
    assert_eq!(stored[0].avg_cost, "500.10");
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn position_event_row_order_follows_slice() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = vec![
        PositionEvent {
            symbol: Symbol::new("ZZZ"),
            qty_after: dec("1"),
            avg_cost: dec("1"),
            ts: ts(30),
        },
        PositionEvent {
            symbol: Symbol::new("AAA"),
            qty_after: dec("2"),
            avg_cost: dec("2"),
            ts: ts(20),
        },
        PositionEvent {
            symbol: Symbol::new("MMM"),
            qty_after: dec("3"),
            avg_cost: dec("3"),
            ts: ts(10),
        },
    ];

    position_event::insert_many(&client, &rows).await.unwrap();

    let symbols: Vec<String> = read_positions(&client)
        .await
        .into_iter()
        .map(|r| r.symbol)
        .collect();
    assert_eq!(symbols, ["ZZZ", "AAA", "MMM"]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn position_event_empty_slice_is_ok_and_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    position_event::insert_many(&client, &[]).await.unwrap();

    assert_eq!(count(&client, "position_event").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn position_event_negative_cost_in_second_row_rejects_whole_batch() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows = vec![
        PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: dec("10"),
            avg_cost: dec("100"),
            ts: ts(0),
        },
        PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: dec("10"),
            avg_cost: dec("-1"),
            ts: ts(1),
        },
    ];

    let err = position_event::insert_many(&client, &rows)
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "position_event").await, 0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn position_event_ingest_ts_is_set_by_database() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    position_event::insert_many(&client, &three_positions())
        .await
        .unwrap();

    let not_null: i64 = client
        .query_one(
            "SELECT count(*) FROM position_event WHERE ingest_ts IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(not_null, 3);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn position_event_thousand_rows_in_one_call() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let rows: Vec<PositionEvent> = (0..1_000)
        .map(|i| PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: Decimal::from(i),
            avg_cost: dec("500.10"),
            ts: ts(i),
        })
        .collect();

    position_event::insert_many(&client, &rows).await.unwrap();

    assert_eq!(count(&client, "position_event").await, 1_000);
    db.drop_db().await.unwrap();
}
