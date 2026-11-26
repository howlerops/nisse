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
        if momentum > variance {
            momentum
        } else {
            variance
        }
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

/// Error for a statistic that would overflow `Decimal`. Also used for the (unreachable)
/// case of an index outside its preallocated ring.
const ARITHMETIC: &str = "rolling statistics arithmetic overflowed";

/// Index of the slot after `index` in a ring of `len` slots.
const fn advance(index: usize, len: usize) -> usize {
    if index + 1 >= len { 0 } else { index + 1 }
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
    /// Ring of the last `spec.vol_window` returns, each rounded to 12 dp; allocated once.
    ret_ring: Vec<Decimal>,
    /// Ring of the squares of the returns in `ret_ring`, each rounded to 24 dp, so that the
    /// exact value added to `ret_sq_sum` is the value later subtracted from it.
    sq_ring: Vec<Decimal>,
    /// Index in `ret_ring` and `sq_ring` where the next return goes.
    ret_next: usize,
}

impl RollingStats {
    /// Allocates the window. Errors with `InvalidConfig` unless `lookback_days > skip_days`,
    /// `vol_window >= 2` and `lookback_days >= 1`.
    pub fn new(spec: WindowSpec) -> Result<Self, crate::EngineError> {
        if spec.lookback_days == 0 {
            return Err(crate::EngineError::InvalidConfig(
                "lookback_days must be at least 1".to_owned(),
            ));
        }
        if spec.lookback_days <= spec.skip_days {
            return Err(crate::EngineError::InvalidConfig(format!(
                "lookback_days ({}) must be greater than skip_days ({})",
                spec.lookback_days, spec.skip_days
            )));
        }
        if spec.vol_window < 2 {
            return Err(crate::EngineError::InvalidConfig(format!(
                "vol_window must be at least 2 (got {})",
                spec.vol_window
            )));
        }
        Ok(Self {
            spec,
            closes: vec![Decimal::ZERO; spec.capacity()],
            next: 0,
            history_days: 0,
            ret_sum: Decimal::ZERO,
            ret_sq_sum: Decimal::ZERO,
            ret_ring: vec![Decimal::ZERO; spec.vol_window],
            sq_ring: vec![Decimal::ZERO; spec.vol_window],
            ret_next: 0,
        })
    }

    /// Feeds the next session's close (must be positive; a non-positive close is
    /// `InvalidBar`, and the window is left unchanged). O(1), no allocation: the return that
    /// enters the variance window is added to the running sums and the one that leaves is
    /// subtracted.
    pub fn push(&mut self, close: Decimal) -> Result<(), crate::EngineError> {
        if close <= Decimal::ZERO {
            return Err(crate::EngineError::InvalidBar("close must be positive"));
        }
        let vol_window = self.ret_ring.len();
        let capacity = self.closes.len();

        // Everything is computed into locals first, so an error leaves `self` unchanged.
        let mut ret_sum = self.ret_sum;
        let mut ret_sq_sum = self.ret_sq_sum;
        let new_return = match self.close_back(0) {
            None => None,
            Some(prev) => {
                let ret = close
                    .checked_div(prev)
                    .and_then(|q| q.checked_sub(Decimal::ONE))
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?
                    .round_dp(12);
                let sq = ret
                    .checked_mul(ret)
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?
                    .round_dp(24);
                Some((ret, sq))
            }
        };
        if let Some((ret, sq)) = new_return {
            // The window is full once `vol_window` returns exist before this one; the slot
            // about to be overwritten then holds the return that leaves.
            if self.history_days > u64::try_from(vol_window).unwrap_or(u64::MAX) {
                let old_ret = self
                    .ret_ring
                    .get(self.ret_next)
                    .copied()
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
                let old_sq = self
                    .sq_ring
                    .get(self.ret_next)
                    .copied()
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
                ret_sum = ret_sum
                    .checked_sub(old_ret)
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
                ret_sq_sum = ret_sq_sum
                    .checked_sub(old_sq)
                    .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
            }
            ret_sum = ret_sum
                .checked_add(ret)
                .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
            ret_sq_sum = ret_sq_sum
                .checked_add(sq)
                .ok_or(crate::EngineError::InvalidBar(ARITHMETIC))?;
        }

        // Commit. The ring indices are below their lengths by construction.
        if let Some((ret, sq)) = new_return {
            if let Some(slot) = self.ret_ring.get_mut(self.ret_next) {
                *slot = ret;
            }
            if let Some(slot) = self.sq_ring.get_mut(self.ret_next) {
                *slot = sq;
            }
            self.ret_next = advance(self.ret_next, vol_window);
        }
        if let Some(slot) = self.closes.get_mut(self.next) {
            *slot = close;
        }
        self.next = advance(self.next, capacity);
        self.history_days = self.history_days.saturating_add(1);
        self.ret_sum = ret_sum;
        self.ret_sq_sum = ret_sq_sum;
        Ok(())
    }

    /// The current statistics; `None` before the first close.
    #[must_use]
    pub fn stats(&self) -> Option<Stats> {
        Some(Stats {
            history_days: self.history_days,
            close: self.close_back(0)?,
            momentum: self.momentum(),
            var_daily: self.var_daily(),
        })
    }

    /// The close `k` sessions before the latest one (`k = 0` is the latest), if retained and
    /// if that many sessions exist. Index arithmetic only.
    fn close_back(&self, k: usize) -> Option<Decimal> {
        let capacity = self.closes.len();
        if k >= capacity || u64::try_from(k).ok()? >= self.history_days {
            return None;
        }
        // `next` is the slot after the latest close; `k < capacity`, so this does not underflow.
        let slot = self
            .next
            .checked_add(capacity)?
            .checked_sub(k.checked_add(1)?)?
            .checked_rem(capacity)?;
        self.closes.get(slot).copied()
    }

    /// `close[t - skip] / close[t - lookback] - 1`.
    fn momentum(&self) -> Option<Decimal> {
        let base = self.close_back(self.spec.lookback_days)?;
        let recent = self.close_back(self.spec.skip_days)?;
        recent.checked_div(base)?.checked_sub(Decimal::ONE)
    }

    /// `sum(r^2) / n - (sum(r) / n)^2` over the `n = vol_window` returns in the window.
    fn var_daily(&self) -> Option<Decimal> {
        let vol_window = u64::try_from(self.ret_ring.len()).ok()?;
        // Needs `vol_window` returns, and the first return comes from the second close.
        if self.history_days <= vol_window {
            return None;
        }
        let n = Decimal::from(vol_window);
        let mean_sq = self.ret_sq_sum.checked_div(n)?;
        let mean = self.ret_sum.checked_div(n)?;
        let var = mean_sq.checked_sub(mean.checked_mul(mean)?)?;
        Some(if var < Decimal::ZERO {
            Decimal::ZERO
        } else {
            var
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests build decimals from literals"
)]
mod tests {
    use super::*;
    use crate::EngineError;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    const SMALL: WindowSpec = WindowSpec {
        lookback_days: 5,
        skip_days: 1,
        vol_window: 3,
    };

    const LARGE: WindowSpec = WindowSpec {
        lookback_days: 252,
        skip_days: 21,
        vol_window: 60,
    };

    /// Deterministic linear congruential generator; no `rand` crate.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
    }

    /// `n` closes starting at 100; each is the previous close times `1 + change`, with
    /// `change` uniform over -3%..=3% in whole basis points, rounded to 4 dp.
    fn lcg_closes(n: usize, seed: u64) -> Vec<Decimal> {
        let mut rng = Lcg(seed);
        let mut closes = Vec::with_capacity(n);
        let mut price = d("100");
        for i in 0..n {
            if i > 0 {
                let bps = (rng.next_u64() >> 33) % 601;
                let change = Decimal::new(bps as i64 - 300, 4);
                price = (price * (Decimal::ONE + change)).round_dp(4);
            }
            closes.push(price);
        }
        closes
    }

    /// Direct recomputation of `var_daily` from a plain slice of closes, with the same
    /// rounding rule as the incremental path: each return to 12 dp, each square to 24 dp.
    fn direct_var(closes: &[Decimal], vol: usize) -> Option<Decimal> {
        let n = closes.len();
        if n <= vol {
            return None;
        }
        let mut sum = Decimal::ZERO;
        let mut sq_sum = Decimal::ZERO;
        for pair in closes[n - vol - 1..].windows(2) {
            let (prev, cur) = (pair[0], pair[1]);
            let r = cur
                .checked_div(prev)?
                .checked_sub(Decimal::ONE)?
                .round_dp(12);
            let s = r.checked_mul(r)?.round_dp(24);
            sum = sum.checked_add(r)?;
            sq_sum = sq_sum.checked_add(s)?;
        }
        let n_dec = Decimal::from(vol as u64);
        let mean = sum.checked_div(n_dec)?;
        let var = sq_sum
            .checked_div(n_dec)?
            .checked_sub(mean.checked_mul(mean)?)?;
        Some(var.max(Decimal::ZERO))
    }

    /// Direct `close[t - skip] / close[t - lookback] - 1` from a plain slice of closes.
    fn direct_momentum(closes: &[Decimal], lookback: usize, skip: usize) -> Option<Decimal> {
        let n = closes.len();
        if n <= lookback {
            return None;
        }
        let t = n - 1;
        closes[t - skip]
            .checked_div(closes[t - lookback])?
            .checked_sub(Decimal::ONE)
    }

    #[test]
    fn new_rejects_invalid_specs_and_accepts_valid() {
        let zero_lookback = RollingStats::new(WindowSpec {
            lookback_days: 0,
            skip_days: 0,
            vol_window: 3,
        });
        assert!(matches!(zero_lookback, Err(EngineError::InvalidConfig(_))));

        let lookback_equals_skip = RollingStats::new(WindowSpec {
            lookback_days: 5,
            skip_days: 5,
            vol_window: 3,
        });
        assert!(matches!(
            lookback_equals_skip,
            Err(EngineError::InvalidConfig(_))
        ));

        let lookback_below_skip = RollingStats::new(WindowSpec {
            lookback_days: 3,
            skip_days: 4,
            vol_window: 3,
        });
        assert!(matches!(
            lookback_below_skip,
            Err(EngineError::InvalidConfig(_))
        ));

        let vol_too_small = RollingStats::new(WindowSpec {
            lookback_days: 5,
            skip_days: 1,
            vol_window: 1,
        });
        assert!(matches!(vol_too_small, Err(EngineError::InvalidConfig(_))));

        assert!(RollingStats::new(SMALL).is_ok());
        // Skip of zero is valid: momentum then ends at the latest close.
        assert!(
            RollingStats::new(WindowSpec {
                lookback_days: 1,
                skip_days: 0,
                vol_window: 2,
            })
            .is_ok()
        );
    }

    #[test]
    fn stats_is_none_before_first_close_and_partial_after_one() {
        let mut rs = RollingStats::new(LARGE).unwrap();
        assert_eq!(rs.stats(), None);
        rs.push(d("100")).unwrap();
        let stats = rs.stats().unwrap();
        assert_eq!(stats.history_days, 1);
        assert_eq!(stats.close, d("100"));
        assert_eq!(stats.momentum, None);
        assert_eq!(stats.var_daily, None);
    }

    #[test]
    fn momentum_matches_hand_computed_values() {
        let mut rs = RollingStats::new(SMALL).unwrap();
        let closes = ["100", "101", "102", "103", "104", "105", "106"].map(d);
        for (i, close) in closes.iter().enumerate() {
            rs.push(*close).unwrap();
            let stats = rs.stats().unwrap();
            assert_eq!(stats.history_days, (i + 1) as u64);
            if i < 5 {
                assert_eq!(stats.momentum, None, "push {}", i + 1);
            }
        }
        // Recheck the two named points with the sixth and seventh pushes.
        let mut rs = RollingStats::new(SMALL).unwrap();
        for close in &closes[..5] {
            rs.push(*close).unwrap();
        }
        assert_eq!(rs.stats().unwrap().momentum, None);
        rs.push(closes[5]).unwrap();
        // close[4] / close[0] - 1 = 104 / 100 - 1
        assert_eq!(rs.stats().unwrap().momentum, Some(d("0.04")));
        rs.push(closes[6]).unwrap();
        // close[5] / close[1] - 1 = 105 / 101 - 1
        assert_eq!(
            rs.stats().unwrap().momentum,
            Some(d("105").checked_div(d("101")).unwrap() - Decimal::ONE)
        );
    }

    #[test]
    fn momentum_stays_exact_after_ring_wraps() {
        // Capacity 6 for SMALL, so 50 pushes wrap the ring several times.
        let closes = lcg_closes(50, 7);
        let mut rs = RollingStats::new(SMALL).unwrap();
        for (i, close) in closes.iter().enumerate() {
            rs.push(*close).unwrap();
            assert_eq!(
                rs.stats().unwrap().momentum,
                direct_momentum(&closes[..=i], 5, 1),
                "small spec, push {}",
                i + 1
            );
        }
        // Capacity 253 for LARGE, so 700 pushes wrap it about twice.
        let closes = lcg_closes(700, 99);
        let mut rs = RollingStats::new(LARGE).unwrap();
        for (i, close) in closes.iter().enumerate() {
            rs.push(*close).unwrap();
            assert_eq!(
                rs.stats().unwrap().momentum,
                direct_momentum(&closes[..=i], 252, 21),
                "large spec, push {}",
                i + 1
            );
        }
    }

    #[test]
    fn variance_appears_exactly_when_vol_window_returns_exist() {
        let closes = lcg_closes(40, 3);
        let mut rs = RollingStats::new(SMALL).unwrap();
        for (i, close) in closes.iter().enumerate() {
            rs.push(*close).unwrap();
            let stats = rs.stats().unwrap();
            // Returns exist once there are vol_window + 1 closes.
            let expected = direct_var(&closes[..=i], 3);
            assert_eq!(stats.var_daily, expected, "push {}", i + 1);
            assert_eq!(stats.var_daily.is_some(), i + 1 >= 4, "push {}", i + 1);
        }
    }

    #[test]
    fn incremental_variance_equals_direct_recomputation_over_2000_steps() {
        let closes = lcg_closes(2000, 42);
        let mut rs = RollingStats::new(LARGE).unwrap();
        let mut with_var = 0_usize;
        for (i, &close) in closes.iter().enumerate() {
            rs.push(close).unwrap();
            let so_far = &closes[..=i];
            let stats = rs.stats().unwrap();
            assert_eq!(stats.history_days, (i + 1) as u64);
            assert_eq!(stats.close, close);
            assert_eq!(
                stats.var_daily,
                direct_var(so_far, 60),
                "variance after push {}",
                i + 1
            );
            assert_eq!(
                stats.momentum,
                direct_momentum(so_far, 252, 21),
                "momentum after push {}",
                i + 1
            );
            if stats.var_daily.is_some() {
                with_var += 1;
            }
        }
        assert_eq!(with_var, 2000 - 60);
    }

    #[test]
    fn constant_series_has_zero_variance_and_zero_momentum() {
        let mut rs = RollingStats::new(LARGE).unwrap();
        for _ in 0..300 {
            rs.push(d("123.45")).unwrap();
        }
        let stats = rs.stats().unwrap();
        assert_eq!(stats.var_daily, Some(Decimal::ZERO));
        assert_eq!(stats.momentum, Some(Decimal::ZERO));

        let mut small = RollingStats::new(SMALL).unwrap();
        for _ in 0..10 {
            small.push(d("7")).unwrap();
        }
        let stats = small.stats().unwrap();
        assert_eq!(stats.var_daily, Some(Decimal::ZERO));
        assert_eq!(stats.momentum, Some(Decimal::ZERO));
    }

    #[test]
    fn non_positive_close_is_rejected_and_leaves_stats_unchanged() {
        let closes = lcg_closes(20, 5);
        let mut rs = RollingStats::new(SMALL).unwrap();
        let mut reference = RollingStats::new(SMALL).unwrap();
        for close in &closes[..10] {
            rs.push(*close).unwrap();
            reference.push(*close).unwrap();
        }
        let before = rs.stats();
        for bad in [Decimal::ZERO, d("-1")] {
            let err = rs.push(bad).unwrap_err();
            assert!(matches!(
                err,
                EngineError::InvalidBar("close must be positive")
            ));
            assert_eq!(rs.stats(), before);
        }
        // Later valid bars still match a window that never saw the bad bars.
        for close in &closes[10..] {
            rs.push(*close).unwrap();
            reference.push(*close).unwrap();
            assert_eq!(rs.stats(), reference.stats());
        }
    }

    #[test]
    fn overflowing_return_is_an_error_and_leaves_stats_unchanged() {
        let mut rs = RollingStats::new(SMALL).unwrap();
        rs.push(d("1")).unwrap();
        let before = rs.stats();
        // The return is about 7.9e28, and its square overflows Decimal.
        let err = rs.push(Decimal::MAX).unwrap_err();
        assert!(matches!(err, EngineError::InvalidBar(_)));
        assert_eq!(rs.stats(), before);
    }

    #[test]
    fn variance_is_never_negative() {
        let closes = lcg_closes(2000, 2024);
        let mut rs = RollingStats::new(LARGE).unwrap();
        for close in &closes {
            rs.push(*close).unwrap();
            let var = rs.stats().unwrap().var_daily;
            assert!(var.is_none_or(|v| v >= Decimal::ZERO));
        }
        // Near-flat series, where rounding is most likely to push the result below zero.
        let mut rs = RollingStats::new(SMALL).unwrap();
        for i in 0..500 {
            let close = if i % 2 == 0 {
                d("100.0000")
            } else {
                d("100.0001")
            };
            rs.push(close).unwrap();
            let var = rs.stats().unwrap().var_daily;
            assert!(var.is_none_or(|v| v >= Decimal::ZERO));
        }
    }
}
