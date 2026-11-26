//! `decision`: one row per strategy decision per symbol, including holds and skips.

use chrono::{DateTime, Utc};
use nisse_core::{
    Action, Decision, DecisionId, StrategyVersion, StrategyVersionId, features_from_json,
    features_to_json, nautilus::Symbol,
};
use rust_decimal::Decimal;
use tokio_postgres::{GenericClient, Row};

use crate::{
    StoreError,
    db::{from_ts, parse_enum, to_ts},
    tables::strategy_version::{hash_from_column, version_from_row},
};

/// A decision as stored, with the strategy version it was made by.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDecision {
    pub id: DecisionId,
    pub decision: Decision,
    pub version: StrategyVersion,
}

/// Inserts one decision and returns its id. `features` is stored as the JSON produced by
/// `nisse_core::features_to_json`; `rng_seed` is stored as the `i64` with the same bits.
pub async fn insert<C: GenericClient>(
    client: &C,
    decision: &Decision,
) -> Result<DecisionId, StoreError> {
    let features = features_to_json(&decision.features);
    let feature_hash: &[u8] = decision.feature_hash.as_bytes();
    let rng_seed = i64::from_ne_bytes(decision.rng_seed.to_ne_bytes());
    let action: &'static str = decision.action.into();
    let symbol = decision.symbol.as_str();
    let event_ts = to_ts(decision.ts)?;

    let row = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, \
                 expected_edge_bps, expected_vol, features, feature_hash, data_snapshot_id, \
                 rng_seed, reason_code, event_ts) \
             VALUES ($1, $2, $3, $4::numeric, $5::float8, $6::float8, $7, $8, $9, $10, $11, $12) \
             RETURNING id",
            &[
                &decision.strategy_version_id.0,
                &symbol,
                &action,
                &decision.intended_qty,
                &decision.expected_edge_bps,
                &decision.expected_vol,
                &features,
                &feature_hash,
                &decision.data_snapshot_id,
                &rng_seed,
                &decision.reason_code,
                &event_ts,
            ],
        )
        .await?;
    Ok(DecisionId(row.try_get(0)?))
}

/// Decisions with `id > after_id`, oldest first, at most `limit`, joined to their strategy
/// version. The replay job pages through the table with this.
pub async fn list_after<C: GenericClient>(
    client: &C,
    after_id: DecisionId,
    limit: i64,
) -> Result<Vec<StoredDecision>, StoreError> {
    let rows = client
        .query(
            "SELECT d.id, d.symbol, d.action, d.intended_qty::numeric, \
                 d.expected_edge_bps::float8, d.expected_vol::float8, d.features, \
                 d.feature_hash, d.data_snapshot_id, d.rng_seed, d.reason_code, d.event_ts, \
                 d.strategy_version_id, \
                 v.git_sha, v.config_hash, v.params, v.parent_id, v.event_ts \
             FROM decision d JOIN strategy_version v ON v.id = d.strategy_version_id \
             WHERE d.id > $1 ORDER BY d.id LIMIT $2",
            &[&after_id.0, &limit],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(stored_from_row(row)?);
    }
    Ok(out)
}

/// Maps a `list_after` row: decision columns 0..=12, then the version's five columns at 13.
fn stored_from_row(row: &Row) -> Result<StoredDecision, StoreError> {
    const VERSION_OFFSET: usize = 13;

    let id: i64 = row.try_get(0)?;
    let symbol: String = row.try_get(1)?;
    let action: String = row.try_get(2)?;
    let intended_qty: Decimal = row.try_get(3)?;
    let expected_edge_bps: Option<f64> = row.try_get(4)?;
    let expected_vol: Option<f64> = row.try_get(5)?;
    let features: serde_json::Value = row.try_get(6)?;
    let feature_hash: Vec<u8> = row.try_get(7)?;
    let data_snapshot_id: String = row.try_get(8)?;
    let rng_seed: i64 = row.try_get(9)?;
    let reason_code: String = row.try_get(10)?;
    let event_ts: DateTime<Utc> = row.try_get(11)?;
    let strategy_version_id: i64 = row.try_get(12)?;
    let version = version_from_row(row, VERSION_OFFSET)?;

    let symbol = Symbol::new_checked(&symbol)
        .map_err(|e| StoreError::Data(format!("bad symbol {symbol}: {e}")))?;
    let decision = Decision {
        strategy_version_id: StrategyVersionId(strategy_version_id),
        symbol,
        action: parse_enum::<Action>(&action, "action")?,
        intended_qty,
        expected_edge_bps,
        expected_vol,
        features: features_from_json(&features)?,
        feature_hash: hash_from_column(&feature_hash, "feature_hash")?,
        data_snapshot_id,
        rng_seed: u64::from_ne_bytes(rng_seed.to_ne_bytes()),
        reason_code,
        ts: from_ts(event_ts)?,
    };
    Ok(StoredDecision {
        id: DecisionId(id),
        decision,
        version,
    })
}
