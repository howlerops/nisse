//! `market_snapshot`: the quote and market state seen at a point in time.

use nisse_core::MarketSnapshot;
use rust_decimal::Decimal;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[MarketSnapshot],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut symbols: Vec<&str> = Vec::with_capacity(n);
    let mut bids: Vec<Option<Decimal>> = Vec::with_capacity(n);
    let mut asks: Vec<Option<Decimal>> = Vec::with_capacity(n);
    let mut mids: Vec<Option<Decimal>> = Vec::with_capacity(n);
    let mut spreads: Vec<Option<f64>> = Vec::with_capacity(n);
    let mut advs: Vec<Option<Decimal>> = Vec::with_capacity(n);
    let mut vols: Vec<Option<f64>> = Vec::with_capacity(n);
    let mut regimes: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut stamps = Vec::with_capacity(n);
    for row in rows {
        symbols.push(row.symbol.as_str());
        bids.push(row.bid);
        asks.push(row.ask);
        mids.push(row.mid);
        spreads.push(row.spread_bps);
        advs.push(row.adv);
        vols.push(row.vol);
        regimes.push(row.regime_tag.as_deref());
        stamps.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO market_snapshot (symbol, bid, ask, mid, spread_bps, adv, vol, regime_tag, event_ts) \
             SELECT * FROM UNNEST($1::text[], $2::numeric[], $3::numeric[], $4::numeric[], \
             $5::float8[], $6::numeric[], $7::float8[], $8::text[], $9::timestamptz[])",
            &[
                &symbols, &bids, &asks, &mids, &spreads, &advs, &vols, &regimes, &stamps,
            ],
        )
        .await?;
    Ok(())
}
