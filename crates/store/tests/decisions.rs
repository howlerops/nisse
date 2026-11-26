//! Acceptance tests S1: `strategy_version`, `trial_registry` and `decision` against a real
//! Postgres (`TEST_DATABASE_URL`).
//!
//! The decision tests call `features_to_json` / `features_from_json`, which come from task C1.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use nisse_core::{
    Action, Decision, DecisionId, Features, Hash32, StrategyVersion, StrategyVersionId, Trial,
    TrialKind,
    nautilus::{Symbol, UnixNanos},
};
use nisse_store::{
    StoreError,
    tables::{decision, strategy_version, trial_registry},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds.
const T0: u64 = 1_767_600_000_123_456_000;
const ONE_SECOND: u64 = 1_000_000_000;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn hash(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

/// A version at git SHA `git` (a single hex digit repeated) with config hash byte `byte`.
fn version(git: char, byte: u8, lookback: i64) -> StrategyVersion {
    StrategyVersion {
        git_sha: git.to_string().repeat(40),
        config_hash: hash(byte),
        params: json!({"strategy": "threshold", "lookback": lookback}),
        parent_id: None,
        ts: UnixNanos::from(T0),
    }
}

fn is_db_error(err: &StoreError) -> bool {
    matches!(err, StoreError::Db(_))
}

fn features() -> Features {
    BTreeMap::from([
        ("mom_12m".to_string(), dec("0.123456789")),
        ("spread".to_string(), dec("-0.0025")),
        ("vol_20d".to_string(), dec("1.50")),
        ("zero".to_string(), dec("0")),
    ])
}

fn decision_for(strategy_version_id: StrategyVersionId, symbol: &str) -> Decision {
    Decision {
        strategy_version_id,
        symbol: Symbol::new(symbol),
        action: Action::Enter,
        intended_qty: dec("-2.5"),
        expected_edge_bps: Some(12.75),
        expected_vol: Some(0.2137),
        features: features(),
        feature_hash: hash(0xab),
        data_snapshot_id: "snap-2026-01-05".to_string(),
        rng_seed: 42,
        reason_code: "threshold_crossed".to_string(),
        ts: UnixNanos::from(T0),
    }
}

fn trial(strategy_version_id: StrategyVersionId, kind: TrialKind, result: Option<Value>) -> Trial {
    Trial {
        strategy_version_id,
        kind,
        data_start: UnixNanos::from(T0),
        data_end: UnixNanos::from(T0 + 365 * 86_400 * ONE_SECOND),
        result,
        ts: UnixNanos::from(T0 + ONE_SECOND),
    }
}

// strategy_version

#[tokio::test]
async fn ensure_twice_returns_same_id_and_keeps_first_row() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let first = version('a', 1, 252);
    let id = strategy_version::ensure(&client, &first).await.unwrap();

    let mut second = first.clone();
    second.params = json!({"strategy": "threshold", "lookback": 20});
    second.ts = UnixNanos::from(T0 + ONE_SECOND);
    let again = strategy_version::ensure(&client, &second).await.unwrap();

    assert_eq!(again, id);
    let stored = strategy_version::get(&client, id).await.unwrap();
    assert_eq!(stored, Some(first));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn different_config_hash_gives_different_id() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let a = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let b = strategy_version::ensure(&client, &version('a', 2, 252))
        .await
        .unwrap();
    let other_sha = strategy_version::ensure(&client, &version('b', 1, 252))
        .await
        .unwrap();

    assert_ne!(a, b);
    assert_ne!(a, other_sha);
    assert_ne!(b, other_sha);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn parent_id_round_trips() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let parent_version = version('a', 1, 252);
    let parent = strategy_version::ensure(&client, &parent_version)
        .await
        .unwrap();

    let child_version = StrategyVersion {
        parent_id: Some(parent),
        ..version('b', 2, 126)
    };
    let child = strategy_version::ensure(&client, &child_version)
        .await
        .unwrap();

    let stored = strategy_version::get(&client, child).await.unwrap();
    assert_eq!(stored, Some(child_version));
    let stored_parent = strategy_version::get(&client, parent).await.unwrap();
    assert_eq!(stored_parent, Some(parent_version));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn get_of_missing_id_is_none() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let missing = strategy_version::get(&client, StrategyVersionId(987_654))
        .await
        .unwrap();
    assert_eq!(missing, None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn bad_git_sha_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let mut bad = version('a', 1, 252);
    bad.git_sha = "not-a-sha".to_string();
    let err = strategy_version::ensure(&client, &bad).await.unwrap_err();
    assert!(is_db_error(&err), "expected Db error, got {err:?}");

    db.drop_db().await.unwrap();
}

// trial_registry

#[tokio::test]
async fn lifetime_count_counts_every_kind_and_version() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    assert_eq!(trial_registry::lifetime_count(&client).await.unwrap(), 0);

    let sv_a = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let sv_b = strategy_version::ensure(&client, &version('b', 2, 252))
        .await
        .unwrap();
    trial_registry::insert(&client, &trial(sv_a, TrialKind::Backtest, None))
        .await
        .unwrap();
    trial_registry::insert(
        &client,
        &trial(sv_a, TrialKind::Shadow, Some(json!({"sharpe": 1.2}))),
    )
    .await
    .unwrap();
    trial_registry::insert(&client, &trial(sv_b, TrialKind::Live, None))
        .await
        .unwrap();

    assert_eq!(trial_registry::lifetime_count(&client).await.unwrap(), 3);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn trial_range_is_half_open_and_result_round_trips() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let with_result = trial(sv, TrialKind::Backtest, Some(json!({"sharpe": 1.25})));
    let id = trial_registry::insert(&client, &with_result).await.unwrap();

    let row = client
        .query_one(
            "SELECT lower(data_range), upper(data_range), lower_inc(data_range), \
                 upper_inc(data_range), result, kind, strategy_version_id \
             FROM trial_registry WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let lower: DateTime<Utc> = row.get(0);
    let upper: DateTime<Utc> = row.get(1);
    assert_eq!(
        lower,
        DateTime::from_timestamp_micros(1_767_600_000_123_456).unwrap()
    );
    assert_eq!(
        upper,
        DateTime::from_timestamp_micros(1_767_600_000_123_456 + 365 * 86_400 * 1_000_000).unwrap()
    );
    assert!(row.get::<_, bool>(2), "lower bound is inclusive");
    assert!(!row.get::<_, bool>(3), "upper bound is exclusive");
    assert_eq!(
        row.get::<_, Option<Value>>(4),
        Some(json!({"sharpe": 1.25}))
    );
    assert_eq!(row.get::<_, String>(5), "backtest");
    assert_eq!(row.get::<_, i64>(6), sv.0);

    let none_id = trial_registry::insert(&client, &trial(sv, TrialKind::Live, None))
        .await
        .unwrap();
    let result: Option<Value> = client
        .query_one(
            "SELECT result FROM trial_registry WHERE id = $1",
            &[&none_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(result, None);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn empty_data_range_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let mut empty = trial(sv, TrialKind::Backtest, None);
    empty.data_end = empty.data_start;
    let err = trial_registry::insert(&client, &empty).await.unwrap_err();
    assert!(is_db_error(&err), "expected Db error, got {err:?}");
    assert_eq!(trial_registry::lifetime_count(&client).await.unwrap(), 0);

    db.drop_db().await.unwrap();
}

// decision

#[tokio::test]
async fn decision_round_trips_every_field() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let v = version('a', 1, 252);
    let sv = strategy_version::ensure(&client, &v).await.unwrap();
    let mut original = decision_for(sv, "SPY");
    original.rng_seed = u64::MAX - 1;
    let id = decision::insert(&client, &original).await.unwrap();

    let rows = decision::list_after(&client, DecisionId(0), 10)
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![decision::StoredDecision {
            id,
            decision: original,
            version: v,
        }]
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn hold_with_no_statistics_and_empty_features_round_trips() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let v = version('a', 1, 252);
    let sv = strategy_version::ensure(&client, &v).await.unwrap();
    let hold = Decision {
        action: Action::Hold,
        intended_qty: Decimal::ZERO,
        expected_edge_bps: None,
        expected_vol: None,
        features: Features::new(),
        rng_seed: 0,
        ..decision_for(sv, "QQQ")
    };
    let id = decision::insert(&client, &hold).await.unwrap();

    let rows = decision::list_after(&client, DecisionId(0), 10)
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![decision::StoredDecision {
            id,
            decision: hold,
            version: v,
        }]
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn seed_above_i64_max_is_stored_as_the_same_bits() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let seeds = [u64::MAX, u64::try_from(i64::MAX).unwrap() + 1, 0];
    let mut ids = Vec::new();
    for seed in seeds {
        let d = Decision {
            rng_seed: seed,
            ..decision_for(sv, "SPY")
        };
        ids.push(decision::insert(&client, &d).await.unwrap());
    }

    let stored: i64 = client
        .query_one("SELECT rng_seed FROM decision WHERE id = $1", &[&ids[0].0])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        stored, -1,
        "u64::MAX is stored as the i64 with the same bits"
    );

    let rows = decision::list_after(&client, DecisionId(0), 10)
        .await
        .unwrap();
    let read: Vec<u64> = rows.iter().map(|r| r.decision.rng_seed).collect();
    assert_eq!(read, seeds.to_vec());

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn timestamps_are_stored_to_the_microsecond() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let written = Decision {
        ts: UnixNanos::from(1_767_600_000_123_456_789),
        ..decision_for(sv, "SPY")
    };
    decision::insert(&client, &written).await.unwrap();

    let rows = decision::list_after(&client, DecisionId(0), 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision.ts, UnixNanos::from(T0));

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn paging_returns_every_row_once_in_id_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(
            decision::insert(&client, &decision_for(sv, "SPY"))
                .await
                .unwrap(),
        );
    }

    let page1 = decision::list_after(&client, DecisionId(0), 2)
        .await
        .unwrap();
    let page2 = decision::list_after(&client, page1[1].id, 2).await.unwrap();
    let page3 = decision::list_after(&client, page2[1].id, 2).await.unwrap();
    let page4 = decision::list_after(&client, page3[0].id, 2).await.unwrap();
    assert!(page4.is_empty());

    let paged: Vec<DecisionId> = [page1, page2, page3]
        .concat()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(paged, ids);

    let all = decision::list_after(&client, DecisionId(0), 1_000)
        .await
        .unwrap();
    assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), ids);

    let after_third = decision::list_after(&client, ids[2], 1_000).await.unwrap();
    assert_eq!(
        after_third.iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[3..].to_vec()
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn joined_version_is_the_ensured_version_of_each_decision() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let va = version('a', 1, 252);
    let vb = version('b', 2, 126);
    let sv_a = strategy_version::ensure(&client, &va).await.unwrap();
    let sv_b = strategy_version::ensure(&client, &vb).await.unwrap();
    let id_a = decision::insert(&client, &decision_for(sv_a, "SPY"))
        .await
        .unwrap();
    let id_b = decision::insert(&client, &decision_for(sv_b, "QQQ"))
        .await
        .unwrap();

    let rows = decision::list_after(&client, DecisionId(0), 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, id_a);
    assert_eq!(rows[0].version, va);
    assert_eq!(rows[0].decision.strategy_version_id, sv_a);
    assert_eq!(rows[1].id, id_b);
    assert_eq!(rows[1].version, vb);
    assert_eq!(rows[1].decision.strategy_version_id, sv_b);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn unknown_strategy_version_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let orphan = decision_for(StrategyVersionId(9_999_999), "SPY");
    let err = decision::insert(&client, &orphan).await.unwrap_err();
    assert!(is_db_error(&err), "expected Db error, got {err:?}");

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn empty_reason_code_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let sv = strategy_version::ensure(&client, &version('a', 1, 252))
        .await
        .unwrap();
    let blank = Decision {
        reason_code: String::new(),
        ..decision_for(sv, "SPY")
    };
    let err = decision::insert(&client, &blank).await.unwrap_err();
    assert!(is_db_error(&err), "expected Db error, got {err:?}");
    assert!(
        decision::list_after(&client, DecisionId(0), 10)
            .await
            .unwrap()
            .is_empty()
    );

    db.drop_db().await.unwrap();
}
