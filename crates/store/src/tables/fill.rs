//! `fill`: one row per fill, unique on the broker execution id.

use chrono::{DateTime, Utc};
use nisse_core::Fill;
use nisse_core::nautilus::{OrderSide, Symbol, UnixNanos};
use tokio_postgres::GenericClient;

use crate::StoreError;
use crate::db::{from_ts, parse_enum, to_ts};

/// One stored fill with the symbol and side of its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillSide {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub ts: UnixNanos,
}

/// Inserts one fill with `fee` 0 (Alpaca reports fees separately; they are added by the
/// evaluation jobs). Returns `false`, changing nothing, when a row with the same
/// `broker_fill_id` already exists: a fill delivered twice is recorded once.
pub async fn insert<C: GenericClient>(client: &C, fill: &Fill) -> Result<bool, StoreError> {
    let event_ts = to_ts(fill.ts)?;
    let written = client
        .execute(
            "INSERT INTO fill (client_order_id, broker_fill_id, px, qty, fee, event_ts) \
             VALUES ($1, $2, $3::numeric, $4::numeric, 0, $5) \
             ON CONFLICT (broker_fill_id) DO NOTHING",
            &[
                &fill.client_order_id.as_str(),
                &fill.trade_id.as_str(),
                &fill.px.as_decimal(),
                &fill.qty.as_decimal(),
                &event_ts,
            ],
        )
        .await?;
    Ok(written == 1)
}

/// Every fill with `event_ts >= since`, in `fill.id` order, with the symbol and side of its
/// order: taken from the order's first `order_event` row (lowest id) for that
/// `client_order_id`. A fill whose order has no `order_event` row is a [`StoreError::Data`]:
/// the caller's day-trade count would be wrong without it, so it is never skipped.
///
/// One statement. The lateral lookup uses `order_event_client_order_id_event_ts_idx`.
pub async fn recent_sides<C: GenericClient>(
    client: &C,
    since: UnixNanos,
) -> Result<Vec<FillSide>, StoreError> {
    let since = to_ts(since)?;
    let rows = client
        .query(
            "SELECT f.id, o.symbol, o.side, f.event_ts \
             FROM fill f \
             LEFT JOIN LATERAL ( \
                 SELECT symbol, side FROM order_event \
                 WHERE client_order_id = f.client_order_id ORDER BY id LIMIT 1 \
             ) o ON true \
             WHERE f.event_ts >= $1 \
             ORDER BY f.id",
            &[&since],
        )
        .await?;

    let mut fills = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let symbol: Option<String> = row.try_get("symbol")?;
        let side: Option<String> = row.try_get("side")?;
        let (Some(symbol), Some(side)) = (symbol, side) else {
            return Err(StoreError::Data(format!(
                "fill {id} has no order_event row for its client order id"
            )));
        };
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        fills.push(FillSide {
            symbol: Symbol::new_checked(&symbol)
                .map_err(|e| StoreError::Data(format!("bad stored symbol: {e}")))?,
            side: parse_enum::<OrderSide>(&side, "side")?,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(fills)
}
