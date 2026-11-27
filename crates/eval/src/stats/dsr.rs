//! Probabilistic and deflated Sharpe ratios (Bailey and López de Prado, 2014).

use super::moments::{kurtosis, sharpe, skewness, variance};
use super::normal::{cdf, inv_cdf};

/// The Euler-Mascheroni constant.
pub const EULER_GAMMA: f64 = std::f64::consts::EULER_GAMMA;

/// Number of observations as `f64`. Counts are far below 2^53, so the conversion is exact.
#[allow(clippy::cast_precision_loss, reason = "counts are far below 2^53")]
fn len_as_f64(n: usize) -> f64 {
    n as f64
}

/// A trial count as `f64`. Counts are far below 2^53, so the conversion is exact.
#[allow(clippy::cast_precision_loss, reason = "counts are far below 2^53")]
fn count_as_f64(n: u64) -> f64 {
    n as f64
}

/// The probability that the true per-period Sharpe exceeds `sr_star`, given an observed
/// per-period Sharpe `sr` over `n` returns with the given skewness and (non-excess)
/// kurtosis. `None` when `n < 2`, an input is not finite, or the radicand
/// `1 - skew·sr + (kurt - 1)/4·sr²` is not positive.
#[must_use]
pub fn psr(sr: f64, sr_star: f64, n: usize, skew: f64, kurt: f64) -> Option<f64> {
    if n < 2 || ![sr, sr_star, skew, kurt].iter().all(|x| x.is_finite()) {
        return None;
    }
    let radicand = 1.0 - skew * sr + (kurt - 1.0) / 4.0 * sr * sr;
    if !radicand.is_finite() || radicand <= 0.0 {
        return None;
    }
    let z = (sr - sr_star) * (len_as_f64(n) - 1.0).sqrt() / radicand.sqrt();
    if z.is_nan() {
        return None;
    }
    Some(cdf(z))
}

/// The per-period Sharpe the best of `n_trials` unskilled trials is expected to show, given
/// the variance of the trials' per-period Sharpes. Zero for one trial; `None` for no trial
/// or a negative or non-finite variance.
#[must_use]
pub fn expected_max_sharpe(variance_of_trial_sharpes: f64, n_trials: u64) -> Option<f64> {
    if n_trials == 0 || !variance_of_trial_sharpes.is_finite() || variance_of_trial_sharpes < 0.0 {
        return None;
    }
    if n_trials == 1 {
        return Some(0.0);
    }
    let big_n = count_as_f64(n_trials);
    let e = std::f64::consts::E;
    let upper = inv_cdf(1.0 - 1.0 / big_n)?;
    let lower = inv_cdf(1.0 - 1.0 / (big_n * e))?;
    let value =
        variance_of_trial_sharpes.sqrt() * ((1.0 - EULER_GAMMA) * upper + EULER_GAMMA * lower);
    value.is_finite().then_some(value)
}

/// The deflated Sharpe ratio of `returns`: [`psr`] against [`expected_max_sharpe`].
/// `trial_sharpes` are the per-period Sharpes of the trials with a stored result and
/// `n_trials` the lifetime trial count. `None` when any ingredient is `None`, or when
/// `n_trials >= 2` and fewer than two trial Sharpes are given (their variance is unknown).
#[must_use]
pub fn deflated_sharpe(returns: &[f64], trial_sharpes: &[f64], n_trials: u64) -> Option<f64> {
    let sr = sharpe(returns)?;
    let skew = skewness(returns)?;
    let kurt = kurtosis(returns)?;
    if n_trials == 0 {
        return None;
    }
    if n_trials == 1 {
        return psr(sr, 0.0, returns.len(), skew, kurt);
    }
    let var = variance(trial_sharpes)?;
    let sr0 = expected_max_sharpe(var, n_trials)?;
    psr(sr, sr0, returns.len(), skew, kurt)
}

#[cfg(test)]
mod tests {
    use super::{EULER_GAMMA, deflated_sharpe, expected_max_sharpe, psr};
    use crate::stats::moments::{kurtosis, sharpe, skewness};
    use crate::stats::normal::{cdf, inv_cdf};

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// `alternating(500, 0.011, -0.009)` gives 250 of each value: mean 0.001.
    fn alternating(len: usize, up: f64, down: f64) -> Vec<f64> {
        (0..len)
            .map(|i| if i % 2 == 0 { up } else { down })
            .collect()
    }

    #[test]
    fn psr_normal_shaped_sample_matches_formula_and_range() {
        // radicand = 1 - 0 + (3 - 1)/4 * 0.01 = 1.005; sqrt(101 - 1) = 10
        // z = 0.1 * 10 / sqrt(1.005) = 0.997509...
        let z = 0.1 * 10.0 / 1.005_f64.sqrt();
        let expected = cdf(z);
        let got = psr(0.1, 0.0, 101, 0.0, 3.0).expect("valid inputs");
        assert!(
            close(got, expected, 1e-12),
            "got {got}, expected {expected}"
        );
        assert!(close(got, 0.840_693, 1e-4), "got {got}");
        assert!(got > 0.8400 && got < 0.8415, "got {got}");
    }

    #[test]
    fn psr_at_threshold_is_one_half() {
        // z = 0, so the result is cdf(0), which is 0.5 up to the approximation error.
        for sr in [-0.3, 0.0, 0.05, 0.1, 0.7] {
            let got = psr(sr, sr, 50, 0.0, 3.0).expect("valid inputs");
            assert!(close(got, 0.5, 1e-9), "sr = {sr}, got {got}");
        }
    }

    #[test]
    fn psr_rises_with_sharpe() {
        let values: Vec<f64> = [0.0, 0.05, 0.1]
            .iter()
            .map(|&sr| psr(sr, 0.0, 101, 0.0, 3.0).expect("valid inputs"))
            .collect();
        assert!(values[0] < values[1] && values[1] < values[2], "{values:?}");
    }

    #[test]
    fn psr_falls_with_threshold() {
        let values: Vec<f64> = [-0.05, 0.0, 0.05]
            .iter()
            .map(|&sr_star| psr(0.1, sr_star, 101, 0.0, 3.0).expect("valid inputs"))
            .collect();
        assert!(values[0] > values[1] && values[1] > values[2], "{values:?}");
    }

    #[test]
    fn psr_rises_with_sample_size_when_sharpe_exceeds_threshold() {
        let values: Vec<f64> = [11, 51, 201]
            .iter()
            .map(|&n| psr(0.1, 0.0, n, 0.0, 3.0).expect("valid inputs"))
            .collect();
        assert!(values[0] < values[1] && values[1] < values[2], "{values:?}");
    }

    #[test]
    fn negative_skew_and_fat_tails_lower_confidence() {
        // skew: radicand 1 + 0.1 + 0.005 = 1.105 > 1.005, so z is smaller.
        assert!(
            psr(0.1, 0.0, 101, -1.0, 3.0).expect("valid")
                < psr(0.1, 0.0, 101, 0.0, 3.0).expect("valid")
        );
        // kurtosis: radicand 1 + (6 - 1)/4 * 0.01 = 1.0125 > 1.005, so z is smaller.
        assert!(
            psr(0.1, 0.0, 101, 0.0, 6.0).expect("valid")
                < psr(0.1, 0.0, 101, 0.0, 3.0).expect("valid")
        );
    }

    #[test]
    fn psr_none_cases() {
        assert_eq!(psr(0.1, 0.0, 0, 0.0, 3.0), None);
        assert_eq!(psr(0.1, 0.0, 1, 0.0, 3.0), None);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(psr(bad, 0.0, 101, 0.0, 3.0), None, "sr = {bad}");
            assert_eq!(psr(0.1, bad, 101, 0.0, 3.0), None, "sr_star = {bad}");
            assert_eq!(psr(0.1, 0.0, 101, bad, 3.0), None, "skew = {bad}");
            assert_eq!(psr(0.1, 0.0, 101, 0.0, bad), None, "kurt = {bad}");
        }
        // radicand = 1 - 2 * 1 + 0 = -1
        assert_eq!(psr(2.0, 0.0, 101, 1.0, 1.0), None);
        // radicand = 0 exactly: 1 - 1 * 1 + 0 = 0
        assert_eq!(psr(1.0, 0.0, 101, 1.0, 1.0), None);
    }

    #[test]
    fn expected_max_none_cases() {
        assert_eq!(expected_max_sharpe(0.01, 0), None);
        assert_eq!(expected_max_sharpe(-0.01, 10), None);
        assert_eq!(expected_max_sharpe(f64::NAN, 10), None);
        assert_eq!(expected_max_sharpe(f64::INFINITY, 10), None);
    }

    #[test]
    fn expected_max_one_trial_is_exactly_zero() {
        assert_eq!(expected_max_sharpe(0.01, 1), Some(0.0));
        assert_eq!(expected_max_sharpe(0.0, 1), Some(0.0));
    }

    #[test]
    fn expected_max_zero_variance_is_zero() {
        assert_eq!(expected_max_sharpe(0.0, 10), Some(0.0));
    }

    #[test]
    fn expected_max_matches_formula_and_range() {
        let e = std::f64::consts::E;
        let n = 10.0_f64;
        let formula = 0.1
            * ((1.0 - EULER_GAMMA) * inv_cdf(1.0 - 1.0 / n).expect("in range")
                + EULER_GAMMA * inv_cdf(1.0 - 1.0 / (n * e)).expect("in range"));
        let got = expected_max_sharpe(0.01, 10).expect("valid inputs");
        assert!(close(got, formula, 1e-12), "got {got}, formula {formula}");
        assert!(got > 0.155 && got < 0.160, "got {got}");
    }

    #[test]
    fn expected_max_grows_with_trial_count() {
        let values: Vec<f64> = [2, 10, 100, 1000]
            .iter()
            .map(|&n| expected_max_sharpe(0.01, n).expect("valid inputs"))
            .collect();
        assert!(values.windows(2).all(|w| w[0] < w[1]), "{values:?}");
    }

    #[test]
    fn expected_max_scales_with_sqrt_variance() {
        let base = expected_max_sharpe(0.01, 10).expect("valid inputs");
        let quad = expected_max_sharpe(0.04, 10).expect("valid inputs");
        assert!(close(quad, 2.0 * base, 1e-12), "base {base}, quad {quad}");
    }

    #[test]
    fn deflated_one_trial_equals_psr_on_moments() {
        let returns = [0.01, -0.02, 0.03, 0.00, 0.02, 0.015, -0.005];
        let sr = sharpe(&returns).expect("non-constant");
        let skew = skewness(&returns).expect("n >= 3");
        let kurt = kurtosis(&returns).expect("n >= 3");
        let by_hand = psr(sr, 0.0, returns.len(), skew, kurt).expect("valid");
        let got = deflated_sharpe(&returns, &[], 1).expect("one trial needs no trial Sharpes");
        assert!(close(got, by_hand, 1e-12), "got {got}, by hand {by_hand}");
    }

    #[test]
    fn deflated_is_strictly_lower_with_many_trials() {
        let returns = alternating(500, 0.011, -0.009);
        let trials = [0.02, 0.05, -0.01, 0.08];
        let one = deflated_sharpe(&returns, &trials, 1).expect("valid");
        let four = deflated_sharpe(&returns, &trials, 4).expect("valid");
        let fifty = deflated_sharpe(&returns, &trials, 50).expect("valid");
        assert!(
            one > four && four > fifty,
            "one {one}, four {four}, fifty {fifty}"
        );
    }

    #[test]
    fn deflated_none_cases() {
        let returns = alternating(500, 0.011, -0.009);
        let trials = [0.02, 0.05, -0.01, 0.08];
        assert_eq!(deflated_sharpe(&returns, &trials, 0), None);
        assert_eq!(deflated_sharpe(&returns, &[], 2), None);
        assert_eq!(deflated_sharpe(&returns, &[0.02], 5), None);
        assert_eq!(deflated_sharpe(&[0.01; 10], &trials, 4), None);
        assert_eq!(deflated_sharpe(&[0.01, -0.02], &trials, 4), None);
        assert_eq!(deflated_sharpe(&[], &trials, 1), None);
    }

    #[test]
    fn deflated_result_is_within_unit_interval() {
        let returns = alternating(500, 0.011, -0.009);
        let trials = [0.02, 0.05, -0.01, 0.08];
        for n in [1, 2, 4, 50, 10_000] {
            let v = deflated_sharpe(&returns, &trials, n).expect("valid");
            assert!((0.0..=1.0).contains(&v), "n = {n}, v = {v}");
        }
    }

    #[test]
    fn deflated_synthetic_positive_drift_is_confident() {
        // mean 0.001, sample sd about 0.01, sr about 0.0999; sqrt(499) * sr = about 2.23,
        // skew 0 and kurtosis 1 give radicand 1, so psr = cdf(2.23) above 0.95.
        let returns = alternating(500, 0.011, -0.009);
        let got = deflated_sharpe(&returns, &[], 1).expect("valid");
        assert!(got > 0.95, "got {got}");
    }

    #[test]
    fn deflated_zero_mean_synthetic_is_one_half() {
        // mean 0 exactly, so sr = 0 and psr = cdf(0), which is 0.5 up to the approximation.
        let returns = alternating(500, 0.01, -0.01);
        let got = deflated_sharpe(&returns, &[], 1).expect("valid");
        assert!(close(got, 0.5, 1e-6), "got {got}");
    }

    #[test]
    fn more_trials_never_raise_the_result() {
        let returns = alternating(500, 0.011, -0.009);
        let trials = [0.02, 0.05, -0.01, 0.08];
        let mut prev = f64::INFINITY;
        for n in [2, 5, 20, 100] {
            let v = deflated_sharpe(&returns, &trials, n).expect("valid");
            assert!(v <= prev, "n = {n}, v = {v}, prev = {prev}");
            prev = v;
        }
    }
}
