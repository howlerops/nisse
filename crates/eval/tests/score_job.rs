//! Acceptance tests for the scoring job, part one (phase 5, JB1a): the decision holdout and the
//! outcome windows, against a real Postgres (`TEST_DATABASE_URL`), with synthetic bars written
//! to and read back from Parquet.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use nisse_bars::{BarReader, write_bars};
use nisse_core::{
    Action, Decision, DecisionId, EvalScore, Hash32, StrategyVersion, StrategyVersionId,
    SubjectType,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Symbol, UnixNanos, Venue,
    },
};
use nisse_eval::{
    EvalError,
    gates::Gates,
    outcome::{BarPoint, decision_quality, outcome},
    score_job::{
        EVAL_VERSION, HoldoutReport, OutcomeReport, assign_holdouts, is_hidden, score_outcomes,
    },
};
use nisse_store::{
    tables::{decision, decision_holdout, eval_score, outcome_window, strategy_version},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds: the `now` every job is given.
const NOW: u64 = 1_767_600_000_123_456_000;
/// 2023-01-02T00:00:00Z: the first session. Whole seconds, so decision timestamps round-trip
/// through `TIMESTAMPTZ` exactly.
const BASE: u64 = 1_672_617_600_000_000_000;
const DAY: u64 = 86_400_000_000_000;
/// Sessions of synthetic bars in most tests.
const SESSIONS: u64 = 30;
const SALT: &[u8] = b"0123456789abcdef";

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp directory, unique to one test and removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("nisse-score-job-{}-{n}-{name}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `TIMESTAMPTZ` value a whole-microsecond engine time is stored as.
fn micros(nanos: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(i64::try_from(nanos / 1_000).unwrap()).unwrap()
}

fn now() -> UnixNanos {
    UnixNanos::from(NOW)
}

fn session_ts(session: u64) -> UnixNanos {
    UnixNanos::from(BASE + session * DAY)
}

fn gates() -> Gates {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/eval/gates.json"
    ));
    Gates::load(path).unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}

// ---- synthetic bars -------------------------------------------------------------------------

/// AAA closes at 100 + session, with high 2 above and low 1 below the close.
fn aaa_point(session: u64) -> BarPoint {
    let close = 100.0 + session as f64;
    BarPoint {
        ts: session_ts(session),
        high: close + 2.0,
        low: close - 1.0,
        close,
    }
}

/// SPY (the benchmark) closes at 200 + session, with high 1 above and low 1 below.
fn spy_point(session: u64) -> BarPoint {
    let close = 200.0 + session as f64;
    BarPoint {
        ts: session_ts(session),
        high: close + 1.0,
        low: close - 1.0,
        close,
    }
}

fn aaa_points(sessions: u64) -> Vec<BarPoint> {
    (0..sessions).map(aaa_point).collect()
}

fn spy_points(sessions: u64) -> Vec<BarPoint> {
    (0..sessions).map(spy_point).collect()
}

/// A price at exactly 4 decimal places (a bar's prices must share one precision).
fn px(value: f64) -> Price {
    Price::from(format!("{value:.4}").as_str())
}

fn bar(symbol: &str, point: BarPoint) -> Bar {
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    Bar::new(
        bar_type,
        px(point.close),
        px(point.high),
        px(point.low),
        px(point.close),
        Quantity::from("1000"),
        point.ts,
        point.ts,
    )
}

/// AAA and SPY for `sessions` sessions, sorted by time then symbol. `skip_spy` leaves out the
/// benchmark's bar for one session.
fn bars_for(sessions: u64, skip_spy: Option<u64>) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(2 * sessions as usize);
    for session in 0..sessions {
        bars.push(bar("AAA", aaa_point(session)));
        if skip_spy != Some(session) {
            bars.push(bar("SPY", spy_point(session)));
        }
    }
    bars
}

// ---- seeding --------------------------------------------------------------------------------

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

/// Creates one strategy version and returns its id.
async fn seed_version(client: &Client) -> StrategyVersionId {
    let params = json!({"strategy": "threshold", "lookback": 252});
    let version = StrategyVersion {
        git_sha: "a".repeat(40),
        config_hash: nisse_core::config_hash(&params),
        params,
        parent_id: None,
        ts: UnixNanos::from(NOW),
    };
    strategy_version::ensure(client, &version).await.unwrap()
}

/// One decision through `decision::insert`.
async fn seed_decision(
    client: &Client,
    version: StrategyVersionId,
    symbol: &str,
    ts: UnixNanos,
    expected_vol: Option<f64>,
) -> DecisionId {
    let decision = Decision {
        strategy_version_id: version,
        symbol: Symbol::new(symbol),
        action: Action::Enter,
        intended_qty: dec("1.5"),
        expected_edge_bps: None,
        expected_vol,
        features: BTreeMap::new(),
        feature_hash: Hash32::from_bytes([0xab; 32]),
        data_snapshot_id: "snap-1".to_string(),
        rng_seed: 42,
        reason_code: "trend_up".to_string(),
        ts,
    };
    decision::insert(client, &decision).await.unwrap()
}

/// `count` decisions for `symbol` at `ts` in one SQL statement, with `expected_vol` on each.
/// Used where a test needs more rows than is practical through `decision::insert`; the
/// columns are the ones `decision::insert` writes.
async fn seed_bulk(
    client: &Client,
    version: StrategyVersionId,
    count: i64,
    symbol: &str,
    ts: UnixNanos,
    expected_vol: Option<f64>,
) {
    let event_ts = micros(ts.as_u64());
    client
        .execute(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, \
                 expected_edge_bps, expected_vol, features, feature_hash, data_snapshot_id, \
                 rng_seed, reason_code, event_ts) \
             SELECT $1, $2, 'enter', 1.5, NULL, $3::float8, '{}'::jsonb, \
                 decode(repeat('ab', 32), 'hex'), 'snap-1', 42, 'trend_up', \
                 $4::timestamptz + make_interval(secs => g::float8) \
             FROM generate_series(1, $5::int8) AS g",
            &[&version.0, &symbol, &expected_vol, &event_ts, &count],
        )
        .await
        .unwrap();
}

/// A fixture: a scratch database with one strategy version, and a bars directory holding
/// `bars` as `0001-bars.parquet`.
struct Fixture {
    db: ScratchDb,
    client: Client,
    dir: TempDir,
    version: StrategyVersionId,
}

impl Fixture {
    async fn new(name: &str, bars: &[Bar]) -> Self {
        let db = ScratchDb::create().await;
        let client = db.client().await.unwrap();
        let version = seed_version(&client).await;
        let dir = TempDir::new(name);
        write_bars(&dir.path().join("0001-bars.parquet"), bars).unwrap();
        Self {
            db,
            client,
            dir,
            version,
        }
    }

    fn reader(&self) -> BarReader {
        BarReader::open(self.dir.path()).unwrap()
    }

    fn add_bars(&self, file_name: &str, bars: &[Bar]) {
        write_bars(&self.dir.path().join(file_name), bars).unwrap();
    }

    async fn decision(&self, symbol: &str, session: u64, expected_vol: Option<f64>) -> DecisionId {
        seed_decision(
            &self.client,
            self.version,
            symbol,
            session_ts(session),
            expected_vol,
        )
        .await
    }

    async fn finish(self) {
        let Self {
            db, client, dir, ..
        } = self;
        drop(client);
        drop(dir);
        db.drop_db().await.unwrap();
    }
}

// ---- queries --------------------------------------------------------------------------------

async fn count_all(client: &Client, table: &str) -> i64 {
    client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0)
}

async fn outcome_count(client: &Client, decision: DecisionId) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM outcome_window WHERE decision_id = $1",
            &[&decision.0],
        )
        .await
        .unwrap()
        .get(0)
}

/// The decision-quality score rows of one decision: `(metric, value, eval_version,
/// subject_type)`, ordered by metric.
async fn quality_scores(
    client: &Client,
    decision: DecisionId,
) -> Vec<(String, f64, String, String)> {
    client
        .query(
            "SELECT metric, value, eval_version, subject_type FROM eval_score \
             WHERE subject_id = $1 AND metric LIKE 'decision_quality_%' ORDER BY metric",
            &[&decision.0],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}

async fn hidden_of(client: &Client, decision: DecisionId) -> bool {
    client
        .query_one(
            "SELECT hidden FROM decision_holdout WHERE decision_id = $1",
            &[&decision.0],
        )
        .await
        .unwrap()
        .get(0)
}

async fn all_decision_ids(client: &Client) -> Vec<DecisionId> {
    client
        .query("SELECT id FROM decision ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| DecisionId(row.get(0)))
        .collect()
}

// ---- is_hidden ------------------------------------------------------------------------------

/// The reference: the first 8 bytes of SHA-256 over the salt then the decimal text, big-endian,
/// modulo 100, below 25.
fn reference_hidden(salt: &[u8], id: i64) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(id.to_string().as_bytes());
    let digest = hasher.finalize();
    let head = u64::from_be_bytes(digest[..8].try_into().unwrap());
    head % 100 < 25
}

#[test]
fn is_hidden_is_deterministic_and_matches_the_reference_encoding() {
    for id in 1..=5 {
        let first = is_hidden(SALT, DecisionId(id));
        for _ in 0..10 {
            assert_eq!(is_hidden(SALT, DecisionId(id)), first);
        }
        assert_eq!(first, reference_hidden(SALT, id), "id {id}");
    }
    // A longer sweep pins the byte order and the decimal encoding: another encoding disagrees
    // on about half of these.
    for id in 1..=500 {
        assert_eq!(
            is_hidden(SALT, DecisionId(id)),
            reference_hidden(SALT, id),
            "id {id}"
        );
    }
}

#[test]
fn is_hidden_hashes_the_salt_then_the_decimal_text() {
    // The hash input for id 12 is the salt followed by the two bytes "12".
    let input = [SALT, b"12"].concat();
    let digest: [u8; 32] = Sha256::digest(&input).into();
    let head = u64::from_be_bytes(digest[..8].try_into().unwrap());
    assert_eq!(is_hidden(SALT, DecisionId(12)), head % 100 < 25);
}

#[test]
fn about_a_quarter_of_ids_are_hidden_and_a_salt_reshuffles_them() {
    let hidden = (1..=20_000)
        .filter(|&id| is_hidden(SALT, DecisionId(id)))
        .count();
    let share = hidden as f64 / 20_000.0;
    assert!(
        (0.23..=0.27).contains(&share),
        "hidden share {share} is not about 25%"
    );

    let other_salt = b"fedcba9876543210";
    let differing = (1..=20_000)
        .filter(|&id| is_hidden(SALT, DecisionId(id)) != is_hidden(other_salt, DecisionId(id)))
        .count();
    assert!(differing >= 5_000, "only {differing} ids changed");
}

// ---- assign_holdouts ------------------------------------------------------------------------

#[tokio::test]
async fn assign_holdouts_assigns_every_decision_once_in_batches() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let version = seed_version(&client).await;
    seed_bulk(&client, version, 2_500, "AAA", session_ts(0), None).await;

    let ids = all_decision_ids(&client).await;
    assert_eq!(ids.len(), 2_500);
    let expected_hidden = ids.iter().filter(|&&id| is_hidden(SALT, id)).count() as u64;

    let report = assign_holdouts(&client, SALT, now()).await.unwrap();
    assert_eq!(
        report,
        HoldoutReport {
            assigned: 2_500,
            hidden: expected_hidden
        }
    );
    let (hidden, visible) = decision_holdout::counts(&client).await.unwrap();
    assert_eq!(hidden as u64, expected_hidden);
    assert_eq!(visible as u64, 2_500 - expected_hidden);
    assert!(
        decision_holdout::unassigned(&client, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // A second run assigns nothing and changes nothing.
    let again = assign_holdouts(&client, SALT, now()).await.unwrap();
    assert_eq!(again, HoldoutReport::default());
    assert_eq!(
        decision_holdout::counts(&client).await.unwrap(),
        (hidden, visible)
    );

    // Three new decisions: exactly three assignments.
    let mut new_ids = Vec::new();
    for i in 0..3 {
        new_ids.push(seed_decision(&client, version, "QQQ", session_ts(i), None).await);
    }
    let expected_new_hidden = new_ids.iter().filter(|&&id| is_hidden(SALT, id)).count() as u64;
    let third = assign_holdouts(&client, SALT, now()).await.unwrap();
    assert_eq!(
        third,
        HoldoutReport {
            assigned: 3,
            hidden: expected_new_hidden
        }
    );
    assert_eq!(
        decision_holdout::counts(&client).await.unwrap().0 as u64,
        expected_hidden + expected_new_hidden
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_short_salt_is_refused_and_nothing_is_written() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let version = seed_version(&client).await;
    seed_bulk(&client, version, 40, "AAA", session_ts(0), None).await;

    let short: &[u8] = b"0123456789abcde"; // 15 bytes
    let err = assign_holdouts(&client, short, now()).await.unwrap_err();
    assert!(matches!(err, EvalError::InvalidJob(_)), "got {err:?}");
    assert_eq!(count_all(&client, "decision_holdout").await, 0);

    // A valid salt (exactly 16 bytes is the minimum) then stores each sampled decision's
    // assignment as the pure function says.
    let report = assign_holdouts(&client, SALT, now()).await.unwrap();
    assert_eq!(report.assigned, 40);
    let ids = all_decision_ids(&client).await;
    for id in ids.iter().take(20) {
        assert_eq!(
            hidden_of(&client, *id).await,
            is_hidden(SALT, *id),
            "decision {}",
            id.0
        );
    }

    drop(client);
    db.drop_db().await.unwrap();
}

// ---- score_outcomes -------------------------------------------------------------------------

#[tokio::test]
async fn a_session_zero_decision_gets_three_windows_and_three_scores() {
    let fx = Fixture::new("session-zero", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("AAA", 0, Some(0.16)).await;
    let reader = fx.reader();
    let gates = gates();

    let report = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3,
            quality_scores: 3,
            waiting: 0,
            base_too_old: 0
        }
    );

    let rows = outcome_window::for_decision(&fx.client, id).await.unwrap();
    let horizons: Vec<u32> = rows.iter().map(|row| row.horizon_sessions).collect();
    assert_eq!(horizons, vec![1, 5, 21]);

    let symbol_bars = aaa_points(SESSIONS);
    let benchmark_bars = spy_points(SESSIONS);
    for row in &rows {
        let expected = outcome(
            &symbol_bars,
            &benchmark_bars,
            session_ts(0),
            row.horizon_sessions,
        )
        .unwrap();
        assert_close(row.fwd_ret, expected.fwd_ret);
        assert_close(row.bench_ret, expected.bench_ret);
        assert_close(row.mfe, expected.mfe);
        assert_close(row.mae, expected.mae);
        assert_eq!(row.ts, now());
    }

    // Hand-computed for horizon 1: AAA 100 → 101 (+1%), SPY 200 → 201 (+0.5%), the day-1 high
    // 103 (+3%), the day-1 low 100 (0%).
    let one = rows[0];
    assert_close(one.fwd_ret, 0.01);
    assert_close(one.bench_ret, 0.005);
    assert_close(one.mfe, 0.03);
    assert_close(one.mae, 0.0);

    let scores = quality_scores(&fx.client, id).await;
    let metrics: Vec<&str> = scores.iter().map(|s| s.0.as_str()).collect();
    assert_eq!(
        metrics,
        [
            "decision_quality_1",
            "decision_quality_21",
            "decision_quality_5"
        ]
    );
    for (metric, value, eval_version, subject_type) in &scores {
        assert_eq!(eval_version, EVAL_VERSION);
        assert_eq!(subject_type, "decision");
        let horizon: u32 = metric
            .trim_start_matches("decision_quality_")
            .parse()
            .unwrap();
        let expected = outcome(&symbol_bars, &benchmark_bars, session_ts(0), horizon).unwrap();
        let quality = decision_quality(&expected, 0.16, gates.periods_per_year).unwrap();
        assert_close(*value, quality);
    }

    fx.finish().await;
}

#[tokio::test]
async fn a_late_decision_waits_then_is_scored_from_a_later_file() {
    let fx = Fixture::new("late", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("AAA", 20, Some(0.16)).await;
    let reader = fx.reader();
    let gates = gates();

    // Session 20 + 21 sessions needs session 41; there are 30 sessions so far.
    let first = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(
        first,
        OutcomeReport {
            written: 2,
            quality_scores: 2,
            waiting: 1,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 2);
    assert_eq!(quality_scores(&fx.client, id).await.len(), 2);

    // A later, complete file with 60 sessions: the reader uses it.
    fx.add_bars("0002-bars.parquet", &bars_for(60, None));
    let second = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(
        second,
        OutcomeReport {
            written: 1,
            quality_scores: 1,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 3);
    assert_eq!(quality_scores(&fx.client, id).await.len(), 3);
    let horizons: Vec<u32> = outcome_window::for_decision(&fx.client, id)
        .await
        .unwrap()
        .iter()
        .map(|row| row.horizon_sessions)
        .collect();
    assert_eq!(horizons, vec![1, 5, 21]);

    // Nothing is left to write, so a third run writes nothing.
    let third = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(third, OutcomeReport::default());
    assert_eq!(outcome_count(&fx.client, id).await, 3);
    assert_eq!(quality_scores(&fx.client, id).await.len(), 3);

    fx.finish().await;
}

#[tokio::test]
async fn a_null_expected_vol_gets_windows_and_no_quality_score() {
    let fx = Fixture::new("null-vol", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("AAA", 0, None).await;
    let reader = fx.reader();

    let report = score_outcomes(&fx.client, &reader, &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3,
            quality_scores: 0,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 3);
    assert!(quality_scores(&fx.client, id).await.is_empty());

    fx.finish().await;
}

#[tokio::test]
async fn a_symbol_without_bars_waits_for_every_horizon() {
    let fx = Fixture::new("no-bars", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("ZZZ", 0, Some(0.16)).await;
    let reader = fx.reader();

    let report = score_outcomes(&fx.client, &reader, &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 0,
            quality_scores: 0,
            waiting: 3,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 0);
    assert!(quality_scores(&fx.client, id).await.is_empty());

    fx.finish().await;
}

#[tokio::test]
async fn a_missing_benchmark_bar_on_the_decision_day_makes_it_wait() {
    // SPY has no bar on session 3.
    let fx = Fixture::new("no-benchmark-bar", &bars_for(SESSIONS, Some(3))).await;
    let on_gap = fx.decision("AAA", 3, Some(0.16)).await;
    let before_gap = fx.decision("AAA", 0, Some(0.16)).await;
    let reader = fx.reader();

    let report = score_outcomes(&fx.client, &reader, &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3,
            quality_scores: 3,
            waiting: 3,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, on_gap).await, 0);
    assert_eq!(outcome_count(&fx.client, before_gap).await, 3);

    fx.finish().await;
}

#[tokio::test]
async fn a_second_run_writes_nothing() {
    let fx = Fixture::new("idempotent", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("AAA", 0, Some(0.16)).await;
    let reader = fx.reader();
    let gates = gates();

    let first = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(first.written, 3);
    let outcomes_before = count_all(&fx.client, "outcome_window").await;
    let scores_before = count_all(&fx.client, "eval_score").await;

    let second = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(second, OutcomeReport::default());
    assert_eq!(
        count_all(&fx.client, "outcome_window").await,
        outcomes_before
    );
    assert_eq!(count_all(&fx.client, "eval_score").await, scores_before);
    assert_eq!(outcome_count(&fx.client, id).await, 3);

    fx.finish().await;
}

#[tokio::test]
async fn a_crash_between_the_writes_does_not_duplicate_a_score() {
    let fx = Fixture::new("crash", &bars_for(SESSIONS, None)).await;
    let id = fx.decision("AAA", 0, Some(0.16)).await;
    // As if a run crashed after writing the 5-session score and before the windows.
    let stale = EvalScore {
        subject_id: id.0,
        subject_type: SubjectType::Decision,
        metric: "decision_quality_5".to_string(),
        value: 9.99,
        eval_version: EVAL_VERSION.to_string(),
        ts: now(),
    };
    eval_score::insert_many(&fx.client, &[stale]).await.unwrap();
    let reader = fx.reader();

    let report = score_outcomes(&fx.client, &reader, &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3,
            quality_scores: 2,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 3);

    let scores = quality_scores(&fx.client, id).await;
    assert_eq!(scores.len(), 3);
    let five: Vec<&(String, f64, String, String)> = scores
        .iter()
        .filter(|s| s.0 == "decision_quality_5")
        .collect();
    assert_eq!(five.len(), 1);
    assert_close(five[0].1, 9.99);

    fx.finish().await;
}

#[tokio::test]
async fn every_pending_decision_is_scored_not_just_the_first_page() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let version = seed_version(&client).await;
    let dir = TempDir::new("pages");
    write_bars(
        &dir.path().join("0001-bars.parquet"),
        &bars_for(SESSIONS, None),
    )
    .unwrap();
    let reader = BarReader::open(dir.path()).unwrap();
    seed_bulk(&client, version, 1_200, "AAA", session_ts(0), Some(0.16)).await;

    let report = score_outcomes(&client, &reader, &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3_600,
            quality_scores: 3_600,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(count_all(&client, "outcome_window").await, 3_600,);

    drop(client);
    drop(dir);
    db.drop_db().await.unwrap();
}

// ---- base bars too old ----------------------------------------------------------------------

const HOUR: u64 = 3_600_000_000_000;

/// AAA bars for `sessions` sessions except the sessions in `skip_aaa`; SPY for every session.
fn bars_with_aaa_gaps(sessions: u64, skip_aaa: &[u64]) -> Vec<Bar> {
    let mut bars = Vec::new();
    for session in 0..sessions {
        if !skip_aaa.contains(&session) {
            bars.push(bar("AAA", aaa_point(session)));
        }
        bars.push(bar("SPY", spy_point(session)));
    }
    bars
}

#[tokio::test]
async fn a_decision_with_a_too_old_base_bar_is_counted_apart_from_waiting() {
    // AAA has no bars on sessions 1..=3. A decision 64 hours after session 0 has session 0 as its
    // base bar, 64 hours old. Sessions 4.. exist, so every horizon would be scorable otherwise.
    let fx = Fixture::new("too-old", &bars_with_aaa_gaps(SESSIONS, &[1, 2, 3])).await;
    let too_old = seed_decision(
        &fx.client,
        fx.version,
        "AAA",
        UnixNanos::from(BASE + 64 * HOUR),
        Some(0.16),
    )
    .await;
    // The last session: its later bars do not exist yet, so it is merely waiting.
    let late = fx.decision("AAA", SESSIONS - 1, Some(0.16)).await;
    let reader = fx.reader();
    let gates = gates();
    let expected = OutcomeReport {
        written: 0,
        quality_scores: 0,
        waiting: 3,
        base_too_old: 3,
    };

    let first = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(first, expected);
    assert_eq!(outcome_count(&fx.client, too_old).await, 0);
    assert!(quality_scores(&fx.client, too_old).await.is_empty());
    assert_eq!(outcome_count(&fx.client, late).await, 0);

    // Re-read each night: a second run reports the same counts and writes nothing.
    let second = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(second, expected);
    assert_eq!(outcome_count(&fx.client, too_old).await, 0);
    assert_eq!(outcome_count(&fx.client, late).await, 0);

    fx.finish().await;
}

#[tokio::test]
async fn a_decision_19h59m_after_its_bar_is_not_counted_as_too_old() {
    // AAA has every session, so the only thing under test is the age limit: the decision is 19
    // hours 59 minutes after session 0, inside the limit, so it is scored from session 0 and never
    // counted as too old (the exact boundary is pinned in `outcome`).
    let fx = Fixture::new("recent", &bars_for(SESSIONS, None)).await;
    let id = seed_decision(
        &fx.client,
        fx.version,
        "AAA",
        UnixNanos::from(BASE + 19 * HOUR + 59 * 60_000_000_000),
        Some(0.16),
    )
    .await;
    let report = score_outcomes(&fx.client, &fx.reader(), &gates(), now())
        .await
        .unwrap();
    assert_eq!(
        report,
        OutcomeReport {
            written: 3,
            quality_scores: 3,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, id).await, 3);
    assert_eq!(quality_scores(&fx.client, id).await.len(), 3);

    fx.finish().await;
}

#[tokio::test]
async fn a_decision_a_day_after_the_newest_bar_waits_then_is_scored() {
    // The nightly job runs before today's bar is ingested: the file ends at session 29, and the
    // decisions are stamped 24 hours later, at session 30's close. Nothing later exists, so they
    // are waiting, not too old.
    let fx = Fixture::new("day-ahead", &bars_for(SESSIONS, None)).await;
    let first = fx.decision("AAA", SESSIONS, Some(0.16)).await;
    let second = fx.decision("AAA", SESSIONS, Some(0.16)).await;
    let reader = fx.reader();
    let gates = gates();

    let waiting = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(
        waiting,
        OutcomeReport {
            written: 0,
            quality_scores: 0,
            waiting: 6,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, first).await, 0);
    assert_eq!(outcome_count(&fx.client, second).await, 0);

    // Today's bars arrive: a complete file that includes session 30 and the sessions after it.
    fx.add_bars("0002-bars.parquet", &bars_for(60, None));
    let scored = score_outcomes(&fx.client, &reader, &gates, now())
        .await
        .unwrap();
    assert_eq!(
        scored,
        OutcomeReport {
            written: 6,
            quality_scores: 6,
            waiting: 0,
            base_too_old: 0
        }
    );
    assert_eq!(outcome_count(&fx.client, first).await, 3);
    assert_eq!(outcome_count(&fx.client, second).await, 3);
    assert_eq!(quality_scores(&fx.client, first).await.len(), 3);

    fx.finish().await;
}
