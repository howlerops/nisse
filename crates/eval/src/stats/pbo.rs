//! Probability of backtest overfitting by combinatorially symmetric cross-validation.

/// Blocks the sessions are cut into; half are in-sample in each combination.
pub const SPLITS: usize = 16;

/// The probability of backtest overfitting of `series` (one return series per
/// configuration, all over the same sessions). Rules in `docs/phases/phase-5.md`. `None`
/// when there are fewer than two series, their lengths differ, or there are fewer than
/// `2 × SPLITS` sessions.
#[allow(unused_variables, reason = "stub: Phase 5 task ST6")]
#[must_use]
pub fn pbo(series: &[&[f64]]) -> Option<f64> {
    todo!("Phase 5 task ST6")
}
