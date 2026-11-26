//! Batched inserts for `risk_check` and `eval_score` against a real Postgres.
//!
//! Needs `TEST_DATABASE_URL` (run `eval "$(scripts/dev-postgres.sh)"` first).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use chrono::{DateTime, Utc};
use nisse_core::{DecisionId, EvalScore, RiskCheck, SubjectType, nautilus::UnixNanos};
use nisse_store::{
    StoreError,
    tables::{eval_score, risk_check},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use tokio_postgres::Client;

/// A whole-microsecond instant in engine nanoseconds.
const T0: u64 = 1_767_600_000_123_456_000;
const SECOND: u64 = 1_000_000_000;

type RiskRow = (i64, String, Decimal, Decimal, bool, DateTime<Utc>);
type ScoreRow = (i64, String, String, f64, String, DateTime<Utc>);

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

/// The `TIMESTAMPTZ` value a whole-microsecond engine time is stored as.
fn micros(nanos: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(i64::try_from(nanos / 1_000).unwrap()).unwrap()
}

/// Inserts one strategy version and one decision with plain SQL; returns their ids.
async fn seed(client: &Client) -> (i64, i64) {
    let strategy_version: i64 = client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', \
                     '2026-01-02T00:00:00Z') \
             RETURNING id",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let decision: i64 = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                                   feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
             VALUES ($1, 'SPY', 'enter', 1.5, '{}', decode(repeat('ab', 32), 'hex'), 'snap-1', \
                     42, 'trend_up', '2026-01-05T21:00:00Z') \
             RETURNING id",
            &[&strategy_version],
        )
        .await
        .unwrap()
        .get(0);
    (strategy_version, decision)
}

async fn read_risk(client: &Client) -> Vec<RiskRow> {
    client
        .query(
            "SELECT decision_id, rule, limit_value::numeric, actual_value::numeric, passed, \
                    event_ts \
             FROM risk_check ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get(2),
                row.get(3),
                row.get(4),
                row.get(5),
            )
        })
        .collect()
}

async fn read_scores(client: &Client) -> Vec<ScoreRow> {
    client
        .query(
            "SELECT subject_id, subject_type, metric, value::float8, eval_version, event_ts \
             FROM eval_score ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get(2),
                row.get(3),
                row.get(4),
                row.get(5),
            )
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

fn risk(
    decision: i64,
    rule: &str,
    limit: &str,
    actual: &str,
    passed: bool,
    nanos: u64,
) -> RiskCheck {
    RiskCheck {
        decision_id: DecisionId(decision),
        rule: rule.to_owned(),
        limit_value: dec(limit),
        actual_value: dec(actual),
        passed,
        ts: UnixNanos::from(nanos),
    }
}

fn score(
    subject_id: i64,
    subject_type: SubjectType,
    metric: &str,
    value: f64,
    nanos: u64,
) -> EvalScore {
    EvalScore {
        subject_id,
        subject_type,
        metric: metric.to_owned(),
        value,
        eval_version: "eval-1".to_owned(),
        ts: UnixNanos::from(nanos),
    }
}

// risk_check

#[tokio::test]
async fn risk_check_three_rows_read_back_equal() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
        risk(
            decision,
            "min_adv",
            "1000000",
            "999999.5",
            false,
            T0 + SECOND,
        ),
        risk(decision, "max_loss", "0", "-12.75", true, T0 + 2 * SECOND),
    ];
    risk_check::insert_many(&client, &rows).await.unwrap();

    let expected: Vec<RiskRow> = vec![
        (
            decision,
            "max_weight".into(),
            dec("0.25"),
            dec("0.005"),
            true,
            micros(T0),
        ),
        (
            decision,
            "min_adv".into(),
            dec("1000000"),
            dec("999999.5"),
            false,
            micros(T0 + SECOND),
        ),
        (
            decision,
            "max_loss".into(),
            dec("0"),
            dec("-12.75"),
            true,
            micros(T0 + 2 * SECOND),
        ),
    ];
    let actual = read_risk(&client).await;
    assert_eq!(actual, expected);
    // Exact decimal text, not just numeric equality.
    assert_eq!(actual[0].2.to_string(), "0.25");
    assert_eq!(actual[0].3.to_string(), "0.005");

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_rows_read_back_in_slice_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    // Timestamps run backwards, so only the insertion order can produce this id order.
    let rows: Vec<RiskCheck> = (0..5)
        .map(|i| {
            risk(
                decision,
                &format!("rule_{i}"),
                "1",
                "0",
                true,
                T0 - i * SECOND,
            )
        })
        .collect();
    risk_check::insert_many(&client, &rows).await.unwrap();

    let rules: Vec<String> = read_risk(&client)
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    let expected: Vec<String> = (0..5).map(|i| format!("rule_{i}")).collect();
    assert_eq!(rules, expected);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_empty_slice_is_ok_and_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    seed(&client).await;

    risk_check::insert_many(&client, &[]).await.unwrap();
    assert_eq!(count(&client, "risk_check").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_unknown_decision_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
        risk(999_999, "max_weight", "0.25", "0.005", true, T0),
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
    ];
    let err = risk_check::insert_many(&client, &rows).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "risk_check").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_empty_rule_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
        risk(decision, "", "0.25", "0.005", true, T0),
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
    ];
    let err = risk_check::insert_many(&client, &rows).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "risk_check").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_ingest_ts_is_set_by_database() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        risk(decision, "max_weight", "0.25", "0.005", true, T0),
        risk(decision, "min_adv", "1", "2", false, T0),
        risk(decision, "max_loss", "0", "-1", true, T0),
    ];
    risk_check::insert_many(&client, &rows).await.unwrap();

    let not_null: i64 = client
        .query_one(
            "SELECT count(*) FROM risk_check WHERE ingest_ts IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(not_null, 3);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn risk_check_thousand_rows_in_one_call() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows: Vec<RiskCheck> = (0..1_000)
        .map(|i| {
            risk(
                decision,
                &format!("rule_{i}"),
                "10",
                "5",
                i % 2 == 0,
                T0 + i * SECOND,
            )
        })
        .collect();
    risk_check::insert_many(&client, &rows).await.unwrap();

    assert_eq!(count(&client, "risk_check").await, 1_000);

    db.drop_db().await.unwrap();
}

// eval_score

#[tokio::test]
async fn eval_score_three_rows_read_back_equal() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (strategy_version, decision) = seed(&client).await;

    let rows = vec![
        score(decision, SubjectType::Decision, "sharpe", 1.25, T0),
        score(
            strategy_version,
            SubjectType::StrategyVersion,
            "pbo",
            0.5,
            T0 + SECOND,
        ),
        score(
            decision,
            SubjectType::Decision,
            "ic",
            -0.0375,
            T0 + 2 * SECOND,
        ),
    ];
    eval_score::insert_many(&client, &rows).await.unwrap();

    let expected: Vec<ScoreRow> = vec![
        (
            decision,
            "decision".into(),
            "sharpe".into(),
            1.25,
            "eval-1".into(),
            micros(T0),
        ),
        (
            strategy_version,
            "strategy_version".into(),
            "pbo".into(),
            0.5,
            "eval-1".into(),
            micros(T0 + SECOND),
        ),
        (
            decision,
            "decision".into(),
            "ic".into(),
            -0.0375,
            "eval-1".into(),
            micros(T0 + 2 * SECOND),
        ),
    ];
    assert_eq!(read_scores(&client).await, expected);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_rows_read_back_in_slice_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows: Vec<EvalScore> = (0..5)
        .map(|i| {
            score(
                decision,
                SubjectType::Decision,
                &format!("metric_{i}"),
                0.0,
                T0 - i * SECOND,
            )
        })
        .collect();
    eval_score::insert_many(&client, &rows).await.unwrap();

    let metrics: Vec<String> = read_scores(&client)
        .await
        .into_iter()
        .map(|row| row.2)
        .collect();
    let expected: Vec<String> = (0..5).map(|i| format!("metric_{i}")).collect();
    assert_eq!(metrics, expected);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_empty_slice_is_ok_and_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    seed(&client).await;

    eval_score::insert_many(&client, &[]).await.unwrap();
    assert_eq!(count(&client, "eval_score").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_nan_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
        score(decision, SubjectType::Decision, "sharpe", f64::NAN, T0),
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
    ];
    let err = eval_score::insert_many(&client, &rows).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "eval_score").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_infinity_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
        score(decision, SubjectType::Decision, "sharpe", f64::INFINITY, T0),
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
    ];
    let err = eval_score::insert_many(&client, &rows).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "eval_score").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_empty_metric_in_second_row_inserts_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
        score(decision, SubjectType::Decision, "", 1.0, T0),
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
    ];
    let err = eval_score::insert_many(&client, &rows).await.unwrap_err();
    assert!(matches!(err, StoreError::Db(_)), "got {err:?}");
    assert_eq!(count(&client, "eval_score").await, 0);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_ingest_ts_is_set_by_database() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows = vec![
        score(decision, SubjectType::Decision, "sharpe", 1.0, T0),
        score(decision, SubjectType::Decision, "ic", 0.1, T0),
        score(decision, SubjectType::Decision, "pbo", 0.2, T0),
    ];
    eval_score::insert_many(&client, &rows).await.unwrap();

    let not_null: i64 = client
        .query_one(
            "SELECT count(*) FROM eval_score WHERE ingest_ts IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(not_null, 3);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_thousand_rows_in_one_call() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let rows: Vec<EvalScore> = (0..1_000)
        .map(|i| {
            score(
                decision,
                SubjectType::Decision,
                &format!("metric_{i}"),
                i as f64,
                T0 + i * SECOND,
            )
        })
        .collect();
    eval_score::insert_many(&client, &rows).await.unwrap();

    assert_eq!(count(&client, "eval_score").await, 1_000);

    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn eval_score_recomputation_is_a_new_row() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_, decision) = seed(&client).await;

    let first = vec![score(decision, SubjectType::Decision, "sharpe", 1.0, T0)];
    let second = vec![score(
        decision,
        SubjectType::Decision,
        "sharpe",
        0.5,
        T0 + SECOND,
    )];
    eval_score::insert_many(&client, &first).await.unwrap();
    eval_score::insert_many(&client, &second).await.unwrap();

    let rows = read_scores(&client).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].3, 1.0);
    assert_eq!(rows[0].5, micros(T0));
    assert_eq!(rows[1].3, 0.5);
    assert_eq!(rows[1].5, micros(T0 + SECOND));

    db.drop_db().await.unwrap();
}
