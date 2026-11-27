#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

//! Acceptance tests P4: the engine's start-up reads (`engine_step`, newest `position_event`
//! per symbol, newest `limits_loaded` hash, `ENGINE_LOCK`) against a real Postgres
//! (`TEST_DATABASE_URL`).

use nisse_core::nautilus::{Symbol, UnixNanos};
use nisse_core::{PositionEvent, RiskEvent, RiskEventKind};
use nisse_store::{
    StoreError,
    tables::{
        engine_step::{self, StepStatus},
        position_event, risk_event, scoring,
    },
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds (the store keeps microseconds).
const T0: u64 = 1_767_600_000_123_456_000;
const SECOND: u64 = 1_000_000_000;

fn symbol(text: &str) -> Symbol {
    Symbol::new_checked(text).unwrap()
}

fn position(sym: &str, qty: &str, cost: &str, ts: u64) -> PositionEvent {
    PositionEvent {
        symbol: symbol(sym),
        qty_after: qty.parse().unwrap(),
        avg_cost: cost.parse().unwrap(),
        ts: UnixNanos::from(ts),
    }
}

fn risk(kind: RiskEventKind, detail: Value, ts: u64) -> RiskEvent {
    RiskEvent {
        kind,
        detail,
        ts: UnixNanos::from(ts),
    }
}

// ---- engine_step -------------------------------------------------------------------------

#[tokio::test]
async fn a_step_reads_back_its_newest_status_and_detail() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let run = "session-close/2026-01-05";
    let ts = UnixNanos::from(T0);

    // No row yet.
    assert_eq!(
        engine_step::latest(&client, run, "ingest").await.unwrap(),
        None
    );

    let started = json!({});
    engine_step::insert(&client, run, "ingest", StepStatus::Started, &started, ts)
        .await
        .unwrap();
    let (status, detail) = engine_step::latest(&client, run, "ingest")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StepStatus::Started);
    assert_eq!(detail, started);

    let done = json!({"bars": 390, "symbols": ["AAPL", "SPY"], "nested": {"ok": true}});
    engine_step::insert(&client, run, "ingest", StepStatus::Done, &done, ts)
        .await
        .unwrap();
    assert_eq!(
        engine_step::latest(&client, run, "ingest").await.unwrap(),
        Some((StepStatus::Done, done))
    );

    // A later failure of the same step wins over the earlier done.
    let failed = json!({"error": "timeout"});
    engine_step::insert(&client, run, "ingest", StepStatus::Failed, &failed, ts)
        .await
        .unwrap();
    assert_eq!(
        engine_step::latest(&client, run, "ingest").await.unwrap(),
        Some((StepStatus::Failed, failed))
    );

    // A different step of the same run, and the same step of another run, are independent.
    assert_eq!(
        engine_step::latest(&client, run, "rebalance")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        engine_step::latest(&client, "session-close/2026-01-06", "ingest")
            .await
            .unwrap(),
        None
    );
    engine_step::insert(
        &client,
        run,
        "rebalance",
        StepStatus::Done,
        &json!({"orders": 0}),
        ts,
    )
    .await
    .unwrap();
    assert_eq!(
        engine_step::latest(&client, run, "ingest")
            .await
            .unwrap()
            .map(|(status, _)| status),
        Some(StepStatus::Failed)
    );
    assert_eq!(
        engine_step::latest(&client, run, "rebalance")
            .await
            .unwrap()
            .map(|(status, _)| status),
        Some(StepStatus::Done)
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_non_object_detail_is_refused_and_nothing_is_stored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let run = "session-close/2026-01-05";

    let err = engine_step::insert(
        &client,
        run,
        "ingest",
        StepStatus::Done,
        &json!([]),
        UnixNanos::from(T0),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(
        engine_step::latest(&client, run, "ingest").await.unwrap(),
        None
    );
    db.drop_db().await.unwrap();
}

// ---- position_event ----------------------------------------------------------------------

#[tokio::test]
async fn positions_are_empty_on_an_empty_table() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    assert!(
        position_event::latest_per_symbol(&client)
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn positions_keep_the_newest_row_per_symbol_sorted_by_symbol() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    // Inserted out of symbol order, with older rows that must be superseded.
    let rows = vec![
        position("MSFT", "5", "300.10", T0),
        position("AAPL", "10", "180.25", T0 + SECOND),
        position("MSFT", "7.5", "301.00", T0 + 2 * SECOND),
        position("AAPL", "12.345678", "181.999", T0 + 3 * SECOND),
        position("MSFT", "7.5", "302.5", T0 + 4 * SECOND),
    ];
    position_event::insert_many(&client, &rows).await.unwrap();

    let got = position_event::latest_per_symbol(&client).await.unwrap();
    assert_eq!(
        got,
        vec![
            position("AAPL", "12.345678", "181.999", T0 + 3 * SECOND),
            position("MSFT", "7.5", "302.5", T0 + 4 * SECOND),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_symbol_whose_newest_row_is_flat_is_left_out() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    // SPY was long, then closed: its older non-zero row must not come back.
    let rows = vec![
        position("SPY", "20", "450.00", T0),
        position("AAPL", "3", "180.00", T0 + SECOND),
        position("SPY", "0", "0", T0 + 2 * SECOND),
    ];
    position_event::insert_many(&client, &rows).await.unwrap();

    let got = position_event::latest_per_symbol(&client).await.unwrap();
    assert_eq!(got, vec![position("AAPL", "3", "180.00", T0 + SECOND)]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn short_positions_and_decimals_round_trip_exactly() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let short = position("QQQ", "-4.000000001", "412.123456789", T0);
    position_event::insert_many(&client, std::slice::from_ref(&short))
        .await
        .unwrap();
    let got = position_event::latest_per_symbol(&client).await.unwrap();
    assert_eq!(got, vec![short]);
    assert_eq!(got[0].qty_after, "-4.000000001".parse::<Decimal>().unwrap());
    assert_eq!(got[0].avg_cost.to_string(), "412.123456789");
    db.drop_db().await.unwrap();
}

// ---- risk_event: limits hash -------------------------------------------------------------

#[tokio::test]
async fn the_limits_hash_is_none_until_a_limits_file_is_loaded() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    assert_eq!(risk_event::latest_limits_hash(&client).await.unwrap(), None);

    // Other kinds alone do not make a limits hash.
    let other = risk(
        RiskEventKind::KillSwitch,
        json!({"engaged": false, "by": "owner"}),
        T0,
    );
    risk_event::insert_many(&client, std::slice::from_ref(&other))
        .await
        .unwrap();
    assert_eq!(risk_event::latest_limits_hash(&client).await.unwrap(), None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_limits_hash_is_the_newest_limits_loaded_row() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let rows = vec![
        risk(RiskEventKind::LimitsLoaded, json!({"hash": "aaaa1111"}), T0),
        risk(
            RiskEventKind::KillSwitch,
            json!({"engaged": true, "by": "owner"}),
            T0 + SECOND,
        ),
        risk(
            RiskEventKind::DrawdownMode,
            json!({"mode": "halved", "equity": "92000.10", "reset": false}),
            T0 + 2 * SECOND,
        ),
    ];
    risk_event::insert_many(&client, &rows).await.unwrap();
    assert_eq!(
        risk_event::latest_limits_hash(&client)
            .await
            .unwrap()
            .as_deref(),
        Some("aaaa1111")
    );

    // A newer limits file, with other kinds between, wins.
    let newer = vec![
        risk(
            RiskEventKind::EquityPeak,
            json!({"peak": "101250.75"}),
            T0 + 3 * SECOND,
        ),
        risk(
            RiskEventKind::LimitsLoaded,
            json!({"hash": "bbbb2222"}),
            T0 + 4 * SECOND,
        ),
        risk(
            RiskEventKind::KillSwitch,
            json!({"engaged": false, "by": "owner"}),
            T0 + 5 * SECOND,
        ),
    ];
    risk_event::insert_many(&client, &newer).await.unwrap();
    assert_eq!(
        risk_event::latest_limits_hash(&client)
            .await
            .unwrap()
            .as_deref(),
        Some("bbbb2222")
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_newest_limits_row_without_a_string_hash_is_a_data_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let older = risk(RiskEventKind::LimitsLoaded, json!({"hash": "aaaa1111"}), T0);
    let missing = risk(
        RiskEventKind::LimitsLoaded,
        json!({"limits": {"max_position": 100}}),
        T0 + SECOND,
    );
    risk_event::insert_many(&client, std::slice::from_ref(&older))
        .await
        .unwrap();
    risk_event::insert_many(&client, std::slice::from_ref(&missing))
        .await
        .unwrap();
    let err = risk_event::latest_limits_hash(&client).await.unwrap_err();
    assert!(matches!(err, StoreError::Data(_)), "got {err:?}");

    // A hash that is not a string is refused the same way.
    let not_text = risk(
        RiskEventKind::LimitsLoaded,
        json!({"hash": 1234}),
        T0 + 2 * SECOND,
    );
    risk_event::insert_many(&client, std::slice::from_ref(&not_text))
        .await
        .unwrap();
    let err = risk_event::latest_limits_hash(&client).await.unwrap_err();
    assert!(matches!(err, StoreError::Data(_)), "got {err:?}");
    db.drop_db().await.unwrap();
}

// ---- ENGINE_LOCK -------------------------------------------------------------------------

#[tokio::test]
async fn the_engine_lock_is_distinct_and_held_by_one_connection() {
    assert_ne!(scoring::ENGINE_LOCK, scoring::SCORE_JOB_LOCK);
    assert_ne!(scoring::ENGINE_LOCK, scoring::HOLDOUT_LOCK);

    let db = ScratchDb::create().await;
    let first = db.client().await.unwrap();
    let second = db.client().await.unwrap();

    assert!(
        scoring::try_advisory_lock(&first, scoring::ENGINE_LOCK)
            .await
            .unwrap()
    );
    assert!(
        !scoring::try_advisory_lock(&second, scoring::ENGINE_LOCK)
            .await
            .unwrap()
    );
    assert!(
        scoring::advisory_unlock(&first, scoring::ENGINE_LOCK)
            .await
            .unwrap()
    );
    assert!(
        scoring::try_advisory_lock(&second, scoring::ENGINE_LOCK)
            .await
            .unwrap()
    );
    db.drop_db().await.unwrap();
}
