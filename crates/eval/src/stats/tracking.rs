//! How far live results have moved from the backtest replayed over the same sessions.

/// The two distances the tracking band is declared in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tracking {
    /// Annualised standard deviation of `live - backtest` daily returns.
    pub tracking_error: f64,
    /// The largest absolute gap between the two compounded curves (each starting at 1).
    pub max_gap: f64,
}

/// Compares live and backtest daily returns over the same sessions. `None` when the lengths
/// differ, there are fewer than 2 sessions, or a result is not finite.
#[allow(unused_variables, reason = "stub: Phase 5 task ST4")]
#[must_use]
pub fn tracking(live: &[f64], backtest: &[f64], periods_per_year: u32) -> Option<Tracking> {
    todo!("Phase 5 task ST4")
}
