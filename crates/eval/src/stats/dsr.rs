//! Probabilistic and deflated Sharpe ratios (Bailey and López de Prado, 2014).

/// The Euler-Mascheroni constant.
pub const EULER_GAMMA: f64 = std::f64::consts::EULER_GAMMA;

/// The probability that the true per-period Sharpe exceeds `sr_star`, given an observed
/// per-period Sharpe `sr` over `n` returns with the given skewness and (non-excess)
/// kurtosis. `None` when `n < 2`, an input is not finite, or the radicand
/// `1 - skew·sr + (kurt - 1)/4·sr²` is not positive.
#[allow(unused_variables, reason = "stub: Phase 5 task ST5")]
#[must_use]
pub fn psr(sr: f64, sr_star: f64, n: usize, skew: f64, kurt: f64) -> Option<f64> {
    todo!("Phase 5 task ST5")
}

/// The per-period Sharpe the best of `n_trials` unskilled trials is expected to show, given
/// the variance of the trials' per-period Sharpes. Zero for one trial; `None` for no trial
/// or a negative or non-finite variance.
#[allow(unused_variables, reason = "stub: Phase 5 task ST5")]
#[must_use]
pub fn expected_max_sharpe(variance_of_trial_sharpes: f64, n_trials: u64) -> Option<f64> {
    todo!("Phase 5 task ST5")
}

/// The deflated Sharpe ratio of `returns`: [`psr`] against [`expected_max_sharpe`].
/// `trial_sharpes` are the per-period Sharpes of the trials with a stored result and
/// `n_trials` the lifetime trial count. `None` when any ingredient is `None`, or when
/// `n_trials >= 2` and fewer than two trial Sharpes are given (their variance is unknown).
#[allow(unused_variables, reason = "stub: Phase 5 task ST5")]
#[must_use]
pub fn deflated_sharpe(returns: &[f64], trial_sharpes: &[f64], n_trials: u64) -> Option<f64> {
    todo!("Phase 5 task ST5")
}
