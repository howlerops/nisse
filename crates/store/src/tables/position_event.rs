//! `position_event`: the position in a symbol after a change.

use nisse_core::PositionEvent;
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
#[allow(unused_variables, reason = "stub: Phase 2 task S3")]
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[PositionEvent],
) -> Result<(), StoreError> {
    todo!("Phase 2 task S3")
}
