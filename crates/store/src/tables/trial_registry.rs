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

/// One engine run as registered: when it started and what it trades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRun {
    pub start: UnixNanos,
    /// The symbols the engine trades, sorted. Empty for a trial registered without them.
    pub symbols: Vec<String>,
}

/// Registers the trial of an engine run: kind `live`, a `data_range` from `start` with no end
/// (the run is still going), and the result
/// `{"environment": <environment>, "symbols": [<symbols>]}`. `symbols` is stored as given: pass
/// it sorted. Returns the trial's id.
pub async fn insert_live<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: &str,
    symbols: &[String],
    start: UnixNanos,
    ts: UnixNanos,
) -> Result<i64, StoreError> {
    let data_start = to_ts(start)?;
    let event_ts = to_ts(ts)?;

    let row = client
        .query_one(
            "INSERT INTO trial_registry (strategy_version_id, kind, data_range, result, event_ts) \
             VALUES ($1, 'live', tstzrange($2, NULL, '[)'), \
                     jsonb_build_object('environment', $3::text, 'symbols', to_jsonb($4::text[])), \
                     $5) \
             RETURNING id",
            &[&version.0, &data_start, &environment, &symbols, &event_ts],
        )
        .await?;
    Ok(row.try_get(0)?)
}

/// The engine's current run of `version`'s strategy config: the newest `live` trial of any
/// strategy version with the same `config_hash`. With `environment` given, only trials
/// registered for that environment count. `None` when there is none. One statement.
///
/// The engine registers a new `live` trial when the symbols it trades change, so the newest one
/// says what is running now and since when.
pub async fn latest_live<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: Option<&str>,
) -> Result<Option<LiveRun>, StoreError> {
    let row = client
        .query_opt(
            "SELECT lower(t.data_range) AS start, \
                    COALESCE(ARRAY(SELECT jsonb_array_elements_text(t.result -> 'symbols')), \
                             ARRAY[]::text[]) AS symbols \
             FROM trial_registry t \
             JOIN strategy_version v ON v.id = t.strategy_version_id \
             WHERE t.kind = 'live' \
               AND v.config_hash = (SELECT config_hash FROM strategy_version WHERE id = $1) \
               AND ($2::text IS NULL OR t.result ->> 'environment' = $2) \
             ORDER BY t.id DESC LIMIT 1",
            &[&version.0, &environment],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let start: Option<DateTime<Utc>> = row.try_get("start")?;
    let Some(start) = start else {
        return Err(StoreError::Data(
            "a live trial has no start to its data range".to_owned(),
        ));
    };
    Ok(Some(LiveRun {
        start: from_ts(start)?,
        symbols: row.try_get("symbols")?,
    }))
}

/// The number of trials that can select a strategy: every `backtest` and `shadow` trial except
/// tracking runs (registered with `"tracking": true`). This is the count the trial budget and
/// the deflated Sharpe use. A `live` trial and a tracking run are registered and kept, but they
/// judge a config that is already running and choose nothing (owner's decision, 2026-11-28).
pub async fn selection_count<C: GenericClient>(client: &C) -> Result<i64, StoreError> {
    let row = client
        .query_one(
            r#"SELECT COUNT(*) FROM trial_registry
               WHERE kind IN ('backtest', 'shadow')
                 AND NOT COALESCE(result @> '{"tracking": true}'::jsonb, false)"#,
            &[],
        )
        .await?;
    Ok(row.try_get(0)?)
}

/// The lifetime trial count: every row, of every kind and every strategy version. The gates
/// count with [`selection_count`]; this is the size of the whole record.
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
