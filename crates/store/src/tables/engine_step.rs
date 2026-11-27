//! `engine_step`: one row per status change of one step of one scheduled engine run. The newest
//! row per (run key, step) is the step's status; a re-run of the same run key skips the steps
//! already `done`.

use nisse_core::nautilus::UnixNanos;
use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// The status of one step of one engine run. The text forms are the table's CHECK values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Started,
    Done,
    Failed,
}

impl StepStatus {
    /// The text stored in the `status` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Parses a stored status. An unknown value is a `Data` error.
    fn parse(text: &str) -> Result<Self, StoreError> {
        match text {
            "started" => Ok(Self::Started),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Data(format!("unknown status value: {other}"))),
        }
    }
}

/// Appends one status row. `detail` must be a JSON object (the table refuses anything else),
/// which comes back as a `Db` error with nothing stored.
pub async fn insert<C: GenericClient>(
    client: &C,
    run_key: &str,
    step: &str,
    status: StepStatus,
    detail: &Value,
    ts: UnixNanos,
) -> Result<(), StoreError> {
    let event_ts = to_ts(ts)?;
    let status = status.as_str();
    client
        .execute(
            "INSERT INTO engine_step (run_key, step, status, detail, event_ts) \
             VALUES ($1, $2, $3, $4, $5)",
            &[&run_key, &step, &status, detail, &event_ts],
        )
        .await?;
    Ok(())
}

/// The newest status of `step` in `run_key` (by id) and its detail; `None` when the step has
/// no row. A stored status this build does not know is a `Data` error. Served by the
/// `(run_key, step, id DESC)` index.
pub async fn latest<C: GenericClient>(
    client: &C,
    run_key: &str,
    step: &str,
) -> Result<Option<(StepStatus, Value)>, StoreError> {
    let row = client
        .query_opt(
            "SELECT status, detail FROM engine_step \
             WHERE run_key = $1 AND step = $2 ORDER BY id DESC LIMIT 1",
            &[&run_key, &step],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let status: String = row.try_get("status")?;
            let detail: Value = row.try_get("detail")?;
            Ok(Some((StepStatus::parse(&status)?, detail)))
        }
    }
}
