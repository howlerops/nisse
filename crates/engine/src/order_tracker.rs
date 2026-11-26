//! In-memory order state, driven by the broker's cumulative figures.
//!
//! The broker is the source of truth. Every update or snapshot carries the order's cumulative
//! filled quantity, and fills are derived from how far that figure moved, so a message that is
//! duplicated, replayed after a reconnect, or delivered out of order can never be counted
//! twice, and a message that was missed is made up by the next one.

use std::collections::BTreeMap;

use nisse_core::{
    BrokerOrder, Fill, FillDetail, OrderIntent, OrderState,
    nautilus::{ClientOrderId, Price, Quantity, UnixNanos, VenueOrderId},
};

use crate::EngineError;

/// What the engine knows about one of its orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedOrder {
    pub intent: OrderIntent,
    /// Unknown until the broker first reports the order.
    pub venue_order_id: Option<VenueOrderId>,
    pub state: OrderState,
    pub cum_filled_qty: Quantity,
    pub avg_fill_px: Option<Price>,
    pub last_update_ts: UnixNanos,
}

/// The effect of one broker update on a tracked order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Applied {
    /// `(before, after)` when the state changed.
    pub state_change: Option<(OrderState, OrderState)>,
    /// The newly filled quantity, when the cumulative figure moved forward.
    pub fill: Option<Fill>,
}

/// All orders the engine has intended, keyed by client order id. A `BTreeMap` so iteration
/// order is deterministic.
#[derive(Debug, Default)]
pub struct OrderTracker {
    orders: BTreeMap<ClientOrderId, TrackedOrder>,
}

#[allow(unused_variables, reason = "stub: Phase 1 task T5")]
impl OrderTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts tracking an order in state `Intent` with nothing filled. Call it when the
    /// intent is recorded, before submission. Errors with `DuplicateOrder` if the client
    /// order id is already tracked.
    pub fn track_intent(&mut self, intent: OrderIntent) -> Result<(), EngineError> {
        todo!("Phase 1 task T5")
    }

    /// Applies the broker's view of an order (from a stream update or a REST snapshot).
    /// `fill` is the single execution the message reported, if any; `ts` is the event time.
    /// The rules are in `docs/phases/phase-1.md`, "Order tracker rules". Errors with
    /// `UnknownOrder` when the client order id is not tracked.
    pub fn apply(
        &mut self,
        order: &BrokerOrder,
        fill: Option<&FillDetail>,
        ts: UnixNanos,
    ) -> Result<Applied, EngineError> {
        todo!("Phase 1 task T5")
    }

    /// Marks a tracked order as `Rejected` because the broker has no record of it (the submit
    /// never arrived). No-op with `state_change: None` if the order is already terminal.
    pub fn mark_never_arrived(
        &mut self,
        client_order_id: ClientOrderId,
        ts: UnixNanos,
    ) -> Result<Applied, EngineError> {
        todo!("Phase 1 task T5")
    }

    #[must_use]
    pub fn get(&self, client_order_id: ClientOrderId) -> Option<&TrackedOrder> {
        self.orders.get(&client_order_id)
    }

    /// Orders that are not in a terminal state, in client order id order.
    pub fn open_orders(&self) -> impl Iterator<Item = &TrackedOrder> {
        self.orders.values().filter(|o| !o.state.is_terminal())
    }
}
