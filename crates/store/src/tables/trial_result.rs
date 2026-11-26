//! `trial_result`: the outcome of a trial, written when the run finishes. The trial itself
//! is registered in `trial_registry` before the run, so a run that crashes still counts.

use nisse_core::nautilus::UnixNanos;
use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::StoreError;

/// Stores the result of trial `trial_id`. A trial has at most one result: a second insert
/// for the same trial is a `Db` error (unique violation), not an overwrite.
#[allow(unused_variables, reason = "stub: Phase 3 task TR1")]
pub async fn insert<C: GenericClient>(
    client: &C,
    trial_id: i64,
    result: &Value,
    ts: UnixNanos,
) -> Result<(), StoreError> {
    todo!("Phase 3 task TR1")
}

/// The result of trial `trial_id`; `None` when the trial has none (it is still running, or
/// it crashed).
#[allow(unused_variables, reason = "stub: Phase 3 task TR1")]
pub async fn get<C: GenericClient>(client: &C, trial_id: i64) -> Result<Option<Value>, StoreError> {
    todo!("Phase 3 task TR1")
}
