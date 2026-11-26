//! Rolling per-symbol statistics, updated one bar at a time.
//!
//! This is the per-bar path. Each symbol owns one preallocated ring buffer of closes and two
//! running sums, so a new bar costs a handful of exact decimal operations and no allocation,
//! however long the window is. All arithmetic is `Decimal`, so the running sums do not drift
//! the way floating-point sums do when values are added and later subtracted.

use rust_decimal::Decimal;

/// How much history the statistics need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSpec {
    /// Momentum is the return from `lookback_days` sessions ago to `skip_days` sessions ago.
    pub lookback_days: usize,
    /// The most recent sessions left out of momentum (the short-term reversal window).
    pub skip_days: usize,
    /// Number of daily returns in the variance estimate.
    pub vol_window: usize,
}

impl WindowSpec {
    /// Closes that must be retained: enough for both momentum and the variance window.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        let momentum = self.lookback_days + 1;
        let variance = self.vol_window + 1;
        if momentum > variance { momentum } else { variance }
    }
}

/// The statistics of one symbol after its latest close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Sessions seen so far (not capped at the window).
    pub history_days: u64,
    pub close: Decimal,
    /// `close[t - skip] / close[t - lookback] - 1`, when `history_days > lookback_days`.
    pub momentum: Option<Decimal>,
    /// Population variance of the last `vol_window` simple daily returns, when that many
    /// returns exist. Never negative.
    pub var_daily: Option<Decimal>,
}

/// One symbol's rolling window.
#[derive(Debug, Clone)]
pub struct RollingStats {
    spec: WindowSpec,
    /// Ring buffer of the last `spec.capacity()` closes; allocated once in `new`.
    closes: Vec<Decimal>,
    /// Index in `closes` where the next close goes.
    next: usize,
    history_days: u64,
    /// Sum and sum of squares of the returns currently inside the variance window.
    ret_sum: Decimal,
    ret_sq_sum: Decimal,
}

#[allow(unused_variables, reason = "stub: Phase 3 task F1")]
impl RollingStats {
    /// Allocates the window. Errors with `InvalidConfig` unless `lookback_days > skip_days`,
    /// `vol_window >= 2` and `lookback_days >= 1`.
    pub fn new(spec: WindowSpec) -> Result<Self, crate::EngineError> {
        todo!("Phase 3 task F1")
    }

    /// Feeds the next session's close (must be positive; a non-positive close is
    /// `InvalidBar`, and the window is left unchanged). O(1), no allocation: the return that
    /// enters the variance window is added to the running sums and the one that leaves is
    /// subtracted.
    pub fn push(&mut self, close: Decimal) -> Result<(), crate::EngineError> {
        todo!("Phase 3 task F1")
    }

    /// The current statistics; `None` before the first close.
    #[must_use]
    pub fn stats(&self) -> Option<Stats> {
        todo!("Phase 3 task F1")
    }
}
