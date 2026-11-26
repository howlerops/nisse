//! Bringing the tracker into agreement with the broker: on startup, on a timer, and after
//! every order stream reconnect.

use std::collections::BTreeMap;

use nisse_core::{
    Broker, BrokerOrder, Fill,
    nautilus::{ClientOrderId, Symbol, UnixNanos},
};
use rust_decimal::Decimal;

use crate::{EngineError, Finding, OrderTracker, reconcile};

/// An order whose broker snapshot could not be applied. The pass carries on with the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProblem {
    pub client_order_id: ClientOrderId,
    pub reason: String,
}

/// What one reconciliation pass changed or found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Every difference found, in [`crate::reconcile`] order, before anything was applied.
    pub findings: Vec<Finding>,
    /// Fills discovered while applying broker state (missed while disconnected).
    pub fills: Vec<Fill>,
    /// Orders whose snapshot could not be applied (inconsistent broker figures). They stay
    /// as they were and will be reported again on the next pass: alert on these.
    pub problems: Vec<SyncProblem>,
    /// Open orders at the broker of a kind the engine never sends. Non-zero means something
    /// else is trading this account: alert.
    pub unrepresentable_at_broker: u32,
}

/// Reconciles `tracker` and `local_positions` with the broker, which is the source of truth.
///
/// 1. Fetch the broker's open orders and positions.
/// 2. Compute findings with [`crate::reconcile`].
/// 3. `OrderDrift`: apply the snapshot to the tracker (`apply_snapshot`: the broker's state
///    wins, even over a local terminal state).
/// 4. `NotOpenAtBroker`: fetch the order by client order id. Found: apply it. Not found
///    (`Ok(None)`): if the intent is at least `never_arrived_grace_nanos` old, mark it
///    never-arrived at `now`; if it is younger, leave it for a later pass, because the broker
///    may simply not have registered a just-sent order yet. The verdict is not final either
///    way: a later snapshot of the order revives it.
/// 5. `PositionMismatch`: overwrite the local quantity with the broker's (remove the entry
///    when the broker is flat).
/// 6. `UnknownAtBroker`: nothing is changed; it is only reported.
///
/// An order whose snapshot cannot be applied is listed in `problems` and skipped; it does not
/// stop the pass. A failed broker request does: it aborts with `Broker(..)` and the tracker
/// keeps whatever was applied so far. `now` is the caller's clock reading.
pub async fn sync_with_broker<B: Broker>(
    broker: &B,
    tracker: &mut OrderTracker,
    local_positions: &mut BTreeMap<Symbol, Decimal>,
    now: UnixNanos,
    never_arrived_grace_nanos: u64,
) -> Result<SyncReport, EngineError> {
    let open = broker.open_orders().await.map_err(broker_error)?;
    let positions = broker.positions().await.map_err(broker_error)?;

    let findings = reconcile(tracker, local_positions, &open.orders, &positions);
    let mut fills = Vec::new();
    let mut problems = Vec::new();

    for &finding in &findings {
        match finding {
            Finding::OrderDrift { order } => {
                apply_snapshot(tracker, &order, &mut fills, &mut problems);
            }
            Finding::NotOpenAtBroker { client_order_id } => {
                match broker
                    .order_by_client_id(client_order_id)
                    .await
                    .map_err(broker_error)?
                {
                    Some(order) => apply_snapshot(tracker, &order, &mut fills, &mut problems),
                    None => {
                        let old_enough = tracker.get(client_order_id).is_some_and(|tracked| {
                            now.as_u64().saturating_sub(tracked.intent.ts.as_u64())
                                >= never_arrived_grace_nanos
                        });
                        if old_enough {
                            tracker.mark_never_arrived(client_order_id, now)?;
                        }
                    }
                }
            }
            Finding::PositionMismatch {
                symbol,
                broker: broker_qty,
                ..
            } => {
                if broker_qty.is_zero() {
                    local_positions.remove(&symbol);
                } else {
                    local_positions.insert(symbol, broker_qty);
                }
            }
            Finding::UnknownAtBroker { .. } => {}
        }
    }

    Ok(SyncReport {
        findings,
        fills,
        problems,
        unrepresentable_at_broker: open.unrepresentable,
    })
}

/// Applies one broker snapshot, keeping any fill it produced. A snapshot that cannot be
/// applied becomes a [`SyncProblem`] instead of ending the pass.
fn apply_snapshot(
    tracker: &mut OrderTracker,
    order: &BrokerOrder,
    fills: &mut Vec<Fill>,
    problems: &mut Vec<SyncProblem>,
) {
    match tracker.apply_snapshot(order) {
        Ok(applied) => fills.extend(applied.fill),
        Err(error) => problems.push(SyncProblem {
            client_order_id: order.client_order_id,
            reason: error.to_string(),
        }),
    }
}

fn broker_error<E>(err: E) -> EngineError
where
    E: std::error::Error + Send + Sync + 'static,
{
    EngineError::Broker(Box::new(err))
}
