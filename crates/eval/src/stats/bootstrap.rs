//! Stationary bootstrap intervals (Politis and Romano).

/// How a bootstrap interval is computed. Stored with the result so it can be reproduced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BootstrapConfig {
    pub resamples: u32,
    /// Mean block length `L`: each next element continues the block with probability
    /// `1 - 1/L`. Must be at least 1.
    pub mean_block: f64,
    /// The interval's coverage, in `(0, 1)`: 0.95 gives the 2.5% and 97.5% quantiles.
    pub level: f64,
    pub seed: u64,
}

impl BootstrapConfig {
    /// 2000 resamples, mean block 21, level 0.95.
    #[must_use]
    pub const fn with_seed(seed: u64) -> Self {
        Self {
            resamples: 2000,
            mean_block: 21.0,
            level: 0.95,
            seed,
        }
    }
}

/// The `(lower, upper)` interval of `statistic` over stationary-bootstrap resamples of
/// `returns`. Rules in `docs/phases/phase-5.md`, "Bootstrap". `None` when `n < 2`, the
/// config is invalid (`resamples == 0`, `mean_block < 1`, `level` outside `(0, 1)`), or
/// fewer than half the resamples give a statistic.
#[allow(unused_variables, reason = "stub: Phase 5 task ST3")]
#[must_use]
pub fn interval(
    returns: &[f64],
    statistic: impl Fn(&[f64]) -> Option<f64>,
    config: &BootstrapConfig,
) -> Option<(f64, f64)> {
    todo!("Phase 5 task ST3")
}
