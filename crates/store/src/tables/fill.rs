//! `fill`: one row per fill, unique on the broker execution id.

use nisse_core::Fill;
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Inserts one fill with `fee` 0 (Alpaca reports fees separately; they are added by the
/// evaluation jobs). Returns `false`, changing nothing, when a row with the same
/// `broker_fill_id` already exists: a fill delivered twice is recorded once.
#[allow(unused_variables, reason = "stub: Phase 2 task S2")]
pub async fn insert<C: GenericClient>(client: &C, fill: &Fill) -> Result<bool, StoreError> {
    todo!("Phase 2 task S2")
}
