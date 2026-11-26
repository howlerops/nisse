//! Bringing the tracker into agreement with the broker: on startup, on a timer, and after
//! every order stream reconnect.

use std::collections::BTreeMap;

use nisse_core::{
    Broker, BrokerOrder, Fill,
    nautilus::{Symbol, UnixNanos},
};
use rust_decimal::Decimal;

use crate::{EngineError, Finding, OrderTracker, reconcile};

/// What one reconciliation pass changed or found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Every difference found, in [`crate::reconcile`] order, before anything was applied.
    pub findings: Vec<Finding>,
    /// Fills discovered while applying broker state (missed while disconnected).
    pub fills: Vec<Fill>,
}

/// Reconciles `tracker` and `local_positions` with the broker, which is the source of truth.
///
/// 1. Fetch the broker's open orders and positions.
/// 2. Compute findings with [`crate::reconcile`].
/// 3. `OrderDrift`: apply the snapshot to the tracker.
/// 4. `NotOpenAtBroker`: fetch the order by client order id. Found: apply it. Not found
///    (`Ok(None)`): `tracker.mark_never_arrived(id, now)`.
/// 5. `PositionMismatch`: overwrite the local quantity with the broker's (remove the entry
///    when the broker is flat).
/// 6. `UnknownAtBroker`: nothing is changed; it is only reported.
///
/// `now` is the caller's clock reading, used as the event time for never-arrived orders. Any
/// broker error aborts the pass with `Broker(..)`; the tracker keeps whatever was applied.
pub async fn sync_with_broker<B: Broker>(
    broker: &B,
    tracker: &mut OrderTracker,
    local_positions: &mut BTreeMap<Symbol, Decimal>,
    now: UnixNanos,
) -> Result<SyncReport, EngineError> {
    let open_orders = broker.open_orders().await.map_err(broker_error)?;
    let positions = broker.positions().await.map_err(broker_error)?;

    let findings = reconcile(tracker, local_positions, &open_orders, &positions);
    let mut fills = Vec::new();

    for &finding in &findings {
        match finding {
            Finding::OrderDrift { order } => apply_snapshot(tracker, &order, &mut fills)?,
            Finding::NotOpenAtBroker { client_order_id } => {
                match broker
                    .order_by_client_id(client_order_id)
                    .await
                    .map_err(broker_error)?
                {
                    // A non-terminal answer (the open-orders list was stale) is applied like
                    // any snapshot: it either changes nothing or reports a fill.
                    Some(order) => apply_snapshot(tracker, &order, &mut fills)?,
                    None => {
                        tracker.mark_never_arrived(client_order_id, now)?;
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

    Ok(SyncReport { findings, fills })
}

/// Applies one broker snapshot without a stream execution and keeps any fill it produced.
fn apply_snapshot(
    tracker: &mut OrderTracker,
    order: &BrokerOrder,
    fills: &mut Vec<Fill>,
) -> Result<(), EngineError> {
    if let Some(fill) = tracker.apply(order, None, order.ts)?.fill {
        fills.push(fill);
    }
    Ok(())
}

fn broker_error<E>(err: E) -> EngineError
where
    E: std::error::Error + Send + Sync + 'static,
{
    EngineError::Broker(Box::new(err))
}
