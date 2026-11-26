//! What a simulated fill costs: slippage against the reference price, and fees.

use nisse_core::nautilus::OrderSide;
use rust_decimal::Decimal;

/// A simple, deliberately pessimistic cost model for backtests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Slippage against the reference price, in basis points, always against the order: a buy
    /// fills higher, a sell lower. Covers half the spread plus impact.
    pub slippage_bps: Decimal,
    /// Fee per share traded, in dollars (regulatory fees; the broker's commission is zero).
    pub fee_per_share: Decimal,
}

/// A fill priced by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricedFill {
    /// The fill price, rounded to 6 decimal places away from the order's favour.
    pub px: Decimal,
    /// Total fee for the quantity, rounded up to the cent. Never negative.
    pub fee: Decimal,
}

#[allow(unused_variables, reason = "stub: Phase 3 task C1")]
impl CostModel {
    /// Errors with `InvalidConfig` when either figure is negative.
    pub fn new(slippage_bps: Decimal, fee_per_share: Decimal) -> Result<Self, crate::EngineError> {
        todo!("Phase 3 task C1")
    }

    /// Prices a fill of `qty` shares at `reference_px` (both positive).
    #[must_use]
    pub fn price(&self, side: OrderSide, reference_px: Decimal, qty: Decimal) -> PricedFill {
        todo!("Phase 3 task C1")
    }
}
