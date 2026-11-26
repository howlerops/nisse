//! Acceptance tests TR1: `trial_result` against a real Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::{StrategyVersion, StrategyVersionId, Trial, TrialKind, nautilus::UnixNanos};
use nisse_store::{
    StoreError,
    tables::{strategy_version, trial_registry, trial_result},
    testing::ScratchDb,
};
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds.
const T0: u64 = 1_767_600_000_123_456_000;
const ONE_SECOND: u64 = 1_000_000_000;

/// Ensures one strategy version and registers `n` backtest trials on it.
async fn trials(client: &Client, n: usize) -> Vec<i64> {
    let params = json!({"strategy": "trend_vol_target", "lookback": 252});
    let version = StrategyVersion {
        git_sha: "a".repeat(40),
        config_hash: nisse_core::config_hash(&params),
        params,
        parent_id: None,
        ts: UnixNanos::from(T0),
    };
    let sv: StrategyVersionId = strategy_version::ensure(client, &version).await.unwrap();
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let trial = Trial {
            strategy_version_id: sv,
            kind: TrialKind::Backtest,
            data_start: UnixNanos::from(T0),
            data_end: UnixNanos::from(T0 + 365 * 86_400 * ONE_SECOND),
            result: None,
            ts: UnixNanos::from(T0 + ONE_SECOND),
        };
        ids.push(trial_registry::insert(client, &trial).await.unwrap());
    }
    ids
}

fn nested_result() -> Value {
    json!({
        "sharpe": 0.4,
        "total_return": -0.125,
        "trades": 37,
        "label": "trend_v1",
        "notes": null,
        "equity": {"start": 100000, "end": 112500.5, "points": [1, 2, 3]}
    })
}

#[tokio::test]
async fn insert_then_get_returns_the_same_json() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let ids = trials(&client, 1).await;

    let result = nested_result();
    trial_result::insert(&client, ids[0], &result, UnixNanos::from(T0))
        .await
        .unwrap();

    let stored = trial_result::get(&client, ids[0]).await.unwrap();
    assert_eq!(stored, Some(result));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn get_for_a_trial_without_a_result_is_none() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let ids = trials(&client, 2).await;

    trial_result::insert(&client, ids[0], &json!({"sharpe": 1}), UnixNanos::from(T0))
        .await
        .unwrap();

    assert_eq!(trial_result::get(&client, ids[1]).await.unwrap(), None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn get_for_an_unknown_trial_id_is_none() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    assert_eq!(trial_result::get(&client, 999_999).await.unwrap(), None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn second_result_for_the_same_trial_is_a_db_error_and_keeps_the_first() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let ids = trials(&client, 1).await;

    let first = json!({"sharpe": 0.4});
    trial_result::insert(&client, ids[0], &first, UnixNanos::from(T0))
        .await
        .unwrap();

    let second =
        trial_result::insert(&client, ids[0], &json!({"sharpe": 9}), UnixNanos::from(T0)).await;
    assert!(matches!(second, Err(StoreError::Db(_))));

    assert_eq!(
        trial_result::get(&client, ids[0]).await.unwrap(),
        Some(first)
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn insert_for_an_unknown_trial_id_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let result = trial_result::insert(&client, 999_999, &json!({}), UnixNanos::from(T0)).await;
    assert!(matches!(result, Err(StoreError::Db(_))));
    assert_eq!(trial_result::get(&client, 999_999).await.unwrap(), None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn writing_results_does_not_change_the_lifetime_trial_count() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let ids = trials(&client, 3).await;

    let before = trial_registry::lifetime_count(&client).await.unwrap();
    assert_eq!(before, 3);

    for (i, id) in ids.iter().enumerate() {
        trial_result::insert(&client, *id, &json!({"run": i}), UnixNanos::from(T0))
            .await
            .unwrap();
    }

    assert_eq!(
        trial_registry::lifetime_count(&client).await.unwrap(),
        before
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn non_object_json_results_round_trip() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let ids = trials(&client, 2).await;

    trial_result::insert(&client, ids[0], &json!(null), UnixNanos::from(T0))
        .await
        .unwrap();
    trial_result::insert(&client, ids[1], &json!([1.5, "x"]), UnixNanos::from(T0))
        .await
        .unwrap();

    assert_eq!(
        trial_result::get(&client, ids[0]).await.unwrap(),
        Some(json!(null))
    );
    assert_eq!(
        trial_result::get(&client, ids[1]).await.unwrap(),
        Some(json!([1.5, "x"]))
    );

    db.drop_db().await.unwrap();
}
