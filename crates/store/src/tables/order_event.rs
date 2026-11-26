//! `order_event`: one row per state of an order. The first row for a client order id is the
//! write-ahead intent.

use nisse_core::{OrderEvent, RestoredOrder};
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Inserts one row.
#[allow(unused_variables, reason = "stub: Phase 2 task S2")]
pub async fn insert<C: GenericClient>(client: &C, event: &OrderEvent) -> Result<(), StoreError> {
    todo!("Phase 2 task S2")
}

/// Every order whose latest row is not in a terminal state, in client order id order. An
/// order whose latest state is terminal is not returned.
#[allow(unused_variables, reason = "stub: Phase 2 task S2")]
pub async fn load_open<C: GenericClient>(client: &C) -> Result<Vec<RestoredOrder>, StoreError> {
    todo!("Phase 2 task S2")
}
