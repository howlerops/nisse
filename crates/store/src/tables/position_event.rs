//! `position_event`: the position in a symbol after a change.

use nisse_core::PositionEvent;
use rust_decimal::Decimal;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

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
