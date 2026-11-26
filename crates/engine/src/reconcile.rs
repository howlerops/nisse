//! Comparing the engine's view with the broker's. Pure: no I/O, no clock.

use std::collections::BTreeMap;

use nisse_core::{
    BrokerOrder, BrokerPosition,
    nautilus::{ClientOrderId, Symbol},
};
use rust_decimal::Decimal;

use crate::OrderTracker;

/// One difference between local and broker state. The broker wins every time; a finding says
/// what has to be done locally to agree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// The broker has this order open and its state or cumulative fill differs from the
    /// tracker's: apply the snapshot.
    OrderDrift { order: BrokerOrder },
    /// The tracker thinks this order is open (or still only an intent) but it is not among
    /// the broker's open orders: look it up by client order id. It was filled, cancelled or
    /// expired while we were not listening, or it never arrived.
    NotOpenAtBroker { client_order_id: ClientOrderId },
    /// The broker has an open order the engine never intended. Someone or something else is
    /// trading this account: alert, and do not touch it automatically.
    UnknownAtBroker { order: BrokerOrder },
    /// Signed quantities differ (long positive, short negative, absent zero): adopt the
    /// broker's and record a `position_event`.
    PositionMismatch {
        symbol: Symbol,
        local: Decimal,
        broker: Decimal,
    },
}

/// Lists every difference, in a deterministic order: order findings sorted by client order
/// id, then position findings sorted by symbol. An empty result means the views agree.
///
/// `local_positions` holds signed quantities; a missing symbol means flat.
#[allow(unused_variables, reason = "stub: Phase 1 task T6")]
#[must_use]
pub fn reconcile(
    tracker: &OrderTracker,
    local_positions: &BTreeMap<Symbol, Decimal>,
    broker_open_orders: &[BrokerOrder],
    broker_positions: &[BrokerPosition],
) -> Vec<Finding> {
    todo!("Phase 1 task T6")
}
