//! `trial_registry`: one row per evaluation ever run. The row count is the lifetime trial
//! count and never resets.

use chrono::{DateTime, Utc};
use nisse_core::nautilus::UnixNanos;
use nisse_core::{StrategyVersionId, Trial};
use tokio_postgres::GenericClient;

use crate::{
    StoreError,
    db::{from_ts, to_ts},
};

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

/// Registers the trial of an engine run: kind `live`, a `data_range` from `start` with no end
/// (the run is still going), and the result `{"environment": <environment>}`. Returns its id.
pub async fn insert_live<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: &str,
    start: UnixNanos,
    ts: UnixNanos,
) -> Result<i64, StoreError> {
    let data_start = to_ts(start)?;
    let event_ts = to_ts(ts)?;

    let row = client
        .query_one(
            "INSERT INTO trial_registry (strategy_version_id, kind, data_range, result, event_ts) \
             VALUES ($1, 'live', tstzrange($2, NULL, '[)'), \
                     jsonb_build_object('environment', $3::text), $4) \
             RETURNING id",
            &[&version.0, &data_start, &environment, &event_ts],
        )
        .await?;
    Ok(row.try_get(0)?)
}

/// When the engine first ran `version`'s strategy config: the start of the oldest `live` trial
/// of any strategy version with the same `config_hash`. With `environment` given, only trials
/// registered for that environment count. `None` when there is none. One statement.
pub async fn live_start<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: Option<&str>,
) -> Result<Option<UnixNanos>, StoreError> {
    let row = client
        .query_opt(
            "SELECT lower(t.data_range) FROM trial_registry t \
             JOIN strategy_version v ON v.id = t.strategy_version_id \
             WHERE t.kind = 'live' \
               AND v.config_hash = (SELECT config_hash FROM strategy_version WHERE id = $1) \
               AND ($2::text IS NULL OR t.result ->> 'environment' = $2) \
             ORDER BY t.id LIMIT 1",
            &[&version.0, &environment],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let start: Option<DateTime<Utc>> = row.try_get(0)?;
    start.map(from_ts).transpose()
}

/// The lifetime trial count: every row, of every kind and every strategy version.
pub async fn lifetime_count<C: GenericClient>(client: &C) -> Result<i64, StoreError> {
    let row = client
        .query_one("SELECT COUNT(*) FROM trial_registry", &[])
        .await?;
    Ok(row.try_get(0)?)
}

/// The number of holdout looks: trials registered with `"holdout": true` in their result,
/// whether or not their run finished. Counts every strategy version.
pub async fn holdout_count<C: GenericClient>(client: &C) -> Result<i64, StoreError> {
    let row = client
        .query_one(
            r#"SELECT COUNT(*) FROM trial_registry WHERE result @> '{"holdout": true}'::jsonb"#,
            &[],
        )
        .await?;
    Ok(row.try_get(0)?)
}
