//! `position_event`: the position in a symbol after a change.

use chrono::{DateTime, Utc};
use nisse_core::PositionEvent;
use nisse_core::nautilus::Symbol;
use rust_decimal::Decimal;
use tokio_postgres::GenericClient;

use crate::{
    StoreError,
    db::{from_ts, to_ts},
};

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[PositionEvent],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut symbols: Vec<&str> = Vec::with_capacity(n);
    let mut qtys: Vec<Decimal> = Vec::with_capacity(n);
    let mut costs: Vec<Decimal> = Vec::with_capacity(n);
    let mut stamps = Vec::with_capacity(n);
    for row in rows {
        symbols.push(row.symbol.as_str());
        qtys.push(row.qty_after);
        costs.push(row.avg_cost);
        stamps.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts) \
             SELECT * FROM UNNEST($1::text[], $2::numeric[], $3::numeric[], $4::timestamptz[])",
            &[&symbols, &qtys, &costs, &stamps],
        )
        .await?;
    Ok(())
}

/// The newest row per symbol (by id): the positions a restarting engine starts from. A symbol
/// whose newest row is flat (`qty_after = 0`) is left out, even when an older row was not:
/// the zero is filtered here, not in SQL, because a WHERE would resurrect the older row.
/// Sorted by symbol.
///
/// One statement; `DISTINCT ON` picks the newest row per symbol and yields symbol order.
pub async fn latest_per_symbol<C: GenericClient>(
    client: &C,
) -> Result<Vec<PositionEvent>, StoreError> {
    let rows = client
        .query(
            "SELECT DISTINCT ON (symbol) symbol, qty_after::numeric AS qty_after, \
                    avg_cost::numeric AS avg_cost, event_ts \
             FROM position_event ORDER BY symbol, id DESC",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let qty_after: Decimal = row.try_get("qty_after")?;
        if qty_after.is_zero() {
            continue;
        }
        let symbol: String = row.try_get("symbol")?;
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        out.push(PositionEvent {
            symbol: Symbol::new_checked(&symbol)
                .map_err(|e| StoreError::Data(format!("bad stored symbol: {e}")))?,
            qty_after,
            avg_cost: row.try_get("avg_cost")?,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(out)
}
