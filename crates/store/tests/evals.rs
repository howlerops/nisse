//! Acceptance tests for `decision_holdout`, `outcome_window` and the `eval_score` read against
//! a real Postgres (`TEST_DATABASE_URL`; run `eval "$(scripts/dev-postgres.sh)"` first).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use nisse_core::{
    Action, Decision, DecisionId, EvalScore, Hash32, StrategyVersion, StrategyVersionId,
    SubjectType,
    nautilus::{Symbol, UnixNanos},
};
use nisse_store::{
    StoreError,
    tables::{
        decision,
        decision_holdout::{self, Assignment},
        eval_score,
        outcome_window::{self, OutcomeRow, PendingDecision},
        strategy_version,
    },
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::json;
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds.
const T0: u64 = 1_767_600_000_123_456_000;
const SECOND: u64 = 1_000_000_000;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

/// The `TIMESTAMPTZ` value a whole-microsecond engine time is stored as.
fn micros(nanos: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(i64::try_from(nanos / 1_000).unwrap()).unwrap()
}

fn is_db_error(err: &StoreError) -> bool {
    matches!(err, StoreError::Db(_))
}

async fn count(client: &Client, table: &str) -> i64 {
    client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0)
}

/// Creates one strategy version and returns its id.
async fn seed_version(client: &Client) -> StrategyVersionId {
    let params = json!({"strategy": "threshold", "lookback": 252});
    let version = StrategyVersion {
        git_sha: "a".repeat(40),
        config_hash: nisse_core::config_hash(&params),
        params,
        parent_id: None,
        ts: UnixNanos::from(T0),
    };
    strategy_version::ensure(client, &version).await.unwrap()
}

/// One decision per `(symbol, expected_vol)` spec, a second apart, through `decision::insert`.
async fn seed_decisions(
    client: &Client,
    version: StrategyVersionId,
    specs: &[(&str, Option<f64>)],
) -> Vec<DecisionId> {
    let mut ids = Vec::with_capacity(specs.len());
    for (i, (symbol, expected_vol)) in specs.iter().enumerate() {
        let decision = Decision {
            strategy_version_id: version,
            symbol: Symbol::new(symbol),
            action: Action::Enter,
            intended_qty: dec("1.5"),
            expected_edge_bps: None,
            expected_vol: *expected_vol,
            features: BTreeMap::new(),
            feature_hash: Hash32::from_bytes([0xab; 32]),
            data_snapshot_id: "snap-1".to_string(),
            rng_seed: 42,
            reason_code: "trend_up".to_string(),
            ts: UnixNanos::from(T0 + i as u64 * SECOND),
        };
        ids.push(decision::insert(client, &decision).await.unwrap());
    }
    ids
}

fn assignment(decision_id: DecisionId, hidden: bool) -> Assignment {
    Assignment {
        decision_id,
        hidden,
    }
}

fn outcome(decision_id: DecisionId, horizon_sessions: u32, fwd_ret: f64) -> OutcomeRow {
    OutcomeRow {
        decision_id,
        horizon_sessions,
        fwd_ret,
        bench_ret: 0.012_345_678_9,
        mfe: 0.031_4,
        mae: -0.027_1,
        ts: UnixNanos::from(T0),
    }
}

// decision_holdout

#[tokio::test]
async fn holdout_unassigned_pages_in_id_order_and_counts_add_up() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None), ("QQQ", None), ("IWM", None)]).await;

    assert_eq!(
        decision_holdout::unassigned(&client, 10).await.unwrap(),
        vec![d[0], d[1], d[2]]
    );
    assert_eq!(
        decision_holdout::unassigned(&client, 2).await.unwrap(),
        vec![d[0], d[1]]
    );

    decision_holdout::insert_many(
        &client,
        &[assignment(d[0], true), assignment(d[1], false)],
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(
        decision_holdout::unassigned(&client, 10).await.unwrap(),
        vec![d[2]]
    );
    assert_eq!(decision_holdout::counts(&client).await.unwrap(), (1, 1));

    // Decision 3 rides along with a repeat of decision 1: the statement fails as a whole.
    let second = decision_holdout::insert_many(
        &client,
        &[assignment(d[2], true), assignment(d[0], false)],
        UnixNanos::from(T0 + SECOND),
    )
    .await;
    assert!(second.as_ref().is_err_and(is_db_error), "got {second:?}");
    assert_eq!(decision_holdout::counts(&client).await.unwrap(), (1, 1));
    assert_eq!(
        decision_holdout::unassigned(&client, 10).await.unwrap(),
        vec![d[2]]
    );

    // Only the visible decision appears in the agent view.
    let visible: Vec<i64> = client
        .query("SELECT id FROM agent_decision ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(visible, vec![d[1].0]);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn holdout_insert_stamps_every_row_with_the_given_ts() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None), ("QQQ", None)]).await;

    decision_holdout::insert_many(
        &client,
        &[assignment(d[0], true), assignment(d[1], false)],
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    let stamped: Vec<DateTime<Utc>> = client
        .query("SELECT event_ts FROM decision_holdout ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(stamped, vec![micros(T0), micros(T0)]);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn holdout_empty_slice_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    seed_decisions(&client, sv, &[("SPY", None)]).await;

    decision_holdout::insert_many(&client, &[], UnixNanos::from(T0))
        .await
        .unwrap();
    assert_eq!(count(&client, "decision_holdout").await, 0);
    assert_eq!(decision_holdout::counts(&client).await.unwrap(), (0, 0));

    db.drop_db().await.unwrap();
}

// outcome_window

#[tokio::test]
async fn outcome_pending_returns_symbol_ts_and_expected_vol() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(
        &client,
        sv,
        &[("SPY", Some(0.02)), ("QQQ", None), ("IWM", Some(0.2137))],
    )
    .await;

    let expected = vec![
        PendingDecision {
            id: d[0],
            symbol: Symbol::new("SPY"),
            ts: UnixNanos::from(T0),
            expected_vol: Some(0.02),
        },
        PendingDecision {
            id: d[1],
            symbol: Symbol::new("QQQ"),
            ts: UnixNanos::from(T0 + SECOND),
            expected_vol: None,
        },
        PendingDecision {
            id: d[2],
            symbol: Symbol::new("IWM"),
            ts: UnixNanos::from(T0 + 2 * SECOND),
            expected_vol: Some(0.2137),
        },
    ];
    assert_eq!(
        outcome_window::pending(&client, 5, 10).await.unwrap(),
        expected
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_row_clears_only_its_own_horizon() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", Some(0.02)), ("QQQ", None)]).await;

    outcome_window::insert_many(&client, &[outcome(d[0], 5, 0.02)])
        .await
        .unwrap();

    let at_five: Vec<DecisionId> = outcome_window::pending(&client, 5, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(at_five, vec![d[1]]);

    let at_21: Vec<DecisionId> = outcome_window::pending(&client, 21, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(at_21, vec![d[0], d[1]]);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_rows_read_back_field_for_field_in_horizon_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", Some(0.02))]).await;

    let five = OutcomeRow {
        decision_id: d[0],
        horizon_sessions: 5,
        fwd_ret: 0.02,
        bench_ret: -0.0001,
        mfe: 0.1,
        mae: -0.0,
        ts: UnixNanos::from(T0),
    };
    let twenty_one = outcome(d[0], 21, -0.0001);
    let one = outcome(d[0], 1, 0.3);

    // Inserted 21, 1, 5: reads come back by horizon, not by insertion.
    outcome_window::insert_many(&client, &[twenty_one, one, five])
        .await
        .unwrap();

    let stored = outcome_window::for_decision(&client, d[0]).await.unwrap();
    assert_eq!(stored, vec![one, five, twenty_one]);
    assert_eq!(stored[1].horizon_sessions, 5);
    assert_eq!(stored[1].fwd_ret, 0.02);
    assert_eq!(stored[1].bench_ret, -0.0001);

    // The row for a decision with no outcomes is empty, not an error.
    let other = seed_decisions(&client, sv, &[("QQQ", None)]).await;
    assert!(
        outcome_window::for_decision(&client, other[0])
            .await
            .unwrap()
            .is_empty()
    );

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_nan_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None)]).await;

    let mut bad = outcome(d[0], 5, 0.0);
    bad.fwd_ret = f64::NAN;
    let rows = vec![outcome(d[0], 1, 0.1), bad, outcome(d[0], 21, 0.2)];
    let err = outcome_window::insert_many(&client, &rows)
        .await
        .unwrap_err();
    assert!(is_db_error(&err), "got {err:?}");
    assert_eq!(count(&client, "outcome_window").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_infinity_is_refused_too() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None)]).await;

    let mut bad = outcome(d[0], 5, 0.0);
    bad.mae = f64::NEG_INFINITY;
    let err = outcome_window::insert_many(&client, &[bad])
        .await
        .unwrap_err();
    assert!(is_db_error(&err), "got {err:?}");
    assert_eq!(count(&client, "outcome_window").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_empty_slice_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    seed_decisions(&client, sv, &[("SPY", None)]).await;

    outcome_window::insert_many(&client, &[]).await.unwrap();
    assert_eq!(count(&client, "outcome_window").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_zero_horizon_is_refused_by_the_check() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None)]).await;

    let err = outcome_window::insert_many(&client, &[outcome(d[0], 0, 0.0)])
        .await
        .unwrap_err();
    assert!(is_db_error(&err), "got {err:?}");
    assert_eq!(count(&client, "outcome_window").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_horizon_beyond_i32_is_a_data_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("SPY", None)]).await;

    let err = outcome_window::insert_many(&client, &[outcome(d[0], u32::MAX, 0.0)])
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Data(_)), "got {err:?}");
    assert_eq!(count(&client, "outcome_window").await, 0);

    let err = outcome_window::pending(&client, u32::MAX, 10)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Data(_)), "got {err:?}");

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn outcome_pending_respects_limit_and_id_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(
        &client,
        sv,
        &[
            ("A", None),
            ("B", None),
            ("C", None),
            ("D", None),
            ("E", None),
        ],
    )
    .await;

    let ids = |rows: Vec<PendingDecision>| rows.into_iter().map(|p| p.id).collect::<Vec<_>>();
    assert_eq!(
        ids(outcome_window::pending(&client, 5, 10).await.unwrap()),
        d
    );
    assert_eq!(
        ids(outcome_window::pending(&client, 5, 3).await.unwrap()),
        vec![d[0], d[1], d[2]]
    );

    // Once decision 2 has its horizon-5 row, the page starts at decision 1 and skips it.
    outcome_window::insert_many(&client, &[outcome(d[1], 5, 0.01)])
        .await
        .unwrap();
    assert_eq!(
        ids(outcome_window::pending(&client, 5, 3).await.unwrap()),
        vec![d[0], d[2], d[3]]
    );

    db.drop_db().await.unwrap();
}

// eval_score::scored_subjects

#[tokio::test]
async fn scored_subjects_lists_distinct_ids_for_one_metric_and_version() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let sv = seed_version(&client).await;
    let d = seed_decisions(&client, sv, &[("A", None), ("B", None), ("C", None)]).await;

    let score = |subject: DecisionId, metric: &str, eval_version: &str| EvalScore {
        subject_id: subject.0,
        subject_type: SubjectType::Decision,
        metric: metric.to_owned(),
        value: 0.5,
        eval_version: eval_version.to_owned(),
        ts: UnixNanos::from(T0),
    };
    eval_score::insert_many(
        &client,
        &[
            score(d[0], "decision_quality_5", "v1"),
            score(d[2], "decision_quality_5", "v1"),
            score(d[1], "other_metric", "v1"),
            // A recomputation of decision 1 is a second row, not a second subject.
            score(d[0], "decision_quality_5", "v1"),
        ],
    )
    .await
    .unwrap();

    let found =
        eval_score::scored_subjects(&client, SubjectType::Decision, "decision_quality_5", "v1")
            .await
            .unwrap();
    assert_eq!(found, vec![d[0].0, d[2].0]);

    let other_version =
        eval_score::scored_subjects(&client, SubjectType::Decision, "decision_quality_5", "v2")
            .await
            .unwrap();
    assert!(other_version.is_empty());

    let other_type = eval_score::scored_subjects(
        &client,
        SubjectType::StrategyVersion,
        "decision_quality_5",
        "v1",
    )
    .await
    .unwrap();
    assert!(other_type.is_empty());

    db.drop_db().await.unwrap();
}
