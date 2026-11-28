//! Acceptance tests for the tracking reads and the live trial row (`trial_registry::insert_live`,
//! `live_start`, `scoring::tracking_result`) against a real Postgres (`TEST_DATABASE_URL`).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use chrono::{DateTime, Utc};
use nisse_core::nautilus::UnixNanos;
use nisse_core::{StrategyVersion, StrategyVersionId, Trial, TrialKind, config_hash};
use nisse_store::tables::scoring::{self, TrialWithResult};
use nisse_store::tables::{strategy_version, trial_registry, trial_result};
use nisse_store::testing::ScratchDb;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// A whole-microsecond base time (2022-04-19), so timestamps round-trip exactly.
const BASE: u64 = 1_650_390_305_000_000;
const DAY_MICROS: u64 = 86_400 * 1_000_000;

fn micros(us: u64) -> UnixNanos {
    UnixNanos::from(us * 1_000)
}

/// Ensures a strategy version for `git` (a single hex digit) and config `cfg`. Two calls with
/// the same `cfg` and different `git` are two builds of one config.
async fn version(client: &Client, git: char, cfg: &str) -> StrategyVersionId {
    let params = json!({"strategy": cfg, "lookback": 252});
    let version = StrategyVersion {
        git_sha: git.to_string().repeat(40),
        config_hash: config_hash(&params),
        params,
        parent_id: None,
        ts: micros(BASE),
    };
    strategy_version::ensure(client, &version).await.unwrap()
}

/// Registers a backtest trial with an optional result stored on the registry row.
async fn backtest(
    client: &Client,
    version: StrategyVersionId,
    result: Option<Value>,
    ts: u64,
) -> i64 {
    let trial = Trial {
        strategy_version_id: version,
        kind: TrialKind::Backtest,
        data_start: micros(BASE),
        data_end: micros(BASE + DAY_MICROS),
        result,
        ts: micros(ts),
    };
    trial_registry::insert(client, &trial).await.unwrap()
}

/// Registers a tracking run: a backtest trial whose registry row carries `"tracking": true`.
async fn tracking(client: &Client, version: StrategyVersionId, ts: u64) -> i64 {
    backtest(client, version, Some(json!({"tracking": true})), ts).await
}

/// Registers a live trial for `environment` starting at `start`.
async fn live(
    client: &Client,
    version: StrategyVersionId,
    environment: &str,
    start: u64,
    ts: u64,
) -> i64 {
    trial_registry::insert_live(client, version, environment, micros(start), micros(ts))
        .await
        .unwrap()
}

#[tokio::test]
async fn insert_live_registers_an_open_ended_live_trial() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    let id = live(&client, v, "paper", BASE, BASE + 1).await;

    let row = client
        .query_one(
            "SELECT kind::text, upper_inf(data_range), lower(data_range), result \
             FROM trial_registry WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let kind: String = row.get(0);
    let open_ended: bool = row.get(1);
    let start: DateTime<Utc> = row.get(2);
    let result: Value = row.get(3);
    assert_eq!(kind, "live");
    assert!(open_ended);
    assert_eq!(start.timestamp_micros(), BASE as i64);
    assert_eq!(result, json!({"environment": "paper"}));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_live_trial_counts_in_the_lifetime_count() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    let before = trial_registry::lifetime_count(&client).await.unwrap();
    live(&client, v, "paper", BASE, BASE + 1).await;
    let after = trial_registry::lifetime_count(&client).await.unwrap();
    assert_eq!(after, before + 1);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_start_is_none_without_a_live_trial() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;
    backtest(&client, v, None, BASE + 1).await;

    assert_eq!(
        trial_registry::live_start(&client, v, None).await.unwrap(),
        None
    );
    assert_eq!(
        trial_registry::live_start(&client, v, Some("paper"))
            .await
            .unwrap(),
        None
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_start_is_the_oldest_live_trial_of_the_config() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    live(&client, v, "paper", BASE + DAY_MICROS, BASE + 1).await;
    live(&client, v, "paper", BASE + 2 * DAY_MICROS, BASE + 2).await;

    assert_eq!(
        trial_registry::live_start(&client, v, None).await.unwrap(),
        Some(micros(BASE + DAY_MICROS))
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_start_follows_the_config_across_builds() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let old_build = version(&client, '1', "trend").await;
    let new_build = version(&client, '2', "trend").await;
    assert_ne!(old_build, new_build);

    live(&client, old_build, "paper", BASE, BASE + 1).await;

    assert_eq!(
        trial_registry::live_start(&client, new_build, None)
            .await
            .unwrap(),
        Some(micros(BASE))
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_start_ignores_another_config() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let trend = version(&client, '1', "trend").await;
    let revert = version(&client, '1', "revert").await;

    live(&client, trend, "paper", BASE, BASE + 1).await;

    assert_eq!(
        trial_registry::live_start(&client, revert, None)
            .await
            .unwrap(),
        None
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_start_filters_by_environment() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    live(&client, v, "paper", BASE, BASE + 1).await;

    assert_eq!(
        trial_registry::live_start(&client, v, Some("paper"))
            .await
            .unwrap(),
        Some(micros(BASE))
    );
    assert_eq!(
        trial_registry::live_start(&client, v, Some("live"))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        trial_registry::live_start(&client, v, None).await.unwrap(),
        Some(micros(BASE))
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn backtest_results_leave_tracking_runs_out() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    let ordinary = backtest(&client, v, Some(json!({"sharpe": 0.4})), BASE + 1).await;
    trial_result::insert(&client, ordinary, &json!({"sharpe": 0.4}), micros(BASE + 2))
        .await
        .unwrap();

    let tracked = tracking(&client, v, BASE + 3).await;
    trial_result::insert(&client, tracked, &json!({"sharpe": 1.1}), micros(BASE + 4))
        .await
        .unwrap();

    let results = scoring::backtest_results(&client).await.unwrap();
    assert_eq!(
        results,
        vec![TrialWithResult {
            trial_id: ordinary,
            strategy_version_id: v,
            holdout: false,
            result: json!({"sharpe": 0.4}),
            ts: micros(BASE + 1),
        }]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn tracking_result_is_the_newest_tracking_run_of_the_config() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;
    let other_build = version(&client, '2', "trend").await;

    let older = tracking(&client, v, BASE + 1).await;
    trial_result::insert(&client, older, &json!({"n": 1}), micros(BASE + 2))
        .await
        .unwrap();
    let newer = tracking(&client, other_build, BASE + 3).await;
    trial_result::insert(&client, newer, &json!({"n": 2}), micros(BASE + 4))
        .await
        .unwrap();

    // An ordinary backtest with a result, registered last: never a tracking result.
    let ordinary = backtest(&client, v, None, BASE + 5).await;
    trial_result::insert(&client, ordinary, &json!({"n": 3}), micros(BASE + 6))
        .await
        .unwrap();

    let got = scoring::tracking_result(&client, v).await.unwrap();
    assert_eq!(
        got,
        Some(TrialWithResult {
            trial_id: newer,
            strategy_version_id: other_build,
            holdout: false,
            result: json!({"n": 2}),
            ts: micros(BASE + 3),
        })
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn tracking_result_is_none_without_a_stored_result() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;
    tracking(&client, v, BASE + 1).await;

    assert_eq!(scoring::tracking_result(&client, v).await.unwrap(), None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_is_not_a_holdout_look() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, '1', "trend").await;

    let before = trial_registry::holdout_count(&client).await.unwrap();
    tracking(&client, v, BASE + 1).await;
    let after = trial_registry::holdout_count(&client).await.unwrap();
    assert_eq!(after, before);
    db.drop_db().await.unwrap();
}
