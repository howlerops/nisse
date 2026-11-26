//! `risk_event`: one change of the risk state (equity peak, drawdown mode, kill switch, limits
//! loaded). The risk state after a restart is the fold of these rows in id order.

use chrono::{DateTime, Utc};
use nisse_core::{RiskEvent, RiskEventKind};
use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::{
    StoreError,
    db::{from_ts, parse_enum, to_ts},
};

/// Inserts all rows with one statement, in slice order: the ids ascend in that order (the
/// `ORDER BY ord` makes the order explicit). Arrays go through `UNNEST`, so a batch costs one
/// round trip however many rows it holds. An empty slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[RiskEvent],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut kinds: Vec<&'static str> = Vec::with_capacity(n);
    let mut details: Vec<&Value> = Vec::with_capacity(n);
    let mut event_ts: Vec<DateTime<Utc>> = Vec::with_capacity(n);
    for row in rows {
        kinds.push(<&'static str>::from(row.kind));
        details.push(&row.detail);
        event_ts.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO risk_event (kind, detail, event_ts) \
             SELECT kind, detail, event_ts \
             FROM UNNEST($1::text[], $2::jsonb[], $3::timestamptz[]) \
                 WITH ORDINALITY AS t(kind, detail, event_ts, ord) \
             ORDER BY ord",
            &[&kinds, &details, &event_ts],
        )
        .await?;
    Ok(())
}

/// Every risk event, in id order: the risk state after a restart is the fold of these.
/// A stored kind this build does not know is a `Data` error, not a skipped row.
pub async fn load_all<C: GenericClient>(client: &C) -> Result<Vec<RiskEvent>, StoreError> {
    let rows = client
        .query(
            "SELECT kind, detail, event_ts FROM risk_event ORDER BY id",
            &[],
        )
        .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let kind: String = row.try_get("kind")?;
        let detail: Value = row.try_get("detail")?;
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        events.push(RiskEvent {
            kind: parse_enum::<RiskEventKind>(&kind, "kind")?,
            detail,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(events)
}

/// Whether the kill switch is engaged: the `engaged` of the newest `kill_switch` row (by id).
/// No such row is `Ok(false)`. A newest row whose `engaged` is missing or not a boolean is a
/// `Data` error, so the caller fails closed.
pub async fn kill_switch_engaged<C: GenericClient>(client: &C) -> Result<bool, StoreError> {
    let kind = <&'static str>::from(RiskEventKind::KillSwitch);
    let row = client
        .query_opt(
            "SELECT detail FROM risk_event WHERE kind = $1 ORDER BY id DESC LIMIT 1",
            &[&kind],
        )
        .await?;
    match row {
        None => Ok(false),
        Some(row) => {
            let detail: Value = row.try_get("detail")?;
            detail
                .get("engaged")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    StoreError::Data(
                        "kill_switch risk_event has no boolean \"engaged\" in its detail"
                            .to_string(),
                    )
                })
        }
    }
}
