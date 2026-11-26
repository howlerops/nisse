//! Acceptance tests V1: the nightly replay job against a real Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
};

use nisse_core::{
    Action, DecisionId, Features, Hash32, StrategyVersion, StrategyVersionId, config_hash,
    nautilus::{Symbol, UnixNanos},
};
use nisse_engine::{DecisionContext, ReplayOutcome, Strategy, Verdict, decide, decide_and_record};
use nisse_eval::{
    EvalError,
    replay::{ReplayFailure, ReplayReport, replay_decisions},
};
use nisse_store::{
    Store,
    tables::{decision, strategy_version},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z.
const T0: u64 = 1_767_600_000_123_456_000;

/// Enters with 10 when the `"signal"` feature is above `level`, holds otherwise.
struct Threshold {
    level: Decimal,
}

impl Threshold {
    fn new(level: &str) -> Self {
        Self {
            level: Decimal::from_str(level).unwrap(),
        }
    }
}

impl Strategy for Threshold {
    fn name(&self) -> &'static str {
        "threshold"
    }

    fn decide(&self, features: &Features, _seed: u64) -> Verdict {
        if features.get("signal").is_some_and(|v| *v > self.level) {
            Verdict {
                action: Action::Enter,
                intended_qty: Decimal::new(10, 0),
                expected_edge_bps: Some(1.5),
                expected_vol: None,
                reason_code: "above",
            }
        } else {
            Verdict {
                action: Action::Hold,
                intended_qty: Decimal::ZERO,
                expected_edge_bps: None,
                expected_vol: None,
                reason_code: "below",
            }
        }
    }
}

/// A factory that builds `Threshold` for `"strategy": "threshold"` and rejects everything
/// else. Every call, successful or not, adds one to `calls`.
fn threshold_factory(
    calls: &AtomicUsize,
) -> impl Fn(&Value) -> Result<Box<dyn Strategy>, EvalError> + Send + Sync + '_ {
    move |params: &Value| -> Result<Box<dyn Strategy>, EvalError> {
        calls.fetch_add(1, Ordering::SeqCst);
        let name = params
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        if name != "threshold" {
            return Err(EvalError::UnknownStrategy(name.to_owned()));
        }
        let level = params
            .get("level")
            .and_then(Value::as_str)
            .and_then(|s| Decimal::from_str(s).ok())
            .ok_or_else(|| EvalError::UnknownStrategy("bad level".to_owned()))?;
        Ok(Box::new(Threshold { level }))
    }
}

fn sha(digit: char) -> String {
    digit.to_string().repeat(40)
}

fn threshold_params(level: &str) -> Value {
    json!({"strategy": "threshold", "level": level})
}

/// Ensures a version with the correct `config_hash` for `params`.
async fn ensure(client: &Client, git_sha: &str, params: Value) -> StrategyVersionId {
    let version = StrategyVersion {
        git_sha: git_sha.to_owned(),
        config_hash: config_hash(&params),
        params,
        parent_id: None,
        ts: UnixNanos::from(T0),
    };
    strategy_version::ensure(client, &version).await.unwrap()
}

/// Ensures a version whose stored `config_hash` is `hash`, whatever `params` say.
async fn ensure_with_hash(
    client: &Client,
    git_sha: &str,
    params: Value,
    hash: Hash32,
) -> StrategyVersionId {
    let version = StrategyVersion {
        git_sha: git_sha.to_owned(),
        config_hash: hash,
        params,
        parent_id: None,
        ts: UnixNanos::from(T0),
    };
    strategy_version::ensure(client, &version).await.unwrap()
}

/// A decision context for `version`; `signal` is written as the `"signal"` feature.
fn context(version: StrategyVersionId, signal: Option<&str>, seed: u64) -> DecisionContext {
    let mut features = Features::new();
    features.insert("pos".to_owned(), Decimal::ZERO);
    if let Some(s) = signal {
        features.insert("signal".to_owned(), Decimal::from_str(s).unwrap());
    }
    DecisionContext {
        strategy_version_id: version,
        symbol: Symbol::new("SPY"),
        features,
        data_snapshot_id: "snap-1".to_owned(),
        rng_seed: seed,
        ts: UnixNanos::from(T0),
    }
}

/// Makes a decision with `Threshold(level)` through the real path and records it.
async fn record(
    store: &Store,
    level: &str,
    version: StrategyVersionId,
    signal: &str,
) -> DecisionId {
    let (id, _) = decide_and_record(
        &Threshold::new(level),
        store,
        context(version, Some(signal), 0),
    )
    .await
    .unwrap();
    id
}

/// `(subject_id, subject_type, metric, value, eval_version)` for every score row, in write order.
async fn score_rows(client: &Client) -> Vec<(i64, String, String, f64, String)> {
    client
        .query(
            "SELECT subject_id, subject_type, metric, value::float8, eval_version \
             FROM eval_score ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)))
        .collect()
}

async fn score_count(client: &Client) -> i64 {
    client
        .query_one("SELECT count(*) FROM eval_score", &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn recorded_decisions_all_replay_as_matches() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;

    let mut ids = Vec::new();
    for signal in ["2", "0.5", "3", "0.1", "1.5"] {
        ids.push(record(&store, "1.0", version, signal).await);
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 5);
    assert!(report.failures.is_empty());
    assert_eq!(report.other_builds, 0);
    assert_eq!(report.unbuildable, 0);
    assert!(report.is_clean());

    let rows = score_rows(&client).await;
    assert_eq!(rows.len(), 5);
    let subjects: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let expected: Vec<i64> = ids.iter().map(|id| id.0).collect();
    assert_eq!(subjects, expected);
    for (_, subject_type, metric, value, eval_version) in &rows {
        assert_eq!(subject_type, "decision");
        assert_eq!(metric, "replay_match");
        assert_eq!(*value, 1.0);
        assert_eq!(eval_version, "replay-1");
    }
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_contradicted_decision_is_a_failure() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;
    record(&store, "1.0", version, "2").await;
    record(&store, "1.0", version, "0.5").await;

    // The strategy says Enter on signal 2; this row says Exit, with a correct feature hash.
    let mut contradicted = decide(&Threshold::new("1.0"), context(version, Some("2"), 0));
    contradicted.action = Action::Exit;
    let bad = decision::insert(&client, &contradicted).await.unwrap();

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 2);
    assert_eq!(
        report.failures,
        vec![ReplayFailure {
            decision_id: bad,
            outcome: ReplayOutcome::Mismatch {
                field: "action",
                recorded: "exit".to_owned(),
                replayed: "enter".to_owned(),
            },
        }]
    );
    assert!(!report.is_clean());

    let rows = score_rows(&client).await;
    let failed: Vec<&(i64, String, String, f64, String)> =
        rows.iter().filter(|r| r.0 == bad.0).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].3, 0.0);
    assert_eq!(failed[0].2, "replay_match");
    assert_eq!(rows.iter().filter(|r| r.3 == 1.0).count(), 2);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn tampered_features_are_a_feature_hash_mismatch() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;

    // Stored features changed after the hash was computed.
    let mut tampered = decide(&Threshold::new("1.0"), context(version, Some("0.5"), 0));
    tampered
        .features
        .insert("signal".to_owned(), Decimal::new(99, 0));
    let bad = decision::insert(&client, &tampered).await.unwrap();

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 0);
    assert_eq!(
        report.failures,
        vec![ReplayFailure {
            decision_id: bad,
            outcome: ReplayOutcome::FeatureHashMismatch,
        }]
    );
    let rows = score_rows(&client).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, bad.0);
    assert_eq!(rows[0].3, 0.0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn decisions_under_another_sha_are_counted_and_not_scored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let ours = ensure(&client, &sha('a'), threshold_params("1.0")).await;
    let theirs = ensure(&client, &sha('b'), threshold_params("1.0")).await;

    let mut ours_ids = Vec::new();
    for _ in 0..5 {
        ours_ids.push(record(&store, "1.0", ours, "2").await);
    }
    for _ in 0..3 {
        record(&store, "1.0", theirs, "2").await;
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 5);
    assert_eq!(report.other_builds, 3);
    assert!(report.is_clean());
    let subjects: Vec<i64> = score_rows(&client).await.iter().map(|r| r.0).collect();
    let expected: Vec<i64> = ours_ids.iter().map(|id| id.0).collect();
    assert_eq!(subjects, expected);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_wrong_config_hash_is_unbuildable_and_scored_zero() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    // The params are fine and the factory would accept them; the stored hash is not their hash.
    let wrong = ensure_with_hash(
        &client,
        &sha('a'),
        threshold_params("1.0"),
        Hash32::from_bytes([0xee; 32]),
    )
    .await;
    for _ in 0..3 {
        record(&store, "1.0", wrong, "2").await;
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 0);
    assert_eq!(report.unbuildable, 3);
    assert!(report.failures.is_empty());
    assert!(!report.is_clean());
    let rows = score_rows(&client).await;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.3 == 0.0 && r.2 == "replay_match"));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_factory_rejection_is_unbuildable_and_scored_zero() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let nope = ensure(&client, &sha('a'), json!({"strategy": "nope"})).await;
    for _ in 0..2 {
        // The recording strategy is irrelevant: replay asks the factory, which rejects "nope".
        record(&store, "1.0", nope, "2").await;
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 0);
    assert_eq!(report.unbuildable, 2);
    assert!(!report.is_clean());
    let rows = score_rows(&client).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.3 == 0.0));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn more_than_one_page_is_fully_replayed() {
    let db = ScratchDb::create().await;
    let mut client = db.client().await.unwrap();
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;
    let template = decide(&Threshold::new("1.0"), context(version, Some("2"), 0));

    let tx = client.transaction().await.unwrap();
    for _ in 0..1_200 {
        decision::insert(&tx, &template).await.unwrap();
    }
    tx.commit().await.unwrap();

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report.matched, 1_200);
    assert!(report.is_clean());
    assert_eq!(score_count(&client).await, 1_200);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn after_skips_earlier_decisions() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;

    let mut ids = Vec::new();
    for _ in 0..10 {
        ids.push(record(&store, "1.0", version, "2").await);
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(&client, &sha('a'), ids[3], &factory, UnixNanos::from(T0))
        .await
        .unwrap();

    assert_eq!(report.matched, 6);
    let subjects: Vec<i64> = score_rows(&client).await.iter().map(|r| r.0).collect();
    let expected: Vec<i64> = ids[4..].iter().map(|id| id.0).collect();
    assert_eq!(subjects, expected);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_factory_is_called_once_per_version() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let v1 = ensure(&client, &sha('a'), threshold_params("1.0")).await;
    let v2 = ensure(&client, &sha('a'), threshold_params("2.0")).await;
    let rejected = ensure(&client, &sha('a'), json!({"strategy": "nope"})).await;
    for _ in 0..10 {
        record(&store, "1.0", v1, "2").await;
        record(&store, "2.0", v2, "3").await;
        record(&store, "1.0", rejected, "2").await;
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    // Two buildable versions and one rejected version: three factory calls for 30 decisions.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(report.matched, 20);
    assert_eq!(report.unbuildable, 10);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn running_twice_writes_a_second_set_of_scores() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let store = Store::new(db.client().await.unwrap());
    let version = ensure(&client, &sha('a'), threshold_params("1.0")).await;
    for _ in 0..5 {
        record(&store, "1.0", version, "2").await;
    }

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let first = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();
    assert_eq!(score_count(&client).await, 5);

    let second = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(first, second);
    assert_eq!(score_count(&client).await, 10);
    let rows = score_rows(&client).await;
    assert_eq!(rows[..5], rows[5..]);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_empty_store_gives_an_empty_clean_report() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    let calls = AtomicUsize::new(0);
    let factory = threshold_factory(&calls);
    let report: ReplayReport = replay_decisions(
        &client,
        &sha('a'),
        DecisionId(0),
        &factory,
        UnixNanos::from(T0),
    )
    .await
    .unwrap();

    assert_eq!(report, ReplayReport::default());
    assert!(report.is_clean());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    db.drop_db().await.unwrap();
}
