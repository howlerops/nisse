//! `trial_result`: the outcome of a trial, written when the run finishes. The trial itself
//! is registered in `trial_registry` before the run, so a run that crashes still counts.

use nisse_core::nautilus::UnixNanos;
use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Stores the result of trial `trial_id`. A trial has at most one result: a second insert
/// for the same trial is a `Db` error (unique violation), not an overwrite.
pub async fn insert<C: GenericClient>(
    client: &C,
    trial_id: i64,
    result: &Value,
    ts: UnixNanos,
) -> Result<(), StoreError> {
    let event_ts = to_ts(ts)?;
    client
        .execute(
            "INSERT INTO trial_result (trial_id, result, event_ts) VALUES ($1, $2, $3)",
            &[&trial_id, &result, &event_ts],
        )
        .await?;
    Ok(())
}

/// The result of trial `trial_id`; `None` when the trial has none (it is still running, or
/// it crashed).
pub async fn get<C: GenericClient>(client: &C, trial_id: i64) -> Result<Option<Value>, StoreError> {
    let row = client
        .query_opt(
            "SELECT result FROM trial_result WHERE trial_id = $1",
            &[&trial_id],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.try_get(0)?)),
        None => Ok(None),
    }
}
