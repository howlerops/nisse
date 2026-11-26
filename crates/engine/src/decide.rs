//! Making, recording and replaying decisions.

use nisse_core::{
    Decision, DecisionId, DecisionLog, Features, StrategyVersionId,
    nautilus::{Symbol, UnixNanos},
};

use crate::{EngineError, Strategy};

/// Everything about one decision point that does not come from the strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionContext {
    pub strategy_version_id: StrategyVersionId,
    pub symbol: Symbol,
    pub features: Features,
    /// Names the market data the features were computed from.
    pub data_snapshot_id: String,
    pub rng_seed: u64,
    /// From the engine's injected clock.
    pub ts: UnixNanos,
}

/// The result of replaying a recorded decision.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    /// The strategy made the same decision again.
    Match,
    /// The stored `feature_hash` is not the hash of the stored features: the row was
    /// corrupted or tampered with, so there is nothing sound to replay.
    FeatureHashMismatch,
    /// The strategy decided differently. `field` names the first difference found, checked
    /// in this order: `action`, `intended_qty`, `reason_code`, `expected_edge_bps`,
    /// `expected_vol`.
    Mismatch {
        field: &'static str,
        recorded: String,
        replayed: String,
    },
}

/// Builds the [`Decision`] a strategy makes in `context`, without recording it.
///
/// Calls `strategy.decide(&context.features, context.rng_seed)` exactly once, and computes
/// `feature_hash` with [`nisse_core::feature_hash`].
#[allow(unused_variables, reason = "stub: Phase 2 task E1")]
#[must_use]
pub fn decide<S: Strategy + ?Sized>(strategy: &S, context: DecisionContext) -> Decision {
    todo!("Phase 2 task E1")
}

/// Makes a decision and records it, returning its id. Every decision is recorded, holds and
/// skips included. If the log fails the decision is returned to nobody: the caller gets
/// `DecisionNotRecorded` and must not act on a decision that is not stored.
#[allow(unused_variables, reason = "stub: Phase 2 task E1")]
pub async fn decide_and_record<S: Strategy + ?Sized, L: DecisionLog>(
    strategy: &S,
    log: &L,
    context: DecisionContext,
) -> Result<(DecisionId, Decision), EngineError> {
    todo!("Phase 2 task E1")
}

/// Replays a recorded decision: checks the feature hash, runs the strategy on the recorded
/// features and seed, and compares the result with what was recorded. `f64` fields are
/// compared by bit pattern, so `NaN` equals `NaN` and `0.0` differs from `-0.0`: replay asks
/// for the same bits, not for closeness.
#[allow(unused_variables, reason = "stub: Phase 2 task E1")]
#[must_use]
pub fn replay<S: Strategy + ?Sized>(strategy: &S, recorded: &Decision) -> ReplayOutcome {
    todo!("Phase 2 task E1")
}
