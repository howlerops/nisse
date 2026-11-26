//! Summary statistics of an equity curve. These are statistics, not money, so they are `f64`.

use nisse_core::nautilus::UnixNanos;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Number, Value, json};

/// One point on the equity curve: account equity at a session's close.
pub type EquityPoint = (UnixNanos, Decimal);

/// What a backtest (or a live period) looks like in a few numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// Sessions in the curve.
    pub sessions: u64,
    /// Final equity over initial equity, minus one.
    pub total_return: f64,
    /// Compound annual growth rate.
    pub cagr: f64,
    /// Annualised standard deviation of daily returns (sample, n - 1).
    pub ann_vol: f64,
    /// Annualised mean daily return over `ann_vol` (risk-free rate taken as zero). Zero when
    /// the volatility is zero.
    pub sharpe: f64,
    /// Largest peak-to-trough fall, as a positive fraction (0.25 is a 25% drawdown).
    pub max_drawdown: f64,
}

/// Converts an equity value to a finite `f64`, `None` otherwise.
fn finite_f64(value: Decimal) -> Option<f64> {
    value.to_f64().filter(|x| x.is_finite())
}

/// Maps a non-finite `f64` to JSON `null`.
fn json_number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// Summarises an equity curve. `None` when it has fewer than two points or a non-positive
/// starting equity.
#[must_use]
pub fn summarize(curve: &[EquityPoint], sessions_per_year: u32) -> Option<Summary> {
    if curve.len() < 2 || sessions_per_year == 0 {
        return None;
    }
    let n = curve.len();
    let sessions = u64::try_from(n).ok()?;
    // Number of daily returns (n - 1), as u32 so it converts to f64 without a lossy cast.
    let returns = u32::try_from(n - 1).ok()?;
    let per_year = f64::from(sessions_per_year);

    let first = finite_f64(curve[0].1)?;
    if first <= 0.0 {
        return None;
    }

    let mut prev = first;
    let mut peak = first;
    let mut last = first;
    let mut max_drawdown = 0.0_f64;
    // Welford's single-pass update: running mean and sum of squared deviations of the returns.
    let mut count = 0_u32;
    let mut mean = 0.0_f64;
    let mut m2 = 0.0_f64;

    for point in &curve[1..] {
        let equity = finite_f64(point.1)?;
        if prev <= 0.0 {
            // The account was wiped out before this session: returns are undefined.
            return None;
        }
        let r = equity / prev - 1.0;
        count += 1;
        let delta = r - mean;
        mean += delta / f64::from(count);
        m2 += delta * (r - mean);

        peak = peak.max(equity);
        max_drawdown = max_drawdown.max((peak - equity) / peak);

        prev = equity;
        last = equity;
    }

    let total_return = last / first - 1.0;
    let cagr = if last <= 0.0 {
        -1.0
    } else {
        // years = returns / sessions_per_year, so growth^(1 / years) = growth^(per_year / returns).
        (last / first).powf(per_year / f64::from(returns)) - 1.0
    };

    let (ann_vol, sharpe) = if returns < 2 {
        (0.0, 0.0)
    } else {
        let sample_var = (m2 / f64::from(returns - 1)).max(0.0);
        let ann_vol = sample_var.sqrt() * per_year.sqrt();
        let sharpe = if ann_vol > 0.0 {
            mean * per_year / ann_vol
        } else {
            0.0
        };
        (ann_vol, sharpe)
    };

    Some(Summary {
        sessions,
        total_return,
        cagr,
        ann_vol,
        sharpe,
        max_drawdown,
    })
}

impl Summary {
    /// The summary as the JSON stored in `trial_result.result`. Non-finite numbers are
    /// written as `null`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "sessions": self.sessions,
            "total_return": json_number(self.total_return),
            "cagr": json_number(self.cagr),
            "ann_vol": json_number(self.ann_vol),
            "sharpe": json_number(self.sharpe),
            "max_drawdown": json_number(self.max_drawdown),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const TOL: f64 = 1e-9;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= TOL,
            "expected {expected}, got {actual}"
        );
    }

    /// Equity points with consecutive timestamps.
    fn curve(values: &[&str]) -> Vec<EquityPoint> {
        values
            .iter()
            .zip(0_u64..)
            .map(|(v, i)| {
                (
                    UnixNanos::from(i),
                    Decimal::from_str(v).expect("test decimal"),
                )
            })
            .collect()
    }

    #[test]
    fn empty_and_single_point_curves_are_none() {
        assert!(summarize(&[], 252).is_none());
        assert!(summarize(&curve(&["100"]), 252).is_none());
    }

    #[test]
    fn non_positive_start_is_none() {
        assert!(summarize(&curve(&["0", "100"]), 252).is_none());
        assert!(summarize(&curve(&["-5", "100"]), 252).is_none());
    }

    #[test]
    fn zero_sessions_per_year_is_none() {
        assert!(summarize(&curve(&["100", "101"]), 0).is_none());
    }

    #[test]
    fn wipeout_in_the_middle_is_none() {
        assert!(summarize(&curve(&["100", "50", "0", "10"]), 252).is_none());
        assert!(summarize(&curve(&["100", "-10", "20"]), 252).is_none());
    }

    #[test]
    fn final_wipeout_gives_minus_one_cagr() {
        let s = summarize(&curve(&["100", "50", "0"]), 252).expect("summary");
        assert_eq!(s.sessions, 3);
        assert_close(s.total_return, -1.0);
        assert_close(s.cagr, -1.0);
    }

    #[test]
    fn flat_curve_is_all_zero() {
        let values = ["100"; 10];
        let s = summarize(&curve(&values), 252).expect("summary");
        assert_eq!(s.sessions, 10);
        assert_close(s.total_return, 0.0);
        assert_close(s.cagr, 0.0);
        assert_close(s.ann_vol, 0.0);
        assert_close(s.sharpe, 0.0);
        assert_close(s.max_drawdown, 0.0);
    }

    #[test]
    fn constant_growth_doubles_over_one_year() {
        // 253 points: 252 daily returns, one year at 252 sessions. Growth factor 2^(1/252).
        let strings: Vec<String> = (0..253)
            .map(|i| format!("{:.12}", 100.0 * 2f64.powf(i as f64 / 252.0)))
            .collect();
        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
        let s = summarize(&curve(&refs), 252).expect("summary");
        assert_eq!(s.sessions, 253);
        assert_close(s.total_return, 1.0);
        assert_close(s.cagr, 1.0);
        assert_close(s.max_drawdown, 0.0);
        assert_close(s.ann_vol, 0.0);
    }

    #[test]
    fn five_point_curve_matches_hand_computed_values() {
        let s = summarize(&curve(&["100", "110", "99", "108.9", "120"]), 252).expect("summary");

        // Returns, written out from the definition r[i] = e[i] / e[i-1] - 1.
        let r = [
            110.0 / 100.0 - 1.0, // 0.10
            99.0 / 110.0 - 1.0,  // -0.10
            108.9 / 99.0 - 1.0,  // 0.10
            120.0 / 108.9 - 1.0, // 0.101928374...
        ];
        let mean = (r[0] + r[1] + r[2] + r[3]) / 4.0;
        let sum_sq_dev: f64 = r.iter().map(|x| (x - mean) * (x - mean)).sum();
        let sample_sd = (sum_sq_dev / 3.0).sqrt();
        let ann_vol = sample_sd * f64::from(252_u32).sqrt();
        let sharpe = mean * 252.0 / ann_vol;

        assert_eq!(s.sessions, 5);
        assert_close(s.total_return, 120.0 / 100.0 - 1.0);
        assert_close(s.ann_vol, ann_vol);
        assert_close(s.sharpe, sharpe);
        assert_close(s.max_drawdown, (110.0 - 99.0) / 110.0);
        assert_close(s.max_drawdown, 0.1);
    }

    #[test]
    fn drawdown_is_measured_from_running_peak() {
        // Peak 120 falls to 90 (25%); the later fall from 130 to 117 is only 10%.
        let s = summarize(&curve(&["100", "120", "90", "130", "117"]), 252).expect("summary");
        assert_close(s.max_drawdown, 0.25);
    }

    #[test]
    fn rising_curve_has_no_drawdown() {
        let s = summarize(&curve(&["100", "101", "105", "110"]), 252).expect("summary");
        assert_close(s.max_drawdown, 0.0);
        assert!(s.total_return > 0.0);
        assert!(s.sharpe > 0.0);
    }

    #[test]
    fn two_points_have_no_sample_volatility() {
        let s = summarize(&curve(&["100", "110"]), 252).expect("summary");
        assert_eq!(s.sessions, 2);
        assert_close(s.ann_vol, 0.0);
        assert_close(s.sharpe, 0.0);
        assert_close(s.total_return, 0.1);
        // One return of 0.1 over one session of a 252-session year: 1.1^252 - 1.
        assert_close(s.cagr, 1.1_f64.powf(252.0) - 1.0);
    }

    #[test]
    fn to_json_has_six_keys_and_null_for_non_finite() {
        let s = summarize(&curve(&["100", "110", "99"]), 252).expect("summary");
        let v = s.to_json();
        let obj = v.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "ann_vol",
                "cagr",
                "max_drawdown",
                "sessions",
                "sharpe",
                "total_return"
            ]
        );
        assert!(v["sessions"].is_u64());
        assert_eq!(v["sessions"].as_u64(), Some(3));
        assert!(v["total_return"].is_f64());
        assert!(v["max_drawdown"].is_f64());

        let hand = Summary {
            sessions: 7,
            total_return: 0.5,
            cagr: f64::INFINITY,
            ann_vol: 0.2,
            sharpe: f64::NAN,
            max_drawdown: 0.1,
        };
        let v = hand.to_json();
        assert_eq!(v["sessions"].as_u64(), Some(7));
        assert!(v["cagr"].is_null());
        assert!(v["sharpe"].is_null());
        assert_close(v["total_return"].as_f64().expect("number"), 0.5);
        assert_close(v["ann_vol"].as_f64().expect("number"), 0.2);
        assert_close(v["max_drawdown"].as_f64().expect("number"), 0.1);
    }
}
