//! The nightly replay test: every recorded decision, run again from its stored features,
//! parameters and seed, must come out the same.
//!
//! A binary can only replay decisions made by the code it was built from, so the job handles
//! the decisions whose strategy version carries the binary's own git SHA and counts the rest
//! as belonging to other builds. `scripts/replay.sh` runs the job once per SHA found in the
//! store, each from a checkout of that SHA.

use nisse_core::{DecisionId, EvalScore, nautilus::UnixNanos};
use nisse_engine::{ReplayOutcome, Strategy};
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
#[allow(unused_variables, reason = "stub: Phase 2 task V1")]
pub async fn replay_decisions(
    client: &Client,
    git_sha: &str,
    after: DecisionId,
    factory: StrategyFactory<'_>,
    now: UnixNanos,
) -> Result<ReplayReport, EvalError> {
    todo!("Phase 2 task V1")
}

/// The score row for one decision. Kept separate so the mapping is tested without a database.
#[allow(dead_code, unused_variables, reason = "stub: Phase 2 task V1")]
fn score(decision_id: DecisionId, matched: bool, now: UnixNanos) -> EvalScore {
    todo!("Phase 2 task V1")
}
