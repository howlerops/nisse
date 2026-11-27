//! What `App.tsx` imports from `nisse-native`: the store's state for the screens, and the one
//! action the app has, engaging the kill switch. Everything of substance is in
//! `nisse-ui-data`; this file opens a connection, calls it and turns the results into plain
//! objects of strings and numbers.
//!
//! The app never talks to the broker and holds no broker credential. The database URL is
//! passed in by the app on every call (it is kept in the app's storage); it is never logged
//! and never put in an error message.

use std::time::Duration;

use nisse_ui_data as data;
use nsh_core::prelude::*;

/// How long one call may take, connection included, before it is reported as unreachable.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The heartbeat source the engine writes: `"source"` in `config/engine/paper.json`. If that
/// is ever changed, change this with it, or the Status screen will say "never".
const HEARTBEAT_SOURCE: &str = "engine";
/// How many engine steps the Steps list asks for.
const STEP_LIMIT: u32 = 50;

/// What the app imports from `nisse-native`.
#[native_module("nisse-native")]
fn native(registry: &mut Registry) {
    registry.module::<Nisse>();
}

/// The engine's state as stored, for the Status screen.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct StatusView {
    /// The kill switch is engaged.
    pub kill_switch: bool,
    /// "normal", "halved" or "flat".
    pub drawdown_mode: String,
    /// The equity high-water mark as decimal text, or null before the first observation.
    pub equity_peak: Option<String>,
    /// Seconds since the engine's newest heartbeat by the database's clock (not this
    /// device's), or null when it has never written one.
    pub heartbeat_age_seconds: Option<f64>,
    /// Seconds since the kill watcher on the host last ran, by the database's clock, or null
    /// when it never has. The watcher is what cancels open orders after a kill from here.
    pub watcher_age_seconds: Option<f64>,
    /// A rebalance is owed (a resize or an incomplete rebalance is pending).
    pub rebalance_owed: bool,
    /// The hash of the limits file the engine last loaded, or null.
    pub limits_hash: Option<String>,
}

/// One held position.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct PositionView {
    pub symbol: String,
    /// Signed quantity as decimal text.
    pub qty: String,
    /// Average cost as decimal text.
    pub avg_cost: String,
}

/// One order that is not in a final state.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct OrderView {
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    pub state: String,
    pub qty: String,
    /// The limit price as decimal text, or null for a market order.
    pub limit_px: Option<String>,
}

/// One promotion gate's newest stored state for a strategy version.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct GateView {
    pub name: String,
    /// "pass", "fail" or "not_evaluated".
    pub state: String,
}

/// One strategy version with its gates.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct VersionView {
    pub id: f64,
    pub git_sha: String,
    pub config_hash: String,
    pub gates: Vec<GateView>,
}

/// One engine step row.
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct StepView {
    pub run_key: String,
    pub step: String,
    /// "started", "done" or "failed".
    pub status: String,
    /// The step's detail as JSON text.
    pub detail: String,
    /// Seconds since the step was recorded, by the database's clock.
    pub age_seconds: f64,
}

/// The store, read for the screens, and the kill switch.
#[module]
pub struct Nisse;

#[module]
impl Nisse {
    /// The engine's state as stored. Rejects when the store cannot be reached or read: the
    /// caller must then show "cannot confirm", never the last state it had.
    pub async fn status(url: String) -> Result<StatusView, String> {
        run(url, |client| async move {
            let status = data::status(&client, HEARTBEAT_SOURCE).await?;
            Ok(StatusView {
                kill_switch: status.kill_switch,
                drawdown_mode: status.drawdown_mode,
                equity_peak: status.equity_peak.map(|peak| peak.to_string()),
                // Seconds fit a JS number exactly far beyond any real age.
                heartbeat_age_seconds: status.heartbeat_age_seconds.map(|age| age as f64),
                watcher_age_seconds: status.watcher_age_seconds.map(|age| age as f64),
                rebalance_owed: status.rebalance_owed,
                limits_hash: status.limits_hash,
            })
        })
        .await
    }

    /// The held positions, by symbol.
    pub async fn positions(url: String) -> Result<Vec<PositionView>, String> {
        run(url, |client| async move {
            let rows = data::positions(&client).await?;
            Ok(rows
                .into_iter()
                .map(|row| PositionView {
                    symbol: row.symbol.as_str().to_owned(),
                    qty: row.qty_after.to_string(),
                    avg_cost: row.avg_cost.to_string(),
                })
                .collect())
        })
        .await
    }

    /// The orders that are not in a final state.
    pub async fn orders(url: String) -> Result<Vec<OrderView>, String> {
        run(url, |client| async move {
            let rows = data::open_orders(&client).await?;
            Ok(rows
                .into_iter()
                .map(|row| OrderView {
                    client_order_id: row.client_order_id,
                    symbol: row.symbol,
                    side: row.side,
                    state: row.state,
                    qty: row.qty.to_string(),
                    limit_px: row.limit_px.map(|px| px.to_string()),
                })
                .collect())
        })
        .await
    }

    /// Every strategy version, newest first, with the newest stored state of each gate.
    pub async fn versions(url: String) -> Result<Vec<VersionView>, String> {
        run(url, |client| async move {
            let rows = data::versions(&client).await?;
            Ok(rows
                .into_iter()
                .map(|row| VersionView {
                    // Ids are small; a JS number holds them exactly.
                    id: row.strategy_version_id as f64,
                    git_sha: row.git_sha,
                    config_hash: row.config_hash,
                    gates: row
                        .gates
                        .into_iter()
                        .map(|(name, state)| GateView {
                            name,
                            state: match state {
                                data::GateState::Pass => "pass",
                                data::GateState::Fail => "fail",
                                data::GateState::NotEvaluated => "not_evaluated",
                            }
                            .to_owned(),
                        })
                        .collect(),
                })
                .collect())
        })
        .await
    }

    /// The engine's newest steps, newest first.
    pub async fn steps(url: String) -> Result<Vec<StepView>, String> {
        run(url, |client| async move {
            let rows = data::recent_steps(&client, STEP_LIMIT).await?;
            Ok(rows
                .into_iter()
                .map(|row| StepView {
                    run_key: row.run_key,
                    step: row.step,
                    status: row.status,
                    detail: row.detail.to_string(),
                    age_seconds: row.age_seconds as f64,
                })
                .collect())
        })
        .await
    }

    /// Engages the kill switch in the name of `by` and reads it back. Resolves with the id of
    /// the row written only when the store shows the switch engaged afterwards; rejects
    /// otherwise, and the caller must then treat the switch as NOT confirmed.
    pub async fn engage_kill(url: String, by: String) -> Result<f64, String> {
        run(url, |client| async move {
            let id = data::engage_kill(&client, &by).await?;
            Ok(id as f64)
        })
        .await
    }
}

/// Connects to the store at `url`, runs `work` on the connection, and returns its result, all
/// within [`CALL_TIMEOUT`], on the runtime's threads (`background`), so the UI thread is never
/// held. Every failure becomes a message for the screen; none contains the URL.
async fn run<T, F, Fut>(url: String, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(tokio_postgres::Client) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, data::UiError>> + Send + 'static,
{
    let joined = background(async move {
        let call = async {
            let client = nisse_store::connect(&url)
                .await
                .map_err(|err| format!("cannot reach the store: {err}"))?;
            work(client).await.map_err(|err| err.to_string())
        };
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(result) => result,
            Err(_) => Err("the store did not answer in time".to_owned()),
        }
    })
    .await;
    // The outer error is the task itself failing (a panic), not the call.
    joined.map_err(|err| format!("the call did not finish: {err}"))?
}
