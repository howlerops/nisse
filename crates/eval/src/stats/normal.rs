//! The standard normal distribution: `std` has neither function.

/// Φ(x), the standard normal cumulative distribution (Abramowitz and Stegun 26.2.17,
/// absolute error below 7.5e-8). Total on finite input, with `cdf(-inf) = 0` and
/// `cdf(inf) = 1`; a NaN input gives NaN, so callers check their inputs first.
#[allow(unused_variables, reason = "stub: Phase 5 task ST1")]
#[must_use]
pub fn cdf(x: f64) -> f64 {
    todo!("Phase 5 task ST1")
}

/// Φ⁻¹(p) for `0 < p < 1` (Acklam's rational approximation, relative error about 1e-9);
/// `None` otherwise, including for NaN.
#[allow(unused_variables, reason = "stub: Phase 5 task ST1")]
#[must_use]
pub fn inv_cdf(p: f64) -> Option<f64> {
    todo!("Phase 5 task ST1")
}
