//! Moments of a return series and the ratios built on them.

/// `Σr / n`. `None` for an empty slice or a non-finite result.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn mean(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// Sample variance (divide by `n - 1`). `None` when `n < 2`.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn variance(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// `m3 / m2^1.5` with population central moments. `None` when `n < 3` or `m2` is zero.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn skewness(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// `m4 / m2²` with population central moments (not excess: about 3 for a normal sample).
/// `None` when `n < 3` or `m2` is zero.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn kurtosis(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// Per-period Sharpe ratio: `mean / sqrt(variance)`. `None` when `n < 2` or the variance is
/// zero.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn sharpe(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// Per-period Sortino ratio: `mean / sqrt(Σ min(r, 0)² / n)`. `None` for an empty slice or
/// when no return is negative.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn sortino(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// The largest peak-to-trough fall of the compounded curve `Π(1 + r)`, starting from 1, as
/// a positive fraction (0.25 is a 25% drawdown; 0 when the curve never falls). `None` for an
/// empty slice or a return at or below -1.
#[allow(unused_variables, reason = "stub: Phase 5 task ST2")]
#[must_use]
pub fn max_drawdown(returns: &[f64]) -> Option<f64> {
    todo!("Phase 5 task ST2")
}

/// A per-period ratio (Sharpe, Sortino) annualised: `ratio × sqrt(periods_per_year)`.
#[must_use]
pub fn annualise(ratio: f64, periods_per_year: u32) -> f64 {
    ratio * f64::from(periods_per_year).sqrt()
}
