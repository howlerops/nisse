//! `trial_registry`: one row per evaluation ever run. The row count is the lifetime trial
//! count and never resets.

use nisse_core::Trial;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Registers a trial and returns its id. `data_range` is `[data_start, data_end)`.
pub async fn insert<C: GenericClient>(client: &C, trial: &Trial) -> Result<i64, StoreError> {
    let kind: &'static str = trial.kind.into();
    let data_start = to_ts(trial.data_start)?;
    let data_end = to_ts(trial.data_end)?;
    let event_ts = to_ts(trial.ts)?;

    let row = client
        .query_one(
            "INSERT INTO trial_registry (strategy_version_id, kind, data_range, result, event_ts) \
             VALUES ($1, $2, tstzrange($3, $4, '[)'), $5, $6) \
             RETURNING id",
            &[
                &trial.strategy_version_id.0,
                &kind,
                &data_start,
                &data_end,
                &trial.result.as_ref(),
                &event_ts,
            ],
        )
        .await?;
    Ok(row.try_get(0)?)
}

/// The lifetime trial count: every row, of every kind and every strategy version.
pub async fn lifetime_count<C: GenericClient>(client: &C) -> Result<i64, StoreError> {
    let row = client
        .query_one("SELECT COUNT(*) FROM trial_registry", &[])
        .await?;
    Ok(row.try_get(0)?)
}
