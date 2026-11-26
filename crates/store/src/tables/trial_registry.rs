//! `trial_registry`: one row per evaluation ever run. The row count is the lifetime trial
//! count and never resets.

use nisse_core::Trial;
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Registers a trial and returns its id. `data_range` is `[data_start, data_end)`.
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn insert<C: GenericClient>(client: &C, trial: &Trial) -> Result<i64, StoreError> {
    todo!("Phase 2 task S1")
}

/// The lifetime trial count: every row, of every kind and every strategy version.
#[allow(unused_variables, reason = "stub: Phase 2 task S1")]
pub async fn lifetime_count<C: GenericClient>(client: &C) -> Result<i64, StoreError> {
    todo!("Phase 2 task S1")
}
