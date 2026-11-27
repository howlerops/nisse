//! `eval_score`: one metric value for a decision or strategy version.

use nisse_core::{EvalScore, SubjectType};
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Inserts all rows with one statement (arrays through `UNNEST`), so a batch costs one round
/// trip however many rows it holds. An empty slice sends nothing.
pub async fn insert_many<C: GenericClient>(
    client: &C,
    rows: &[EvalScore],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let mut subject_ids: Vec<i64> = Vec::with_capacity(n);
    let mut subject_types: Vec<&str> = Vec::with_capacity(n);
    let mut metrics: Vec<&str> = Vec::with_capacity(n);
    let mut values: Vec<f64> = Vec::with_capacity(n);
    let mut eval_versions: Vec<&str> = Vec::with_capacity(n);
    let mut event_ts = Vec::with_capacity(n);
    for row in rows {
        subject_ids.push(row.subject_id);
        subject_types.push(<&'static str>::from(row.subject_type));
        metrics.push(row.metric.as_str());
        values.push(row.value);
        eval_versions.push(row.eval_version.as_str());
        event_ts.push(to_ts(row.ts)?);
    }
    client
        .execute(
            "INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts) \
             SELECT * FROM UNNEST($1::bigint[], $2::text[], $3::text[], $4::float8[], \
                                  $5::text[], $6::timestamptz[])",
            &[
                &subject_ids,
                &subject_types,
                &metrics,
                &values,
                &eval_versions,
                &event_ts,
            ],
        )
        .await?;
    Ok(())
}

/// The subject ids of `subject_type` that already have a score for `metric` computed by
/// `eval_version`, in ascending order, deduplicated (so a job can skip them).
pub async fn scored_subjects<C: GenericClient>(
    client: &C,
    subject_type: SubjectType,
    metric: &str,
    eval_version: &str,
) -> Result<Vec<i64>, StoreError> {
    let subject_type: &'static str = subject_type.into();
    let rows = client
        .query(
            "SELECT DISTINCT subject_id FROM eval_score \
             WHERE subject_type = $1 AND metric = $2 AND eval_version = $3 \
             ORDER BY subject_id",
            &[&subject_type, &metric, &eval_version],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get(0)?;
        ids.push(id);
    }
    Ok(ids)
}
