//! Moments of a return series and the ratios built on them.

/// Number of values as `f64`. Series lengths are far below 2^53, so the conversion is exact.
#[allow(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
fn count(returns: &[f64]) -> f64 {
    returns.len() as f64
}

/// `Some(x)` only when `x` is finite.
fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// One pass: the sum of the values and whether every value equals the first one.
/// `None` for an empty slice or when any value is not finite.
fn sum_and_constancy(returns: &[f64]) -> Option<(f64, bool)> {
    let first = *returns.first()?;
    let mut sum = 0.0;
    let mut constant = true;
    for &r in returns {
        if !r.is_finite() {
            return None;
        }
        sum += r;
        constant &= r == first;
    }
    Some((sum, constant))
}

/// Two passes: the mean and the sample variance (`Σ(r - mean)² / (n - 1)`). Constant data has
/// variance exactly 0 and mean `Σr / n`. `None` when `n < 2` or any value is not finite.
fn mean_and_sample_variance(returns: &[f64]) -> Option<(f64, f64)> {
    if returns.len() < 2 {
        return None;
    }
    let (sum, constant) = sum_and_constancy(returns)?;
    let n = count(returns);
    let mean = finite(sum / n)?;
    if constant {
        return Some((mean, 0.0));
    }
    let mut ss = 0.0;
    for &r in returns {
        let d = r - mean;
        ss += d * d;
    }
    Some((mean, finite(ss / (n - 1.0))?))
}

/// Two passes: population central moments `(m2, m3, m4)`. `None` when `n < 3`, when the data
/// is constant (so `m2` is exactly zero), or when any value is not finite.
fn central_moments(returns: &[f64]) -> Option<(f64, f64, f64)> {
    if returns.len() < 3 {
        return None;
    }
    let (sum, constant) = sum_and_constancy(returns)?;
    if constant {
        return None;
    }
    let n = count(returns);
    let mean = sum / n;
    let (mut s2, mut s3, mut s4) = (0.0, 0.0, 0.0);
    for &r in returns {
        let d = r - mean;
        let d2 = d * d;
        s2 += d2;
        s3 += d2 * d;
        s4 += d2 * d2;
    }
    let m2 = s2 / n;
    if m2 <= 0.0 {
        return None;
    }
    Some((m2, s3 / n, s4 / n))
}

/// `Σr / n`. `None` for an empty slice or a non-finite result.
#[must_use]
pub fn mean(returns: &[f64]) -> Option<f64> {
    let (sum, _) = sum_and_constancy(returns)?;
    finite(sum / count(returns))
}

/// Sample variance (divide by `n - 1`). `None` when `n < 2`.
#[must_use]
pub fn variance(returns: &[f64]) -> Option<f64> {
    mean_and_sample_variance(returns).map(|(_, var)| var)
}

/// `m3 / m2^1.5` with population central moments. `None` when `n < 3` or `m2` is zero.
#[must_use]
pub fn skewness(returns: &[f64]) -> Option<f64> {
    let (m2, m3, _) = central_moments(returns)?;
    finite(m3 / (m2 * m2.sqrt()))
}

/// `m4 / m2²` with population central moments (not excess: about 3 for a normal sample).
/// `None` when `n < 3` or `m2` is zero.
#[must_use]
pub fn kurtosis(returns: &[f64]) -> Option<f64> {
    let (m2, _, m4) = central_moments(returns)?;
    finite(m4 / (m2 * m2))
}

/// Per-period Sharpe ratio: `mean / sqrt(variance)`. `None` when `n < 2` or the variance is
/// zero.
#[must_use]
pub fn sharpe(returns: &[f64]) -> Option<f64> {
    let (mean, var) = mean_and_sample_variance(returns)?;
    if var <= 0.0 {
        return None;
    }
    finite(mean / var.sqrt())
}

/// Per-period Sortino ratio: `mean / sqrt(Σ min(r, 0)² / n)`. `None` for an empty slice or
/// when no return is negative.
#[must_use]
pub fn sortino(returns: &[f64]) -> Option<f64> {
    if returns.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    let mut downside = 0.0;
    for &r in returns {
        if !r.is_finite() {
            return None;
        }
        sum += r;
        if r < 0.0 {
            downside += r * r;
        }
    }
    if downside <= 0.0 {
        return None;
    }
    let n = count(returns);
    finite((sum / n) / (downside / n).sqrt())
}

/// The largest peak-to-trough fall of the compounded curve `Π(1 + r)`, starting from 1, as
/// a positive fraction (0.25 is a 25% drawdown; 0 when the curve never falls). `None` for an
/// empty slice or a return at or below -1.
#[must_use]
pub fn max_drawdown(returns: &[f64]) -> Option<f64> {
    if returns.is_empty() {
        return None;
    }
    let mut curve = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut worst = 0.0_f64;
    for &r in returns {
        if !r.is_finite() || r <= -1.0 {
            return None;
        }
        curve *= 1.0 + r;
        if !curve.is_finite() {
            return None;
        }
        if curve > peak {
            peak = curve;
        } else {
            // peak >= 1 always, so the division is safe.
            worst = worst.max(1.0 - curve / peak);
        }
    }
    finite(worst)
}

/// A per-period ratio (Sharpe, Sortino) annualised: `ratio × sqrt(periods_per_year)`.
#[must_use]
pub fn annualise(ratio: f64, periods_per_year: u32) -> f64 {
    ratio * f64::from(periods_per_year).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts that `actual` is `Some` and within `tol` of `expected`. A `None` fails.
    fn assert_close(actual: Option<f64>, expected: f64, tol: f64) {
        let value = actual.unwrap_or(f64::NAN);
        assert!(
            (value - expected).abs() <= tol,
            "got {actual:?}, expected {expected} (tol {tol})"
        );
    }

    /// Data A: the returns from the acceptance criteria.
    const A: [f64; 5] = [0.01, -0.02, 0.03, 0.00, 0.02];
    /// Data B: plain numbers for the moment checks.
    const B: [f64; 5] = [1.0, 2.0, 3.0, 4.0, 10.0];

    #[test]
    fn mean_of_data_a() {
        // (0.01 - 0.02 + 0.03 + 0 + 0.02) / 5 = 0.04 / 5 = 0.008
        assert_close(mean(&A), 0.008, 1e-12);
    }

    #[test]
    fn mean_of_empty_is_none() {
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn mean_of_single_value_is_that_value() {
        assert_close(mean(&[0.25]), 0.25, 1e-12);
    }

    #[test]
    fn variance_of_data_a() {
        // deviations from 0.008: 0.002, -0.028, 0.022, -0.008, 0.012
        // squares sum: 4e-6 + 7.84e-4 + 4.84e-4 + 6.4e-5 + 1.44e-4 = 0.00148
        // / (5 - 1) = 0.00037
        assert_close(variance(&A), 0.00037, 1e-12);
    }

    #[test]
    fn variance_of_single_value_is_none() {
        assert_eq!(variance(&[0.1]), None);
    }

    #[test]
    fn variance_of_constant_series_is_zero() {
        assert_close(variance(&[0.1, 0.1, 0.1]), 0.0, 0.0);
    }

    #[test]
    fn sharpe_of_data_a_matches_formula_and_literal() {
        // 0.008 / sqrt(0.00037)
        let expected = 0.008 / 0.00037_f64.sqrt();
        assert_close(sharpe(&A), expected, 1e-12);
        assert_close(sharpe(&A), 0.415_900_14, 1e-7);
    }

    #[test]
    fn sortino_of_data_a() {
        // downside squares: 0.02^2 = 0.0004; / 5 = 0.00008; sqrt = 0.0089442719
        // 0.008 / 0.0089442719 = 0.894427191
        assert_close(sortino(&A), 0.894_427_191, 1e-8);
    }

    #[test]
    fn sortino_of_all_non_negative_is_none() {
        assert_eq!(sortino(&[0.0, 0.01, 0.02]), None);
    }

    #[test]
    fn sortino_of_empty_is_none() {
        assert_eq!(sortino(&[]), None);
    }

    #[test]
    fn skewness_of_data_b() {
        // mean 4; m2 = 10; m3 = (-27 - 8 - 1 + 0 + 216) / 5 = 36
        // skewness = 36 / 10^1.5 = 1.1384199577
        assert_close(skewness(&B), 1.138_419_957_7, 1e-9);
    }

    #[test]
    fn kurtosis_of_data_b() {
        // m4 = (81 + 16 + 1 + 0 + 1296) / 5 = 278.8; kurtosis = 278.8 / 100 = 2.788
        assert_close(kurtosis(&B), 2.788, 1e-12);
    }

    #[test]
    fn symmetric_sample_has_zero_skewness_and_kurtosis_1_7() {
        // m3 = 0; m2 = 10 / 5 = 2; m4 = (16 + 1 + 0 + 1 + 16) / 5 = 6.8; kurtosis = 6.8 / 4 = 1.7
        let sample = [-2.0, -1.0, 0.0, 1.0, 2.0];
        assert_close(skewness(&sample), 0.0, 1e-15);
        assert_close(kurtosis(&sample), 1.7, 1e-12);
    }

    #[test]
    fn skewness_and_kurtosis_need_three_values() {
        assert_eq!(skewness(&[1.0, 2.0]), None);
        assert_eq!(kurtosis(&[1.0, 2.0]), None);
    }

    #[test]
    fn skewness_and_kurtosis_of_constant_series_are_none() {
        assert_eq!(skewness(&[0.1, 0.1, 0.1, 0.1]), None);
        assert_eq!(kurtosis(&[0.1, 0.1, 0.1, 0.1]), None);
    }

    #[test]
    fn sharpe_of_constant_series_is_none() {
        assert_eq!(sharpe(&[0.1, 0.1, 0.1]), None);
    }

    #[test]
    fn sharpe_of_single_value_is_none() {
        assert_eq!(sharpe(&[0.1]), None);
    }

    #[test]
    fn non_finite_input_gives_none_everywhere() {
        let bad_slices: [&[f64]; 4] = [
            &[0.01, f64::NAN, 0.02, 0.03],
            &[0.01, f64::INFINITY, 0.02, 0.03],
            &[0.01, f64::NEG_INFINITY, 0.02, 0.03],
            &[f64::NAN],
        ];
        for bad in bad_slices {
            assert_eq!(mean(bad), None);
            assert_eq!(variance(bad), None);
            assert_eq!(skewness(bad), None);
            assert_eq!(kurtosis(bad), None);
            assert_eq!(sharpe(bad), None);
            assert_eq!(sortino(bad), None);
            assert_eq!(max_drawdown(bad), None);
        }
    }

    #[test]
    fn max_drawdown_of_mixed_returns() {
        // curve 1.1 (peak), 0.55 (dd 0.5), 0.66 (dd 0.4)
        assert_close(max_drawdown(&[0.1, -0.5, 0.2]), 0.5, 1e-12);
    }

    #[test]
    fn max_drawdown_of_rising_series_is_zero() {
        assert_close(max_drawdown(&[0.01, 0.02, 0.03]), 0.0, 0.0);
    }

    #[test]
    fn max_drawdown_of_two_losses_compounds() {
        // curve 0.9, 0.81; dd = 1 - 0.81 = 0.19
        assert_close(max_drawdown(&[-0.1, -0.1]), 0.19, 1e-12);
    }

    #[test]
    fn max_drawdown_counts_fall_from_initial_value() {
        // curve 0.8 (dd 0.2 from the initial 1), then 1.2 (new peak)
        assert_close(max_drawdown(&[-0.2, 0.5]), 0.2, 1e-12);
    }

    #[test]
    fn max_drawdown_of_empty_is_none() {
        assert_eq!(max_drawdown(&[]), None);
    }

    #[test]
    fn max_drawdown_with_return_at_or_below_minus_one_is_none() {
        assert_eq!(max_drawdown(&[0.1, -1.0]), None);
        assert_eq!(max_drawdown(&[0.1, -1.5]), None);
    }

    #[test]
    fn annualise_scales_by_sqrt_of_periods() {
        // 0.1 * sqrt(252) = 1.5874507866
        assert_close(Some(annualise(0.1, 252)), 1.587_450_786_6, 1e-9);
    }

    #[test]
    fn scaling_returns_by_three_leaves_ratios_unchanged() {
        let scaled = A.map(|r| r * 3.0);
        assert_close(sharpe(&scaled), sharpe(&A).unwrap_or(f64::NAN), 1e-12);
        assert_close(sortino(&scaled), sortino(&A).unwrap_or(f64::NAN), 1e-12);
        assert_close(skewness(&scaled), skewness(&A).unwrap_or(f64::NAN), 1e-12);
        assert_close(kurtosis(&scaled), kurtosis(&A).unwrap_or(f64::NAN), 1e-12);
    }

    #[test]
    fn scaling_returns_by_three_multiplies_variance_by_nine() {
        let scaled = A.map(|r| r * 3.0);
        assert_close(variance(&scaled), 9.0 * 0.00037, 1e-12);
    }

    #[test]
    fn long_alternating_series_is_finite_with_zero_mean_and_sharpe() {
        let mut series = vec![0.0_f64; 10_000];
        for (i, r) in series.iter_mut().enumerate() {
            *r = if i % 2 == 0 { 0.01 } else { -0.01 };
        }
        assert_close(mean(&series), 0.0, 0.0);
        assert_close(sharpe(&series), 0.0, 0.0);
        for value in [
            variance(&series),
            skewness(&series),
            kurtosis(&series),
            sortino(&series),
            max_drawdown(&series),
        ] {
            assert!(value.is_some_and(f64::is_finite));
        }
    }
}
