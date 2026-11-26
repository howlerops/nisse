//! `decision`: one row per strategy decision per symbol, including holds and skips.

use nisse_core::{Decision, DecisionId, StrategyVersion};
use tokio_postgres::GenericClient;

use crate::StoreError;

/// A decision as stored, with the strategy version it was made by.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDecision {
    pub id: DecisionId,
    pub decision: Decision,
    pub version: StrategyVersion,
}

/// Inserts one decision and returns its id. `features` is stored as the JSON produced by
/// `nisse_core::features_to_json`; `rng_seed` is stored as the `i64` with the same bits.
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn insert<C: GenericClient>(
    client: &C,
    decision: &Decision,
) -> Result<DecisionId, StoreError> {
    todo!("Phase 2 task S1")
}

/// Decisions with `id > after_id`, oldest first, at most `limit`, joined to their strategy
/// version. The replay job pages through the table with this.
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn list_after<C: GenericClient>(
    client: &C,
    after_id: DecisionId,
    limit: i64,
) -> Result<Vec<StoredDecision>, StoreError> {
    todo!("Phase 2 task S1")
}
