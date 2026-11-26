//! `fill`: one row per fill, unique on the broker execution id.

use nisse_core::Fill;
use tokio_postgres::GenericClient;

use crate::StoreError;
use crate::db::to_ts;

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
