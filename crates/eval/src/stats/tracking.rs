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
#[must_use]
pub fn tracking(live: &[f64], backtest: &[f64], periods_per_year: u32) -> Option<Tracking> {
    if live.len() != backtest.len() || live.len() < 2 {
        return None;
    }
    let valid = live
        .iter()
        .chain(backtest)
        .all(|r| r.is_finite() && *r > -1.0);
    if !valid {
        return None;
    }

    let n = live.len() as f64;
    let mean = live.iter().zip(backtest).map(|(l, b)| l - b).sum::<f64>() / n;
    let variance = live
        .iter()
        .zip(backtest)
        .map(|(l, b)| {
            let dev = (l - b) - mean;
            dev * dev
        })
        .sum::<f64>()
        / (n - 1.0);
    let tracking_error = variance.sqrt() * f64::from(periods_per_year).sqrt();

    let (mut live_curve, mut backtest_curve) = (1.0_f64, 1.0_f64);
    let mut max_gap = 0.0_f64;
    for (l, b) in live.iter().zip(backtest) {
        live_curve *= 1.0 + l;
        backtest_curve *= 1.0 + b;
        max_gap = max_gap.max((live_curve - backtest_curve).abs());
    }

    if tracking_error.is_finite() && max_gap.is_finite() {
        Some(Tracking {
            tracking_error,
            max_gap,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f64 = 1e-12;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn identical_series_have_zero_distance() {
        let r = [0.01, -0.02, 0.003, 0.0];
        let t = tracking(&r, &r, 252).expect("finite");
        assert!(close(t.tracking_error, 0.0, TOL));
        assert!(close(t.max_gap, 0.0, TOL));
    }

    #[test]
    fn worked_example_matches_formula_and_literal() {
        let live = [0.01, 0.02, 0.00];
        let backtest = [0.00, 0.01, 0.01];
        let t = tracking(&live, &backtest, 252).expect("finite");

        // d = [0.01, 0.01, -0.01]; mean = 1/300; sample variance = 0.000133333...
        let mean = 0.01 / 3.0;
        let d = [0.01_f64, 0.01, -0.01];
        let var: f64 = d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 2.0;
        assert!(close(var, 0.000_133_333_333_333_333_3, 1e-15));
        let expected = var.sqrt() * 252.0_f64.sqrt();
        assert!(close(t.tracking_error, expected, TOL));
        assert!((t.tracking_error - 0.183_303_03).abs() < 1e-7);

        // L = 1.01, 1.0302, 1.0302; B = 1.00, 1.01, 1.0201; gaps 0.01, 0.0202, 0.0101.
        assert!(close(t.max_gap, 0.0202, TOL));
    }

    #[test]
    fn constant_difference_has_zero_tracking_error_but_a_gap() {
        let backtest = [0.01, -0.005, 0.002, 0.004, -0.001];
        let live: Vec<f64> = backtest.iter().map(|b| b + 0.001).collect();
        let t = tracking(&live, &backtest, 252).expect("finite");
        assert!(close(t.tracking_error, 0.0, 1e-12));
        assert!(t.max_gap > 0.0);
    }

    #[test]
    fn invalid_inputs_return_none() {
        assert!(tracking(&[0.01, 0.02], &[0.01], 252).is_none());
        assert!(tracking(&[0.01], &[0.01], 252).is_none());
        assert!(tracking(&[], &[], 252).is_none());
        assert!(tracking(&[0.01, f64::NAN], &[0.0, 0.0], 252).is_none());
        assert!(tracking(&[0.01, 0.02], &[0.0, f64::INFINITY], 252).is_none());
        assert!(tracking(&[0.01, -1.0], &[0.0, 0.0], 252).is_none());
        assert!(tracking(&[0.01, 0.0], &[0.0, -1.5], 252).is_none());
    }

    #[test]
    fn swapping_live_and_backtest_is_symmetric() {
        let live = [0.01, 0.02, 0.00, -0.03];
        let backtest = [0.00, 0.01, 0.01, 0.002];
        let a = tracking(&live, &backtest, 252).expect("finite");
        let b = tracking(&backtest, &live, 252).expect("finite");
        assert!(close(a.tracking_error, b.tracking_error, TOL));
        assert!(close(a.max_gap, b.max_gap, TOL));
    }
}
