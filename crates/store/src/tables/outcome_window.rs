//! `outcome_window`: the forward return, benchmark return and excursions after a decision
//! over one fixed horizon.

use chrono::{DateTime, Utc};
use nisse_core::{
    DecisionId,
    nautilus::{Symbol, UnixNanos},
};
use tokio_postgres::{GenericClient, Row};

use crate::{
    StoreError,
    db::{from_ts, to_ts},
};

/// One row to insert. `horizon_sessions` is stored in the `horizon` column as that many days.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutcomeRow {
    pub decision_id: DecisionId,
    pub horizon_sessions: u32,
    pub fwd_ret: f64,
    pub bench_ret: f64,
    pub mfe: f64,
    pub mae: f64,
    pub ts: UnixNanos,
}

/// A decision that still needs an outcome row for one horizon.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDecision {
    pub id: DecisionId,
    pub symbol: Symbol,
    pub ts: UnixNanos,
    pub expected_vol: Option<f64>,
}

/// A horizon in sessions as the `i32` that `make_interval(days => ...)` takes.
fn horizon_days(sessions: u32) -> Result<i32, StoreError> {
    i32::try_from(sessions)
        .map_err(|_| StoreError::Data(format!("horizon out of range: {sessions} sessions")))
}

/// Inserts all rows with one statement, in slice order. The horizon interval is built in SQL
/// from the session counts (`make_interval(days => n)`). Non-finite values are refused by the
/// `finite_double` domain, so the whole batch is an error and nothing is inserted. An empty
/// slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[OutcomeRow],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut decision_ids: Vec<i64> = Vec::with_capacity(n);
    let mut day_counts: Vec<i32> = Vec::with_capacity(n);
    let mut fwd_rets: Vec<f64> = Vec::with_capacity(n);
    let mut bench_rets: Vec<f64> = Vec::with_capacity(n);
    let mut mfes: Vec<f64> = Vec::with_capacity(n);
    let mut maes: Vec<f64> = Vec::with_capacity(n);
    let mut event_ts: Vec<DateTime<Utc>> = Vec::with_capacity(n);
    for row in rows {
        decision_ids.push(row.decision_id.0);
        day_counts.push(horizon_days(row.horizon_sessions)?);
        fwd_rets.push(row.fwd_ret);
        bench_rets.push(row.bench_ret);
        mfes.push(row.mfe);
        maes.push(row.mae);
        event_ts.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, \
                 event_ts) \
             SELECT t.decision_id, make_interval(days => t.day_count), t.fwd_ret, t.bench_ret, \
                 t.mfe, t.mae, t.event_ts \
             FROM UNNEST($1::bigint[], $2::int4[], $3::float8[], $4::float8[], $5::float8[], \
                         $6::float8[], $7::timestamptz[]) \
                 WITH ORDINALITY AS t(decision_id, day_count, fwd_ret, bench_ret, mfe, mae, \
                                      event_ts, ord) \
             ORDER BY t.ord",
            &[
                &decision_ids,
                &day_counts,
                &fwd_rets,
                &bench_rets,
                &mfes,
                &maes,
                &event_ts,
            ],
        )
        .await?;
    Ok(())
}

/// Decisions with no `outcome_window` row for `horizon_sessions`, in id order, at most `limit`.
pub async fn pending<C: GenericClient>(
    client: &C,
    horizon_sessions: u32,
    limit: i64,
) -> Result<Vec<PendingDecision>, StoreError> {
    let days = horizon_days(horizon_sessions)?;
    let rows = client
        .query(
            "SELECT d.id, d.symbol, d.event_ts, d.expected_vol::float8 \
             FROM decision d \
             WHERE NOT EXISTS ( \
                 SELECT 1 FROM outcome_window o \
                 WHERE o.decision_id = d.id AND o.horizon = make_interval(days => $1::int4)) \
             ORDER BY d.id LIMIT $2",
            &[&days, &limit],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(pending_from_row(row)?);
    }
    Ok(out)
}

/// Maps a `pending` row: id, symbol, event_ts, expected_vol.
fn pending_from_row(row: &Row) -> Result<PendingDecision, StoreError> {
    let id: i64 = row.try_get(0)?;
    let symbol: String = row.try_get(1)?;
    let event_ts: DateTime<Utc> = row.try_get(2)?;
    let expected_vol: Option<f64> = row.try_get(3)?;
    let symbol = Symbol::new_checked(&symbol)
        .map_err(|e| StoreError::Data(format!("bad symbol {symbol}: {e}")))?;
    Ok(PendingDecision {
        id: DecisionId(id),
        symbol,
        ts: from_ts(event_ts)?,
        expected_vol,
    })
}

/// The rows stored for a decision, in horizon order (for tests and reports).
pub async fn for_decision<C: GenericClient>(
    client: &C,
    decision_id: DecisionId,
) -> Result<Vec<OutcomeRow>, StoreError> {
    let rows = client
        .query(
            "SELECT EXTRACT(DAY FROM horizon)::int4, fwd_ret::float8, bench_ret::float8, \
                 mfe::float8, mae::float8, event_ts \
             FROM outcome_window WHERE decision_id = $1 ORDER BY horizon, id",
            &[&decision_id.0],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let days: i32 = row.try_get(0)?;
        let horizon_sessions = u32::try_from(days)
            .map_err(|_| StoreError::Data(format!("bad stored horizon: {days} days")))?;
        let event_ts: DateTime<Utc> = row.try_get(5)?;
        out.push(OutcomeRow {
            decision_id,
            horizon_sessions,
            fwd_ret: row.try_get(1)?,
            bench_ret: row.try_get(2)?,
            mfe: row.try_get(3)?,
            mae: row.try_get(4)?,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(out)
}
