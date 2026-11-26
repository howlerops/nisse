//! `strategy_version`: one immutable strategy configuration at one git SHA.

use chrono::{DateTime, Utc};
use nisse_core::{Hash32, StrategyVersion, StrategyVersionId};
use tokio_postgres::{GenericClient, Row};

use crate::{
    StoreError,
    db::{from_ts, to_ts},
};

/// Returns the id of the row for `(git_sha, config_hash)`, inserting it if it does not exist.
/// Safe to call concurrently and repeatedly: the unique constraint decides, and an existing
/// row is never changed (its `params`, `parent_id` and `event_ts` stay as first written).
pub async fn ensure<C: GenericClient>(
    client: &C,
    version: &StrategyVersion,
) -> Result<StrategyVersionId, StoreError> {
    let config_hash: &[u8] = version.config_hash.as_bytes();
    let parent_id = version.parent_id.map(|id| id.0);
    let event_ts = to_ts(version.ts)?;

    let inserted = client
        .query_opt(
            "INSERT INTO strategy_version (git_sha, config_hash, params, parent_id, event_ts) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (git_sha, config_hash) DO NOTHING \
             RETURNING id",
            &[
                &version.git_sha,
                &config_hash,
                &version.params,
                &parent_id,
                &event_ts,
            ],
        )
        .await?;
    if let Some(row) = inserted {
        return Ok(StrategyVersionId(row.try_get(0)?));
    }

    let existing = client
        .query_one(
            "SELECT id FROM strategy_version WHERE git_sha = $1 AND config_hash = $2",
            &[&version.git_sha, &config_hash],
        )
        .await?;
    Ok(StrategyVersionId(existing.try_get(0)?))
}

/// Reads one version. `Ok(None)` when the id does not exist.
pub async fn get<C: GenericClient>(
    client: &C,
    id: StrategyVersionId,
) -> Result<Option<StrategyVersion>, StoreError> {
    let row = client
        .query_opt(
            "SELECT git_sha, config_hash, params, parent_id, event_ts \
             FROM strategy_version WHERE id = $1",
            &[&id.0],
        )
        .await?;
    row.as_ref().map(|row| version_from_row(row, 0)).transpose()
}

/// Reads a [`StrategyVersion`] from five consecutive columns of `row`, starting at `offset`, in
/// the order `git_sha, config_hash, params, parent_id, event_ts`. Shared by [`get`] and the
/// joined query in `decision::list_after`, so the mapping exists once.
pub(crate) fn version_from_row(row: &Row, offset: usize) -> Result<StrategyVersion, StoreError> {
    let git_sha: String = row.try_get(offset)?;
    let config_hash: Vec<u8> = row.try_get(offset + 1)?;
    let params: serde_json::Value = row.try_get(offset + 2)?;
    let parent_id: Option<i64> = row.try_get(offset + 3)?;
    let event_ts: DateTime<Utc> = row.try_get(offset + 4)?;
    Ok(StrategyVersion {
        git_sha,
        config_hash: hash_from_column(&config_hash, "config_hash")?,
        params,
        parent_id: parent_id.map(StrategyVersionId),
        ts: from_ts(event_ts)?,
    })
}

/// A stored 32-byte `BYTEA` hash to a [`Hash32`]; any other length is `StoreError::Data`.
pub(crate) fn hash_from_column(bytes: &[u8], column: &'static str) -> Result<Hash32, StoreError> {
    <[u8; 32]>::try_from(bytes)
        .map(Hash32::from_bytes)
        .map_err(|_| StoreError::Data(format!("{column} is {} bytes, expected 32", bytes.len())))
}
