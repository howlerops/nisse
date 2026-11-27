//! Stationary bootstrap intervals (Politis and Romano).

use crate::stats::rng::Rng;

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
#[must_use]
pub fn interval(
    returns: &[f64],
    statistic: impl Fn(&[f64]) -> Option<f64>,
    config: &BootstrapConfig,
) -> Option<(f64, f64)> {
    let n = returns.len();
    let resamples = usize::try_from(config.resamples).ok()?;
    if n < 2
        || resamples == 0
        || !config.mean_block.is_finite()
        || config.mean_block < 1.0
        || !config.level.is_finite()
        || config.level <= 0.0
        || config.level >= 1.0
    {
        return None;
    }

    let p_new = 1.0 / config.mean_block;
    let mut rng = Rng::new(config.seed);
    let mut buffer = vec![0.0_f64; n];
    let mut stats: Vec<f64> = Vec::with_capacity(resamples);

    for _ in 0..resamples {
        let mut idx = rng.below(n);
        for slot in &mut buffer {
            *slot = returns[idx];
            // The step draw happens after every element, the last included, so the stream
            // is consumed the same way at every position.
            if rng.next_f64() < p_new {
                idx = rng.below(n);
            } else {
                idx = (idx + 1) % n;
            }
        }
        if let Some(s) = statistic(&buffer)
            && s.is_finite()
        {
            stats.push(s);
        }
    }

    if stats.is_empty() || stats.len() * 2 < resamples {
        return None;
    }
    stats.sort_unstable_by(f64::total_cmp);
    let lower = nearest_rank(&stats, (1.0 - config.level) / 2.0);
    let upper = nearest_rank(&stats, (1.0 + config.level) / 2.0);
    Some((lower, upper))
}

/// Nearest-rank quantile of a sorted, non-empty slice: index `ceil(q × m) - 1`, clamped to
/// `[0, m - 1]`.
fn nearest_rank(sorted: &[f64], q: f64) -> f64 {
    let m = sorted.len();
    let rank = (q * m as f64).ceil();
    let idx = if rank >= 1.0 { rank as usize - 1 } else { 0 };
    sorted[idx.min(m - 1)]
}

#[cfg(test)]
mod tests {
    use super::{BootstrapConfig, interval, nearest_rank};
    use crate::stats::rng::Rng;
    use std::cell::Cell;

    fn mean(x: &[f64]) -> Option<f64> {
        Some(x.iter().sum::<f64>() / x.len() as f64)
    }

    fn config(seed: u64) -> BootstrapConfig {
        BootstrapConfig::with_seed(seed)
    }

    /// 50 values with mixed signs and a mean away from zero.
    fn mixed_series() -> Vec<f64> {
        (0..50)
            .map(|i| {
                if i % 3 == 0 {
                    -0.02
                } else {
                    0.004 * f64::from(i % 7)
                }
            })
            .collect()
    }

    fn noisy_series(n: usize, seed: u64) -> Vec<f64> {
        let mut rng = Rng::new(seed);
        (0..n).map(|_| -0.01 + 0.04 * rng.next_f64()).collect()
    }

    #[test]
    fn same_inputs_and_seed_give_same_interval() {
        let data = mixed_series();
        let a = interval(&data, mean, &config(9));
        let b = interval(&data, mean, &config(9));
        assert!(a.is_some());
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_gives_different_interval() {
        let data = noisy_series(200, 1);
        let a = interval(&data, mean, &config(1));
        let b = interval(&data, mean, &config(2));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_series_gives_degenerate_interval() {
        let data = [0.01; 50];
        let (lo, hi) = interval(&data, mean, &config(3)).unwrap_or((f64::NAN, f64::NAN));
        assert!((lo - 0.01).abs() < 1e-15);
        assert!((hi - 0.01).abs() < 1e-15);
    }

    #[test]
    fn interval_contains_sample_mean_and_is_ordered() {
        let data = mixed_series();
        let sample_mean = mean(&data).unwrap_or(f64::NAN);
        let (lo, hi) = interval(&data, mean, &config(4)).unwrap_or((f64::NAN, f64::NAN));
        assert!(lo <= hi);
        assert!(
            lo <= sample_mean && sample_mean <= hi,
            "{lo} {sample_mean} {hi}"
        );
    }

    #[test]
    fn half_width_matches_standard_error() {
        let data = noisy_series(500, 17);
        let (lo, hi) = interval(&data, mean, &config(5)).unwrap_or((f64::NAN, f64::NAN));
        let half = (hi - lo) / 2.0;
        assert!((0.0005..=0.002).contains(&half), "half-width {half}");
    }

    #[test]
    fn lower_level_gives_narrower_interval() {
        let data = noisy_series(300, 23);
        let wide = BootstrapConfig {
            level: 0.95,
            ..config(6)
        };
        let narrow = BootstrapConfig {
            level: 0.5,
            ..config(6)
        };
        let (wl, wh) = interval(&data, mean, &wide).unwrap_or((f64::NAN, f64::NAN));
        let (nl, nh) = interval(&data, mean, &narrow).unwrap_or((f64::NAN, f64::NAN));
        assert!(nh - nl < wh - wl);
    }

    #[test]
    fn mean_block_extremes_work() {
        let data = noisy_series(100, 8);
        let fresh = BootstrapConfig {
            mean_block: 1.0,
            ..config(7)
        };
        let one_block = BootstrapConfig {
            mean_block: 1e9,
            ..config(7)
        };
        let a = interval(&data, mean, &fresh);
        let b = interval(&data, mean, &one_block);
        assert!(a.is_some());
        assert!(b.is_some());
    }

    #[test]
    fn most_resamples_returning_none_gives_none() {
        let data = mixed_series();
        let calls = Cell::new(0_u32);
        let stat = |x: &[f64]| {
            let c = calls.get();
            calls.set(c + 1);
            if c.is_multiple_of(3) { mean(x) } else { None }
        };
        assert_eq!(interval(&data, stat, &config(10)), None);
    }

    #[test]
    fn few_resamples_returning_none_are_tolerated() {
        let data = mixed_series();
        let calls = Cell::new(0_u32);
        let stat = |x: &[f64]| {
            let c = calls.get();
            calls.set(c + 1);
            if c.is_multiple_of(10) { None } else { mean(x) }
        };
        assert!(interval(&data, stat, &config(11)).is_some());
    }

    #[test]
    fn non_finite_statistics_are_dropped() {
        let data = mixed_series();
        let stat = |_: &[f64]| Some(f64::NAN);
        assert_eq!(interval(&data, stat, &config(12)), None);
    }

    #[test]
    fn invalid_configs_give_none() {
        let data = mixed_series();
        let base = config(13);
        let cases = [
            BootstrapConfig {
                resamples: 0,
                ..base
            },
            BootstrapConfig {
                mean_block: 0.5,
                ..base
            },
            BootstrapConfig {
                mean_block: 0.0,
                ..base
            },
            BootstrapConfig {
                mean_block: f64::NAN,
                ..base
            },
            BootstrapConfig {
                mean_block: f64::INFINITY,
                ..base
            },
            BootstrapConfig { level: 0.0, ..base },
            BootstrapConfig { level: 1.0, ..base },
            BootstrapConfig {
                level: -0.1,
                ..base
            },
            BootstrapConfig {
                level: f64::NAN,
                ..base
            },
        ];
        for cfg in cases {
            assert_eq!(interval(&data, mean, &cfg), None, "{cfg:?}");
        }
    }

    #[test]
    fn series_shorter_than_two_gives_none() {
        assert_eq!(interval(&[], mean, &config(14)), None);
        assert_eq!(interval(&[0.01], mean, &config(14)), None);
    }

    #[test]
    fn nearest_rank_quantile_on_small_slice() {
        let s = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(nearest_rank(&s, 0.25), 1.0);
        assert_eq!(nearest_rank(&s, 0.5), 2.0);
        assert_eq!(nearest_rank(&s, 1.0), 4.0);
        assert_eq!(nearest_rank(&s, 0.0), 1.0);
    }
}
