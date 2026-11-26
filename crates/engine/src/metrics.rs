//! Summary statistics of an equity curve. These are statistics, not money, so they are `f64`.

use nisse_core::nautilus::UnixNanos;
use rust_decimal::Decimal;

/// One point on the equity curve: account equity at a session's close.
pub type EquityPoint = (UnixNanos, Decimal);

/// What a backtest (or a live period) looks like in a few numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// Sessions in the curve.
    pub sessions: u64,
    /// Final equity over initial equity, minus one.
    pub total_return: f64,
    /// Compound annual growth rate.
    pub cagr: f64,
    /// Annualised standard deviation of daily returns (sample, n - 1).
    pub ann_vol: f64,
    /// Annualised mean daily return over `ann_vol` (risk-free rate taken as zero). Zero when
    /// the volatility is zero.
    pub sharpe: f64,
    /// Largest peak-to-trough fall, as a positive fraction (0.25 is a 25% drawdown).
    pub max_drawdown: f64,
}

/// Summarises an equity curve. `None` when it has fewer than two points or a non-positive
/// starting equity.
#[allow(unused_variables, reason = "stub: Phase 3 task MT1")]
#[must_use]
pub fn summarize(curve: &[EquityPoint], sessions_per_year: u32) -> Option<Summary> {
    todo!("Phase 3 task MT1")
}

impl Summary {
    /// The summary as the JSON stored in `trial_result.result`. Non-finite numbers are
    /// written as `null`.
    #[allow(unused_variables, reason = "stub: Phase 3 task MT1")]
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        todo!("Phase 3 task MT1")
    }
}
