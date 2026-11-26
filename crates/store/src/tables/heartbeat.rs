//! `heartbeat`: a periodic liveness row. The dead-man check reads the newest row per source.

use chrono::{DateTime, Utc};
use nisse_core::nautilus::UnixNanos;
use tokio_postgres::GenericClient;

use crate::{
    StoreError,
    db::{from_ts, to_ts},
};

/// Records that `source` was alive at `ts`. An empty source is refused by the table's CHECK
/// and comes back as a `Db` error.
pub async fn insert<C: GenericClient>(
    client: &C,
    source: &str,
    ts: UnixNanos,
) -> Result<(), StoreError> {
    let event_ts = to_ts(ts)?;
    client
        .execute(
            "INSERT INTO heartbeat (source, event_ts) VALUES ($1, $2)",
            &[&source, &event_ts],
        )
        .await?;
    Ok(())
}

/// The newest heartbeat time of `source` (by `event_ts`); `None` when it has none. Served by
/// the `(source, event_ts DESC)` index.
pub async fn latest<C: GenericClient>(
    client: &C,
    source: &str,
) -> Result<Option<UnixNanos>, StoreError> {
    let row = client
        .query_opt(
            "SELECT event_ts FROM heartbeat WHERE source = $1 ORDER BY event_ts DESC LIMIT 1",
            &[&source],
        )
        .await?;
    match row {
        Some(row) => {
            let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
            Ok(Some(from_ts(event_ts)?))
        }
        None => Ok(None),
    }
}
