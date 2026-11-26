//! `decision_holdout`: whether a decision is hidden from agents. Assigned once per decision.

use nisse_core::{DecisionId, nautilus::UnixNanos};
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// One assignment: the decision and whether it is hidden from agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    pub decision_id: DecisionId,
    pub hidden: bool,
}

/// Inserts the assignments with one statement, all stamped `ts`. Empty slice sends nothing.
/// A decision that already has a row is a unique violation (`StoreError::Db`): an assignment is
/// never changed, and because the statement is one unit, none of the batch is inserted then.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[Assignment],
    ts: UnixNanos,
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut decision_ids: Vec<i64> = Vec::with_capacity(n);
    let mut hidden: Vec<bool> = Vec::with_capacity(n);
    for row in rows {
        decision_ids.push(row.decision_id.0);
        hidden.push(row.hidden);
    }
    let event_ts = to_ts(ts)?;
    client
        .execute(
            "INSERT INTO decision_holdout (decision_id, hidden, event_ts) \
             SELECT t.decision_id, t.hidden, $3::timestamptz \
             FROM UNNEST($1::bigint[], $2::bool[]) WITH ORDINALITY AS t(decision_id, hidden, ord) \
             ORDER BY t.ord",
            &[&decision_ids, &hidden, &event_ts],
        )
        .await?;
    Ok(())
}

/// The ids of decisions that have no `decision_holdout` row, in id order, at most `limit`.
pub async fn unassigned<C: GenericClient>(
    client: &C,
    limit: i64,
) -> Result<Vec<DecisionId>, StoreError> {
    let rows = client
        .query(
            "SELECT d.id FROM decision d \
             WHERE NOT EXISTS (SELECT 1 FROM decision_holdout h WHERE h.decision_id = d.id) \
             ORDER BY d.id LIMIT $1",
            &[&limit],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get(0)?;
        ids.push(DecisionId(id));
    }
    Ok(ids)
}

/// How many decisions are hidden and how many visible: `(hidden, visible)`.
pub async fn counts<C: GenericClient>(client: &C) -> Result<(i64, i64), StoreError> {
    let row = client
        .query_one(
            "SELECT count(*) FILTER (WHERE hidden), count(*) FILTER (WHERE NOT hidden) \
             FROM decision_holdout",
            &[],
        )
        .await?;
    let hidden: i64 = row.try_get(0)?;
    let visible: i64 = row.try_get(1)?;
    Ok((hidden, visible))
}
