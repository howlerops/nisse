//! `eval_score`: one metric value for a decision or strategy version.

use nisse_core::EvalScore;
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
#[allow(unused_variables, reason = "stub: Phase 2 task S3")]
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[EvalScore],
) -> Result<(), StoreError> {
    todo!("Phase 2 task S3")
}
