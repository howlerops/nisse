//! What the phone UI reads from the store, and the one thing it may do: engage the kill
//! switch. Design: `docs/phases/phase-8.md`. This crate never talks to the broker and holds
//! no broker credential.
//!
//! Every function takes a `tokio_postgres` client (or transaction) by reference. The reads are
//! SELECTs; the only write is the call to `nisse_engage_kill_switch`, which the `nisse_ui`
//! role may execute and nothing else. Everything is as stored: nothing is inferred, and a
//! stored row this build cannot read is an error, so the UI never guesses.

use chrono::{DateTime, Utc};
use nisse_core::nautilus::UnixNanos;
use nisse_core::{PositionEvent, RestoredOrder};
use nisse_risk::{RiskError, RiskState};
use nisse_store::StoreError;
use nisse_store::tables::engine_step::{self, StepStatus};
use nisse_store::tables::{heartbeat, order_event, position_event, risk_event};
use rust_decimal::Decimal;
use serde_json::Value;
use tokio_postgres::GenericClient;

/// Run key and step of the rebalance the engine owes after a resize. Origin:
/// `RESIZE_RUN_KEY` and `RESIZE_STEP` in `crates/app/src/session.rs`. This crate must not
/// depend on `nisse-app`, so the values are copied; a test in `tests/ui_data.rs` checks they
/// still appear in that file.
pub const RESIZE_RUN_KEY: &str = "resize";
/// See [`RESIZE_RUN_KEY`].
pub const RESIZE_STEP: &str = "pending";

/// The longest name the kill switch accepts, in characters after trimming.
const MAX_BY_CHARS: usize = 64;
/// The most rows `recent_steps` returns, whatever the caller asks for.
const MAX_STEPS: u32 = 200;
/// Anything the data layer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Risk(#[from] RiskError),
    #[error(transparent)]
    Db(#[from] tokio_postgres::Error),
    /// The kill switch call returned but the store does not show the switch engaged.
    #[error("the kill switch was not confirmed: {0}")]
    NotConfirmed(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// What the Status screen shows. Everything is as stored; nothing is inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub kill_switch: bool,
    /// "normal", "halved" or "flat": `DrawdownMode::as_str`.
    pub drawdown_mode: String,
    pub equity_peak: Option<Decimal>,
    pub last_heartbeat: Option<UnixNanos>,
    /// Seconds since the newest heartbeat, by the database's clock; `None` with no heartbeat.
    pub heartbeat_age_seconds: Option<u64>,
    pub rebalance_owed: bool,
    pub limits_hash: Option<String>,
}

/// The risk state, the newest heartbeat of `source`, whether a rebalance is owed, and the
/// newest limits hash, as the Status screen shows them.
pub async fn status<C: GenericClient>(client: &C, source: &str) -> Result<Status, UiError> {
    let risk = RiskState::restore(&risk_event::load_all(client).await?)?;
    let last_heartbeat = heartbeat::latest(client, source).await?;
    let heartbeat_age_seconds = heartbeat_age(client, source).await?;
    let rebalance_owed = matches!(
        engine_step::latest(client, RESIZE_RUN_KEY, RESIZE_STEP).await?,
        Some((StepStatus::Started, _))
    );
    Ok(Status {
        kill_switch: risk.kill_switch_engaged,
        drawdown_mode: risk.drawdown.mode.as_str().to_owned(),
        equity_peak: risk.drawdown.peak,
        last_heartbeat,
        heartbeat_age_seconds,
        rebalance_owed,
        limits_hash: risk_event::latest_limits_hash(client).await?,
    })
}

/// A heartbeat this far in the database's future is put down to rounding; further is an error.
const FUTURE_TOLERANCE_SECONDS: f64 = 5.0;

/// How old the newest heartbeat of `source` is, in whole seconds, by the DATABASE's clock:
/// the engine's host stamps heartbeats and the database is beside it, while the caller's own
/// clock (a phone's) may be wrong, and a slow phone clock would make a dead engine look
/// alive. `None` with no heartbeat. A heartbeat more than a few seconds in the database's
/// future is an error, not "just now": some clock is wrong and the age cannot be trusted.
async fn heartbeat_age<C: GenericClient>(client: &C, source: &str) -> Result<Option<u64>, UiError> {
    let row = client
        .query_opt(
            "SELECT extract(epoch FROM (clock_timestamp() - event_ts))::float8 AS age \
             FROM heartbeat WHERE source = $1 ORDER BY event_ts DESC LIMIT 1",
            &[&source],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let age: f64 = row.try_get("age")?;
    if !age.is_finite() || age < -FUTURE_TOLERANCE_SECONDS {
        return Err(UiError::Invalid(format!(
            "the newest heartbeat is {:.0} s in the future by the database's clock; \
             the engine's liveness cannot be judged",
            -age
        )));
    }
    // Finite and at least -5: flooring a non-negative f64 of this size into u64 is exact.
    Ok(Some(age.max(0.0).floor() as u64))
}

/// Newest row per symbol, flat ones left out, sorted by symbol.
pub async fn positions<C: GenericClient>(client: &C) -> Result<Vec<PositionEvent>, UiError> {
    Ok(position_event::latest_per_symbol(client).await?)
}

/// An order that is not in a final state, as the Orders list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOrder {
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    pub state: String,
    pub qty: Decimal,
    pub limit_px: Option<Decimal>,
    pub last_change: UnixNanos,
}

/// Orders whose newest row is not a final state, in client order id order. Reuses
/// `order_event::load_open`, whose intent join supplies the symbol and side.
pub async fn open_orders<C: GenericClient>(client: &C) -> Result<Vec<OpenOrder>, UiError> {
    let orders = order_event::load_open(client).await?;
    Ok(orders.iter().map(open_order).collect())
}

fn open_order(order: &RestoredOrder) -> OpenOrder {
    let intent = &order.intent;
    OpenOrder {
        client_order_id: intent.client_order_id.as_str().to_owned(),
        symbol: intent.symbol.as_str().to_owned(),
        side: intent.side.to_string(),
        state: <&'static str>::from(order.state).to_owned(),
        qty: intent.qty.as_decimal(),
        limit_px: intent.limit_px.map(|px| px.as_decimal()),
        last_change: order.last_update_ts,
    }
}

/// The stored state of one gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateState {
    Pass,
    Fail,
    NotEvaluated,
}

/// One strategy version and the newest stored status of each of its gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionGates {
    pub strategy_version_id: i64,
    pub git_sha: String,
    pub config_hash: String,
    /// (gate name, newest stored state), sorted by name.
    pub gates: Vec<(String, GateState)>,
}

/// Every strategy version, newest first, with the newest stored status of each gate.
///
/// The gate rows are `eval_score` rows with `subject_type = 'strategy_version'` and metric
/// `gate_<name>_status` (written by `nisse_eval::store_report`), newest by id.
pub async fn versions<C: GenericClient>(client: &C) -> Result<Vec<VersionGates>, UiError> {
    let rows = client
        .query(
            r"WITH newest AS (
                  SELECT DISTINCT ON (subject_id, metric) subject_id, metric,
                         value::float8 AS value
                  FROM eval_score
                  WHERE subject_type = 'strategy_version' AND metric LIKE 'gate\_%\_status'
                  ORDER BY subject_id, metric, id DESC
              )
              SELECT v.id, v.git_sha, encode(v.config_hash, 'hex') AS config_hash,
                     n.metric, n.value
              FROM strategy_version v
              LEFT JOIN newest n ON n.subject_id = v.id
              ORDER BY v.id DESC",
            &[],
        )
        .await?;

    let mut out: Vec<VersionGates> = Vec::new();
    for row in rows {
        let id: i64 = row.try_get("id")?;
        if out.last().is_none_or(|v| v.strategy_version_id != id) {
            out.push(VersionGates {
                strategy_version_id: id,
                git_sha: row.try_get("git_sha")?,
                config_hash: row.try_get("config_hash")?,
                gates: Vec::new(),
            });
        }
        let metric: Option<String> = row.try_get("metric")?;
        let value: Option<f64> = row.try_get("value")?;
        if let (Some(metric), Some(value), Some(current)) = (metric, value, out.last_mut())
            && let Some(name) = gate_name(&metric)
        {
            current.gates.push((name.to_owned(), gate_state(value)));
        }
    }
    for version in &mut out {
        version.gates.sort_by(|a, b| a.0.cmp(&b.0));
    }
    Ok(out)
}

/// The gate name in `gate_<name>_status`, or `None` for any other metric.
fn gate_name(metric: &str) -> Option<&str> {
    metric
        .strip_prefix("gate_")
        .and_then(|rest| rest.strip_suffix("_status"))
}

/// 1 is a pass, 0 a fail, anything else (-1 in the store) not evaluated.
fn gate_state(value: f64) -> GateState {
    if value == 1.0 {
        GateState::Pass
    } else if value == 0.0 {
        GateState::Fail
    } else {
        GateState::NotEvaluated
    }
}

/// One engine step status row, as the Steps list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub run_key: String,
    pub step: String,
    pub status: String,
    pub detail: Value,
    pub ts: UnixNanos,
}

/// The newest `limit` rows of `engine_step`, newest first. `limit` is clamped to 1..=200.
pub async fn recent_steps<C: GenericClient>(client: &C, limit: u32) -> Result<Vec<Step>, UiError> {
    let limit = i64::from(limit.clamp(1, MAX_STEPS));
    let rows = client
        .query(
            "SELECT run_key, step, status, detail, event_ts FROM engine_step \
             ORDER BY id DESC LIMIT $1",
            &[&limit],
        )
        .await?;
    let mut steps = Vec::with_capacity(rows.len());
    for row in rows {
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        steps.push(Step {
            run_key: row.try_get("run_key")?,
            step: row.try_get("step")?,
            status: row.try_get("status")?,
            detail: row.try_get("detail")?,
            ts: to_unix_nanos(event_ts)?,
        });
    }
    Ok(steps)
}

/// A stored `TIMESTAMPTZ` as engine time. The store's own conversion is crate-private, so
/// this is the same check: a time outside the nanosecond range is an error.
fn to_unix_nanos(ts: DateTime<Utc>) -> Result<UnixNanos, UiError> {
    ts.timestamp_nanos_opt()
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(UnixNanos::from)
        .ok_or_else(|| UiError::Invalid(format!("timestamp out of range: {ts}")))
}

/// Engages the kill switch through `nisse_engage_kill_switch` and then reads the switch back.
/// `by` is trimmed; empty or longer than 64 characters is `Invalid` before anything is sent.
/// Returns the id of the row written. If the read-back does not show the switch engaged, the
/// error is `NotConfirmed`: the caller must not show "engaged".
pub async fn engage_kill<C: GenericClient>(client: &C, by: &str) -> Result<i64, UiError> {
    let by = by.trim();
    if by.is_empty() {
        return Err(UiError::Invalid("the name must not be blank".to_owned()));
    }
    if by.chars().count() > MAX_BY_CHARS {
        return Err(UiError::Invalid(format!(
            "the name must be at most {MAX_BY_CHARS} characters"
        )));
    }
    let row = client
        .query_one("SELECT nisse_engage_kill_switch($1)", &[&by])
        .await?;
    let id: i64 = row.try_get(0)?;
    if !risk_event::kill_switch_engaged(client).await? {
        return Err(UiError::NotConfirmed(format!(
            "row {id} was written but the store reads the switch as released"
        )));
    }
    Ok(id)
}
