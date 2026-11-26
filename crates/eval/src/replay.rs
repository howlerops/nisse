//! The nightly replay test: every recorded decision, run again from its stored features,
//! parameters and seed, must come out the same.
//!
//! A binary can only replay decisions made by the code it was built from, so the job handles
//! the decisions whose strategy version carries the binary's own git SHA and counts the rest
//! as belonging to other builds. `scripts/replay.sh` runs the job once per SHA found in the
//! store, each from a checkout of that SHA.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use nisse_core::{DecisionId, EvalScore, SubjectType, config_hash, nautilus::UnixNanos};
use nisse_engine::{ReplayOutcome, Strategy, replay};
use nisse_store::tables::{decision, eval_score};
use serde_json::Value;
use tokio_postgres::Client;

use crate::EvalError;

/// Metric name written to `eval_score` for each replayed decision: 1 for a match, 0 otherwise.
pub const REPLAY_METRIC: &str = "replay_match";
/// Version tag of this job's scoring, stored with every score it writes.
pub const REPLAY_EVAL_VERSION: &str = "replay-1";
/// Decisions read from the store per page.
pub const REPLAY_PAGE_SIZE: i64 = 500;

/// Builds the strategy a `strategy_version.params` value describes. Phase 3 registers the
/// real strategies; tests pass their own.
pub type StrategyFactory<'a> =
    &'a (dyn Fn(&Value) -> Result<Box<dyn Strategy>, EvalError> + Send + Sync);

/// One decision that did not replay.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayFailure {
    pub decision_id: DecisionId,
    pub outcome: ReplayOutcome,
}

/// What one run of the job found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReplayReport {
    /// Decisions replayed and found identical.
    pub matched: u64,
    /// Decisions that did not replay, oldest first. Any entry here is an alert.
    pub failures: Vec<ReplayFailure>,
    /// Decisions made by another build (a different git SHA): not replayable by this binary.
    pub other_builds: u64,
    /// Decisions whose strategy could not be built from its stored parameters, or whose
    /// stored `config_hash` is not the hash of its stored parameters. Also an alert.
    pub unbuildable: u64,
}

impl ReplayReport {
    /// Nothing failed and nothing was unbuildable.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty() && self.unbuildable == 0
    }
}

/// Replays every stored decision with `id > after` whose strategy version has `git_sha`.
///
/// Pages through `decision` oldest first ([`REPLAY_PAGE_SIZE`] rows at a time). For each row:
/// - another build's SHA: count it in `other_builds`;
/// - stored `config_hash` differs from `nisse_core::config_hash(params)`, or `factory`
///   fails: count it in `unbuildable`;
/// - otherwise `nisse_engine::replay` it: `Match` adds to `matched`, anything else is
///   pushed to `failures`.
///
/// Each replayed or unbuildable decision gets one `eval_score` row (`subject_type`
/// `decision`, metric [`REPLAY_METRIC`], value 1 for a match and 0 otherwise, version
/// [`REPLAY_EVAL_VERSION`], time `now`), written one page at a time. Strategies are built
/// once per strategy version, not once per decision.
pub async fn replay_decisions(
    client: &Client,
    git_sha: &str,
    after: DecisionId,
    factory: StrategyFactory<'_>,
    now: UnixNanos,
) -> Result<ReplayReport, EvalError> {
    let page_capacity = usize::try_from(REPLAY_PAGE_SIZE).unwrap_or(0);
    let mut report = ReplayReport::default();
    // `None` caches a version that could not be built, so the factory is not asked again.
    let mut cache: BTreeMap<String, Option<Box<dyn Strategy>>> = BTreeMap::new();
    let mut key = String::with_capacity(git_sha.len() + 1 + 64);
    let mut scores: Vec<EvalScore> = Vec::with_capacity(page_capacity);
    let mut cursor = after;

    loop {
        let page = decision::list_after(client, cursor, REPLAY_PAGE_SIZE).await?;
        if page.is_empty() {
            break;
        }
        let short_page = i64::try_from(page.len()).unwrap_or(i64::MAX) < REPLAY_PAGE_SIZE;

        for stored in &page {
            if stored.version.git_sha != git_sha {
                report.other_builds += 1;
                continue;
            }

            key.clear();
            // Writing to a String cannot fail.
            let _ = write!(
                key,
                "{}:{}",
                stored.version.git_sha, stored.version.config_hash
            );
            if !cache.contains_key(key.as_str()) {
                let built = build(factory, &stored.version.params, &stored.version.config_hash);
                cache.insert(key.clone(), built);
            }
            let Some(strategy) = cache.get(key.as_str()).and_then(Option::as_ref) else {
                report.unbuildable += 1;
                scores.push(score(stored.id, false, now));
                continue;
            };

            match replay(strategy.as_ref(), &stored.decision) {
                ReplayOutcome::Match => {
                    report.matched += 1;
                    scores.push(score(stored.id, true, now));
                }
                outcome => {
                    report.failures.push(ReplayFailure {
                        decision_id: stored.id,
                        outcome,
                    });
                    scores.push(score(stored.id, false, now));
                }
            }
        }

        if let Some(last) = page.last() {
            cursor = last.id;
        }
        eval_score::insert_many(client, &scores).await?;
        scores.clear();
        if short_page {
            break;
        }
    }
    Ok(report)
}

/// Builds the strategy for one version, or `None` when its stored `config_hash` is not the
/// hash of its params or the factory rejects them.
fn build(
    factory: StrategyFactory<'_>,
    params: &Value,
    stored_hash: &nisse_core::Hash32,
) -> Option<Box<dyn Strategy>> {
    if config_hash(params) != *stored_hash {
        return None;
    }
    factory(params).ok()
}

/// The score row for one decision. Kept separate so the mapping is tested without a database.
fn score(decision_id: DecisionId, matched: bool, now: UnixNanos) -> EvalScore {
    EvalScore {
        subject_id: decision_id.0,
        subject_type: SubjectType::Decision,
        metric: REPLAY_METRIC.to_owned(),
        value: if matched { 1.0 } else { 0.0 },
        eval_version: REPLAY_EVAL_VERSION.to_owned(),
        ts: now,
    }
}

#[cfg(test)]
mod tests {
    use nisse_core::{Action, Features, Hash32};
    use nisse_engine::Verdict;
    use rust_decimal::Decimal;
    use serde_json::json;

    use super::*;

    struct Fixed;

    impl Strategy for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }

        fn decide(&self, _features: &Features, _seed: u64) -> Verdict {
            Verdict {
                action: Action::Hold,
                intended_qty: Decimal::ZERO,
                expected_edge_bps: None,
                expected_vol: None,
                reason_code: "fixed",
            }
        }
    }

    fn failure(id: i64) -> ReplayFailure {
        ReplayFailure {
            decision_id: DecisionId(id),
            outcome: ReplayOutcome::FeatureHashMismatch,
        }
    }

    #[test]
    fn score_maps_a_match_to_one() {
        let row = score(DecisionId(7), true, UnixNanos::from(99_u64));
        assert_eq!(
            row,
            EvalScore {
                subject_id: 7,
                subject_type: SubjectType::Decision,
                metric: "replay_match".to_owned(),
                value: 1.0,
                eval_version: "replay-1".to_owned(),
                ts: UnixNanos::from(99_u64),
            }
        );
    }

    #[test]
    fn score_maps_a_failure_to_zero() {
        let row = score(DecisionId(8), false, UnixNanos::from(5_u64));
        assert_eq!(row.subject_id, 8);
        assert_eq!(row.value, 0.0);
        assert_eq!(row.metric, REPLAY_METRIC);
        assert_eq!(row.eval_version, REPLAY_EVAL_VERSION);
        assert_eq!(row.ts, UnixNanos::from(5_u64));
    }

    #[test]
    fn empty_report_is_clean() {
        assert!(ReplayReport::default().is_clean());
    }

    #[test]
    fn other_builds_alone_are_clean() {
        let report = ReplayReport {
            other_builds: 3,
            ..ReplayReport::default()
        };
        assert!(report.is_clean());
    }

    #[test]
    fn a_failure_is_not_clean() {
        let report = ReplayReport {
            failures: vec![failure(1)],
            ..ReplayReport::default()
        };
        assert!(!report.is_clean());
    }

    #[test]
    fn unbuildable_is_not_clean() {
        let report = ReplayReport {
            unbuildable: 1,
            ..ReplayReport::default()
        };
        assert!(!report.is_clean());
    }

    #[test]
    fn build_refuses_params_whose_hash_differs() {
        let params = json!({"strategy": "fixed"});
        let wrong = Hash32::from_bytes([0xee; 32]);
        let factory = |_: &Value| -> Result<Box<dyn Strategy>, EvalError> { Ok(Box::new(Fixed)) };
        assert!(build(&factory, &params, &wrong).is_none());
    }

    #[test]
    fn build_calls_the_factory_once_for_a_good_hash() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let params = json!({"strategy": "fixed"});
        let calls = AtomicUsize::new(0);
        let factory = |_: &Value| -> Result<Box<dyn Strategy>, EvalError> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Fixed))
        };
        let built = build(&factory, &params, &config_hash(&params));
        assert!(built.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn build_maps_a_factory_error_to_none() {
        let params = json!({"strategy": "nope"});
        let factory = |p: &Value| -> Result<Box<dyn Strategy>, EvalError> {
            Err(EvalError::UnknownStrategy(
                p["strategy"].as_str().unwrap_or("<missing>").to_owned(),
            ))
        };
        assert!(build(&factory, &params, &config_hash(&params)).is_none());
    }
}
