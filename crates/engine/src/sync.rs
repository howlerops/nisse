//! Bringing the tracker into agreement with the broker: on startup, on a timer, and after
//! every order stream reconnect.

use std::collections::BTreeMap;

use nisse_core::{
    Broker, Fill,
    nautilus::{Symbol, UnixNanos},
};
use rust_decimal::Decimal;

use crate::{EngineError, Finding, OrderTracker};

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
#[allow(unused_variables, reason = "stub: Phase 1 task T10")]
pub async fn sync_with_broker<B: Broker>(
    broker: &B,
    tracker: &mut OrderTracker,
    local_positions: &mut BTreeMap<Symbol, Decimal>,
    now: UnixNanos,
) -> Result<SyncReport, EngineError> {
    todo!("Phase 1 task T10")
}
