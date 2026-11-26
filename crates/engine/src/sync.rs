//! Bringing the tracker into agreement with the broker: on startup, on a timer, and after
//! every order stream reconnect.

use std::collections::BTreeMap;

use nisse_core::{
    Broker, BrokerOrder, Fill, OrderState,
    nautilus::{ClientOrderId, Symbol, UnixNanos},
};
use rust_decimal::Decimal;

use crate::{EngineError, Finding, OrderTracker, reconcile};

/// Timing rules for [`sync_with_broker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOptions {
    /// How old an intent must be before a "not found" from the broker is believed. A
    /// just-sent order may simply not be registered yet.
    pub never_arrived_grace_nanos: u64,
    /// How long after a never-arrived verdict the order is still looked up on every pass. If
    /// the broker turns out to have it after all (even already filled), it is revived and its
    /// fills are recovered. After this the verdict is final.
    pub never_arrived_recheck_nanos: u64,
}

impl Default for SyncOptions {
    /// One minute of grace, one day of rechecks.
    fn default() -> Self {
        Self {
            never_arrived_grace_nanos: 60 * 1_000_000_000,
            never_arrived_recheck_nanos: 24 * 3_600 * 1_000_000_000,
        }
    }
}

/// An order whose broker snapshot could not be applied. The pass carries on with the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProblem {
    pub client_order_id: ClientOrderId,
    pub reason: String,
}

/// A state transition made during a pass. The caller records each as an `order_event` row;
/// transitions can go "backwards" (`rejected` to `accepted` when a never-arrived verdict is
/// overturned, `pending_cancel` to `accepted` when a cancel is refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateChange {
    pub client_order_id: ClientOrderId,
    pub before: OrderState,
    pub after: OrderState,
}

/// What one reconciliation pass changed or found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Every difference found, in [`crate::reconcile`] order, before anything was applied.
    pub findings: Vec<Finding>,
    /// Every order state transition the pass made, in the order it made them.
    pub state_changes: Vec<StateChange>,
    /// Fills discovered while applying broker state (missed while disconnected). These are
    /// for the fill log only: positions are overwritten from the broker in the same pass, so
    /// the caller must not also add these quantities to its positions.
    pub fills: Vec<Fill>,
    /// Orders whose snapshot could not be applied (inconsistent broker figures). They stay
    /// as they were and will be reported again on the next pass: alert on these.
    pub problems: Vec<SyncProblem>,
    /// Open orders at the broker of a kind the engine never sends. Non-zero means something
    /// else is trading this account: alert.
    pub unrepresentable_at_broker: u32,
}

impl SyncReport {
    /// Whether every order's state is now known. `false` when any order could not be
    /// reconciled (a failed lookup, inconsistent broker figures): the engine must not decide
    /// on top of that, even though the pass itself returned `Ok`.
    #[must_use]
    pub fn is_reconciled(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Reconciles `tracker` and `local_positions` with the broker, which is the source of truth.
///
/// 1. Fetch the broker's open orders and positions.
/// 2. Compute findings with [`crate::reconcile`].
/// 3. `OrderDrift`: apply the snapshot to the tracker (`apply_snapshot`: the broker's state
///    wins, even over a local terminal state, unless the snapshot is older than what the
///    tracker already has from the broker).
/// 4. `NotOpenAtBroker`: fetch the order by client order id. Found: apply it. Not found
///    (`Ok(None)`):
///    - an order not yet judged: if its intent is at least `never_arrived_grace_nanos` old,
///      mark it never-arrived at `now`; if younger, leave it for a later pass;
///    - an order already judged never-arrived: keep it under watch until the verdict is
///      `never_arrived_recheck_nanos` old, then make it final.
/// 5. `PositionMismatch`: overwrite the local quantity with the broker's (remove the entry
///    when the broker is flat).
/// 6. `UnknownAtBroker`: nothing is changed; it is only reported.
///
/// An order whose snapshot cannot be applied, or whose lookup fails, is listed in `problems`
/// and skipped; it does not stop the pass. Only a failure of the two opening reads (open
/// orders, positions) aborts with `Broker(..)`, and at that point nothing has been changed. `now` is the caller's clock reading.
pub async fn sync_with_broker<B: Broker>(
    broker: &B,
    tracker: &mut OrderTracker,
    local_positions: &mut BTreeMap<Symbol, Decimal>,
    now: UnixNanos,
    options: SyncOptions,
) -> Result<SyncReport, EngineError> {
    let open = broker.open_orders().await.map_err(broker_error)?;
    let positions = broker.positions().await.map_err(broker_error)?;

    let mut report = SyncReport {
        findings: reconcile(tracker, local_positions, &open.orders, &positions),
        unrepresentable_at_broker: open.unrepresentable,
        ..SyncReport::default()
    };

    for index in 0..report.findings.len() {
        let Some(&finding) = report.findings.get(index) else {
            break;
        };
        match finding {
            Finding::OrderDrift { order } => apply_snapshot(tracker, &order, &mut report),
            Finding::NotOpenAtBroker { client_order_id } => {
                // A failed lookup is a problem for this order only. Ending the pass here would
                // throw away the report, and with it the fills already applied to the tracker,
                // which no later pass would find again.
                match broker.order_by_client_id(client_order_id).await {
                    Ok(Some(order)) => apply_snapshot(tracker, &order, &mut report),
                    Ok(None) => not_found(tracker, client_order_id, now, options, &mut report)?,
                    Err(error) => report.problems.push(SyncProblem {
                        client_order_id,
                        reason: format!("lookup failed: {error}"),
                    }),
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

    Ok(report)
}

/// The broker has no record of `client_order_id`.
fn not_found(
    tracker: &mut OrderTracker,
    client_order_id: ClientOrderId,
    now: UnixNanos,
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), EngineError> {
    let Some(tracked) = tracker.get(client_order_id) else {
        return Ok(());
    };
    let age_of = |since: UnixNanos| now.as_u64().saturating_sub(since.as_u64());
    match tracked.never_arrived_at {
        Some(judged_at) => {
            if age_of(judged_at) >= options.never_arrived_recheck_nanos {
                tracker.settle_never_arrived(client_order_id)?;
            }
        }
        None => {
            if age_of(tracked.intent.ts) >= options.never_arrived_grace_nanos {
                let applied = tracker.mark_never_arrived(client_order_id, now)?;
                record_change(client_order_id, applied.state_change, report);
            }
        }
    }
    Ok(())
}

fn record_change(
    client_order_id: ClientOrderId,
    change: Option<(OrderState, OrderState)>,
    report: &mut SyncReport,
) {
    if let Some((before, after)) = change {
        report.state_changes.push(StateChange {
            client_order_id,
            before,
            after,
        });
    }
}

/// Applies one broker snapshot, keeping the fill and state change it produced. A snapshot
/// that cannot be applied becomes a [`SyncProblem`] instead of ending the pass.
fn apply_snapshot(tracker: &mut OrderTracker, order: &BrokerOrder, report: &mut SyncReport) {
    match tracker.apply_snapshot(order) {
        Ok(applied) => {
            report.fills.extend(applied.fill);
            record_change(order.client_order_id, applied.state_change, report);
        }
        Err(error) => report.problems.push(SyncProblem {
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
