//! `risk_check`: one risk rule evaluated for a decision.

use nisse_core::RiskCheck;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[RiskCheck],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut decision_ids: Vec<i64> = Vec::with_capacity(n);
    let mut rules: Vec<&str> = Vec::with_capacity(n);
    let mut limit_values = Vec::with_capacity(n);
    let mut actual_values = Vec::with_capacity(n);
    let mut passed: Vec<bool> = Vec::with_capacity(n);
    let mut event_ts = Vec::with_capacity(n);
    for row in rows {
        decision_ids.push(row.decision_id.0);
        rules.push(row.rule.as_str());
        limit_values.push(row.limit_value);
        actual_values.push(row.actual_value);
        passed.push(row.passed);
        event_ts.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts) \
             SELECT * FROM UNNEST($1::bigint[], $2::text[], $3::numeric[], $4::numeric[], \
                                  $5::bool[], $6::timestamptz[])",
            &[
                &decision_ids,
                &rules,
                &limit_values,
                &actual_values,
                &passed,
                &event_ts,
            ],
        )
        .await?;
    Ok(())
}
