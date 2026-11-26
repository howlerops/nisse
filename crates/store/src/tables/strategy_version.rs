//! `strategy_version`: one immutable strategy configuration at one git SHA.

use nisse_core::{StrategyVersion, StrategyVersionId};
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Returns the id of the row for `(git_sha, config_hash)`, inserting it if it does not exist.
/// Safe to call concurrently and repeatedly: the unique constraint decides, and an existing
/// row is never changed (its `params`, `parent_id` and `event_ts` stay as first written).
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn ensure<C: GenericClient>(
    client: &C,
    version: &StrategyVersion,
) -> Result<StrategyVersionId, StoreError> {
    todo!("Phase 2 task S1")
}

/// Reads one version. `Ok(None)` when the id does not exist.
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn get<C: GenericClient>(
    client: &C,
    id: StrategyVersionId,
) -> Result<Option<StrategyVersion>, StoreError> {
    todo!("Phase 2 task S1")
}
