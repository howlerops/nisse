//! The engine's own ledger of positions, kept from fills and corrected from the broker.

use std::collections::BTreeMap;

use nisse_core::{
    PositionEvent,
    nautilus::{OrderSide, Symbol, UnixNanos},
};
use rust_decimal::Decimal;

/// One position: signed quantity and the average cost of what is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    /// Negative when short.
    pub qty: Decimal,
    /// Average price paid (long) or received (short) for the open quantity. Zero when flat.
    pub avg_cost: Decimal,
}

/// Positions by symbol. A `BTreeMap`, so iteration order is deterministic. Flat positions are
/// not stored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PositionBook {
    positions: BTreeMap<Symbol, Position>,
}

#[allow(unused_variables, reason = "stub: Phase 2 task E2")]
impl PositionBook {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The position in `symbol`; flat (`Position::default()`) when there is none.
    #[must_use]
    pub fn get(&self, symbol: Symbol) -> Position {
        todo!("Phase 2 task E2")
    }

    /// Signed quantity per symbol, in the form `sync_with_broker` reconciles.
    #[must_use]
    pub fn quantities(&self) -> BTreeMap<Symbol, Decimal> {
        todo!("Phase 2 task E2")
    }

    /// Applies one fill (`qty` and `px` positive; `side` gives the direction) and returns the
    /// row to record. Rules, with `signed = +qty` for a buy and `-qty` for a sell:
    ///
    /// - Adding to a position, or opening one from flat: quantity grows and `avg_cost` becomes
    ///   the quantity-weighted average of the old cost and `px`.
    /// - Reducing a position without crossing zero: quantity shrinks, `avg_cost` unchanged.
    /// - Closing exactly: the position is removed; the event has `qty_after` 0, `avg_cost` 0.
    /// - Crossing zero (a long sold past flat, or the reverse): the remainder is a new
    ///   position opened at `px`, so `avg_cost` becomes `px`.
    pub fn apply_fill(
        &mut self,
        symbol: Symbol,
        side: OrderSide,
        qty: Decimal,
        px: Decimal,
        ts: UnixNanos,
    ) -> PositionEvent {
        todo!("Phase 2 task E2")
    }

    /// Overwrites the position with the broker's figures (the broker is the source of truth)
    /// and returns the row to record, or `None` when nothing changed. A `broker_qty` of zero
    /// removes the position. `broker_avg_cost` is used when given; when it is `None` the old
    /// cost is kept if the sign of the quantity did not change, else it becomes zero.
    pub fn adopt(
        &mut self,
        symbol: Symbol,
        broker_qty: Decimal,
        broker_avg_cost: Option<Decimal>,
        ts: UnixNanos,
    ) -> Option<PositionEvent> {
        todo!("Phase 2 task E2")
    }
}
