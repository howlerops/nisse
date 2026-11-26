//! What happened after a decision: forward returns, the benchmark's, and the excursions on
//! the way. Pure functions on bar series; rules in `docs/phases/phase-5.md`, "Trade-level
//! scores".

use nisse_core::nautilus::UnixNanos;

/// The horizons every decision is scored at, in sessions. Fixed before any trade: changing
/// them is a code change with a new `eval_version`.
pub const OUTCOME_HORIZONS: [u32; 3] = [1, 5, 21];

/// Sessions in the mean the regime compares the benchmark's close with.
pub const REGIME_WINDOW: usize = 200;

/// One daily bar of one symbol, as the scores need it. Series are sorted by `ts`, strictly
/// increasing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarPoint {
    /// The session close.
    pub ts: UnixNanos,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// One `outcome_window` row's figures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outcome {
    /// Sessions.
    pub horizon: u32,
    pub fwd_ret: f64,
    pub bench_ret: f64,
    /// Largest favourable excursion, as a return from the decision's close (can be negative
    /// when the price never rose above it).
    pub mfe: f64,
    /// Largest adverse excursion, as a return from the decision's close.
    pub mae: f64,
}

/// The market state a decision was made in, from the benchmark alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Regime {
    RiskOn,
    RiskOff,
    /// Fewer than [`REGIME_WINDOW`] benchmark bars up to the time asked about.
    Unknown,
}

impl Regime {
    /// The label used in reports and scores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RiskOn => "risk_on",
            Self::RiskOff => "risk_off",
            Self::Unknown => "unknown",
        }
    }
}

/// The outcome of a decision made at `decision_ts` over `horizon` sessions. `None` when it
/// cannot be computed yet or at all: no symbol bar at or before the decision, fewer than
/// `horizon` symbol bars after it, the benchmark lacking a bar at either end's timestamp, a
/// non-positive or non-finite price, or `horizon == 0`.
#[must_use]
pub fn outcome(
    symbol_bars: &[BarPoint],
    benchmark_bars: &[BarPoint],
    decision_ts: UnixNanos,
    horizon: u32,
) -> Option<Outcome> {
    if horizon == 0 {
        return None;
    }
    let t = index_at_or_before(symbol_bars, decision_ts)?;
    let sessions = usize::try_from(horizon).ok()?;
    let end = t.checked_add(sessions)?;
    if end >= symbol_bars.len() {
        return None;
    }

    let base = symbol_bars.get(t)?.close;
    let end_close = symbol_bars.get(end)?.close;
    if !is_usable_price(base) || !is_usable_price(end_close) {
        return None;
    }

    // The decision's own bar is excluded: the excursion runs over the bars after it.
    let mut max_high = f64::NEG_INFINITY;
    let mut min_low = f64::INFINITY;
    for bar in symbol_bars.get(t + 1..=end)? {
        if !is_usable_price(bar.high) || !is_usable_price(bar.low) || !is_usable_price(bar.close) {
            return None;
        }
        max_high = max_high.max(bar.high);
        min_low = min_low.min(bar.low);
    }

    let bench_start = benchmark_bars.get(index_at(benchmark_bars, symbol_bars.get(t)?.ts)?)?;
    let bench_end = benchmark_bars.get(index_at(benchmark_bars, symbol_bars.get(end)?.ts)?)?;
    if !is_usable_price(bench_start.close) || !is_usable_price(bench_end.close) {
        return None;
    }

    let result = Outcome {
        horizon,
        fwd_ret: end_close / base - 1.0,
        bench_ret: bench_end.close / bench_start.close - 1.0,
        mfe: max_high / base - 1.0,
        mae: min_low / base - 1.0,
    };
    let all_finite = result.fwd_ret.is_finite()
        && result.bench_ret.is_finite()
        && result.mfe.is_finite()
        && result.mae.is_finite();
    all_finite.then_some(result)
}

/// The decision-quality score: `(fwd_ret - bench_ret) / (expected_vol × sqrt(horizon /
/// periods_per_year))`, where `expected_vol` is the decision's annualised volatility.
/// `None` when `expected_vol` is not positive, `periods_per_year` or `horizon` is zero, or
/// the result is not finite.
#[must_use]
pub fn decision_quality(
    outcome: &Outcome,
    expected_vol: f64,
    periods_per_year: u32,
) -> Option<f64> {
    if !expected_vol.is_finite() || expected_vol <= 0.0 {
        return None;
    }
    if periods_per_year == 0 || outcome.horizon == 0 {
        return None;
    }
    let years = f64::from(outcome.horizon) / f64::from(periods_per_year);
    let scale = expected_vol * years.sqrt();
    let score = (outcome.fwd_ret - outcome.bench_ret) / scale;
    score.is_finite().then_some(score)
}

/// The regime at `ts`: `RiskOn` when the benchmark's last close at or before `ts` is above
/// the mean of its last [`REGIME_WINDOW`] closes up to and including that bar, `RiskOff`
/// when it is at or below it, `Unknown` when there are fewer bars than the window.
#[must_use]
pub fn regime(benchmark_bars: &[BarPoint], ts: UnixNanos) -> Regime {
    let Some(t) = index_at_or_before(benchmark_bars, ts) else {
        return Regime::Unknown;
    };
    if t + 1 < REGIME_WINDOW {
        return Regime::Unknown;
    }
    let Some(window) = benchmark_bars.get(t + 1 - REGIME_WINDOW..=t) else {
        return Regime::Unknown;
    };
    let mut sum = 0.0;
    for bar in window {
        if !bar.close.is_finite() {
            return Regime::Unknown;
        }
        sum += bar.close;
    }
    let mean = sum / REGIME_WINDOW as f64;
    let last_close = benchmark_bars.get(t).map_or(f64::NAN, |bar| bar.close);
    if last_close > mean {
        Regime::RiskOn
    } else {
        Regime::RiskOff
    }
}

/// Index of the last bar with `ts <= at`, or `None` when every bar is later.
fn index_at_or_before(bars: &[BarPoint], at: UnixNanos) -> Option<usize> {
    bars.partition_point(|bar| bar.ts <= at).checked_sub(1)
}

/// Index of the bar whose `ts` equals `at` exactly, or `None` when there is none.
fn index_at(bars: &[BarPoint], at: UnixNanos) -> Option<usize> {
    let i = bars.partition_point(|bar| bar.ts < at);
    bars.get(i).filter(|bar| bar.ts == at).map(|_| i)
}

/// A price that can be used in a return: finite and strictly positive.
fn is_usable_price(price: f64) -> bool {
    price.is_finite() && price > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_NANOS: u64 = 86_400_000_000_000;
    const HOUR_NANOS: u64 = 3_600_000_000_000;
    const TOL: f64 = 1e-12;

    fn bar(day: u64, high: f64, low: f64, close: f64) -> BarPoint {
        BarPoint {
            ts: UnixNanos::from(day * DAY_NANOS),
            high,
            low,
            close,
        }
    }

    /// A bar whose high and low are 1% either side of the close.
    fn flat_bar(day: u64, close: f64) -> BarPoint {
        bar(day, close * 1.01, close * 0.99, close)
    }

    /// Bars on consecutive days from `first_day`, one per close, with 1% high and low.
    fn series_from(first_day: u64, closes: &[f64]) -> Vec<BarPoint> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &close)| flat_bar(first_day + i as u64, close))
            .collect()
    }

    fn series(closes: &[f64]) -> Vec<BarPoint> {
        series_from(0, closes)
    }

    fn at(day: u64, hours: u64) -> UnixNanos {
        UnixNanos::from(day * DAY_NANOS + hours * HOUR_NANOS)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < TOL,
            "expected {expected}, got {actual}"
        );
    }

    const SYMBOL_CLOSES: [f64; 6] = [100.0, 102.0, 101.0, 105.0, 103.0, 110.0];
    const BENCH_CLOSES: [f64; 6] = [200.0, 202.0, 204.0, 206.0, 208.0, 210.0];

    fn symbol() -> Vec<BarPoint> {
        series(&SYMBOL_CLOSES)
    }

    fn bench() -> Vec<BarPoint> {
        series(&BENCH_CLOSES)
    }

    #[test]
    fn horizons_are_one_five_and_twenty_one_sessions() {
        assert_eq!(OUTCOME_HORIZONS, [1, 5, 21]);
    }

    #[test]
    fn regime_labels_are_stable() {
        assert_eq!(Regime::RiskOn.as_str(), "risk_on");
        assert_eq!(Regime::RiskOff.as_str(), "risk_off");
        assert_eq!(Regime::Unknown.as_str(), "unknown");
    }

    #[test]
    fn outcome_on_the_decision_bar_horizon_one_and_five() {
        let sym = symbol();
        let bench = bench();

        // Horizon 1: 102 / 100 - 1; benchmark 202 / 200 - 1; mfe 102 * 1.01 / 100 - 1.
        let one = outcome(&sym, &bench, at(0, 0), 1).expect("horizon 1");
        assert_eq!(one.horizon, 1);
        assert_close(one.fwd_ret, 0.02);
        assert_close(one.bench_ret, 0.01);
        assert_close(one.mfe, 0.0302);
        assert_close(one.mae, 0.0098);

        // Horizon 5: 110 / 100 - 1; benchmark 210 / 200 - 1; mfe 110 * 1.01 / 100 - 1;
        // mae from the min low 101 * 0.99 = 99.99 over days 1..=5 → -0.0001.
        let five = outcome(&sym, &bench, at(0, 0), 5).expect("horizon 5");
        assert_eq!(five.horizon, 5);
        assert_close(five.fwd_ret, 0.10);
        assert_close(five.bench_ret, 0.05);
        assert_close(five.mfe, 0.111);
        assert_close(five.mae, -0.0001);
    }

    #[test]
    fn decision_bar_is_not_part_of_the_excursion() {
        let mut sym = symbol();
        sym[0] = bar(0, 1000.0, 0.001, 100.0);
        let bench = bench();

        let one = outcome(&sym, &bench, at(0, 0), 1).expect("horizon 1");
        assert_close(one.mfe, 0.0302);
        assert_close(one.mae, 0.0098);
    }

    #[test]
    fn decision_between_bars_uses_the_earlier_bar() {
        let sym = symbol();
        let bench = bench();

        // Twelve hours after day 0 uses day 0 as the base.
        let between = outcome(&sym, &bench, at(0, 12), 1).expect("between");
        assert_close(between.fwd_ret, 0.02);

        // Exactly at day 2 uses day 2 (close 101) as the base; end is day 3 (close 105).
        let exact = outcome(&sym, &bench, at(2, 0), 1).expect("exact");
        assert_close(exact.fwd_ret, 105.0 / 101.0 - 1.0);
        assert_close(exact.bench_ret, 206.0 / 204.0 - 1.0);
    }

    #[test]
    fn decision_before_the_first_bar_is_none() {
        let sym = series_from(1, &SYMBOL_CLOSES);
        let bench = series_from(1, &BENCH_CLOSES);
        assert!(outcome(&sym, &bench, at(0, 12), 1).is_none());
        assert!(outcome(&sym, &bench, at(0, 0), 1).is_none());
    }

    #[test]
    fn not_enough_bars_yet_is_none() {
        let sym = symbol();
        let bench = bench();

        // Day 1 plus five sessions needs day 6, which does not exist.
        let from_day_one = series_from(0, &SYMBOL_CLOSES);
        assert!(outcome(&from_day_one, &bench, at(1, 0), 5).is_none());
        assert!(outcome(&sym, &bench, at(0, 0), 5).is_some());
        assert!(outcome(&sym, &bench, at(0, 0), 0).is_none());
    }

    #[test]
    fn benchmark_must_have_bars_at_both_ends() {
        let sym = symbol();

        // Missing the bar at t (day 0).
        let no_start = series_from(1, &BENCH_CLOSES);
        assert!(outcome(&sym, &no_start, at(0, 0), 1).is_none());

        // Missing the bar at end (day 5).
        let no_end = series(&BENCH_CLOSES[..5]);
        assert!(outcome(&sym, &no_end, at(0, 0), 5).is_none());

        // Extra bars on other days are fine.
        let mut extra = bench();
        extra.push(flat_bar(10, 500.0));
        let ok = outcome(&sym, &extra, at(0, 0), 5).expect("extra bars");
        assert_close(ok.bench_ret, 0.05);
    }

    #[test]
    fn non_usable_prices_in_the_range_are_none() {
        let bad_values = [0.0, -1.0, f64::NAN, f64::INFINITY];
        let bench = bench();

        for &bad in &bad_values {
            // Base close (day 0).
            let mut sym = symbol();
            sym[0].close = bad;
            assert!(outcome(&sym, &bench, at(0, 0), 1).is_none(), "base {bad}");

            // End close (day 1 with horizon 1).
            let mut sym = symbol();
            sym[1].close = bad;
            assert!(outcome(&sym, &bench, at(0, 0), 1).is_none(), "end {bad}");

            // A high and a low inside the window (day 1 with horizon 1).
            let mut sym = symbol();
            sym[1].high = bad;
            assert!(outcome(&sym, &bench, at(0, 0), 1).is_none(), "high {bad}");
            let mut sym = symbol();
            sym[1].low = bad;
            assert!(outcome(&sym, &bench, at(0, 0), 1).is_none(), "low {bad}");

            // Benchmark close at t and at end.
            let mut b = bench.clone();
            b[0].close = bad;
            assert!(
                outcome(&symbol(), &b, at(0, 0), 1).is_none(),
                "bench t {bad}"
            );
            let mut b = bench.clone();
            b[1].close = bad;
            assert!(
                outcome(&symbol(), &b, at(0, 0), 1).is_none(),
                "bench end {bad}"
            );
        }
    }

    #[test]
    fn mfe_can_be_negative_when_prices_only_fall() {
        let sym = vec![
            bar(0, 101.0, 99.0, 100.0),
            bar(1, 99.0, 97.0, 98.0),
            bar(2, 98.0, 96.0, 97.0),
        ];
        let bench = series(&[200.0, 199.0, 198.0]);
        let result = outcome(&sym, &bench, at(0, 0), 2).expect("falling");
        // Highest high over days 1..=2 is 99 → 99 / 100 - 1 = -0.01.
        assert_close(result.mfe, -0.01);
        assert!(result.mfe < 0.0);
        // Lowest low is 96 → -0.04.
        assert_close(result.mae, -0.04);
    }

    #[test]
    fn mae_can_be_positive_when_prices_only_rise() {
        let sym = vec![
            bar(0, 101.0, 99.0, 100.0),
            bar(1, 106.0, 102.0, 104.0),
            bar(2, 108.0, 103.0, 107.0),
        ];
        let bench = series(&[200.0, 201.0, 202.0]);
        let result = outcome(&sym, &bench, at(0, 0), 2).expect("rising");
        // Lowest low over days 1..=2 is 102 → 102 / 100 - 1 = 0.02.
        assert_close(result.mae, 0.02);
        assert!(result.mae > 0.0);
        // Highest high is 108 → 0.08.
        assert_close(result.mfe, 0.08);
    }

    #[test]
    fn decision_quality_matches_the_worked_value() {
        let outcome = Outcome {
            horizon: 21,
            fwd_ret: 0.03,
            bench_ret: 0.01,
            mfe: 0.0,
            mae: 0.0,
        };
        // 0.02 / (0.16 × sqrt(21 / 252)) = 0.02 / (0.16 × 0.288675134…) = 0.433012701…
        let score = decision_quality(&outcome, 0.16, 252).expect("score");
        assert!((score - 0.433_012_701).abs() < 1e-9, "got {score}");
    }

    #[test]
    fn decision_quality_is_none_for_invalid_inputs() {
        let outcome = Outcome {
            horizon: 21,
            fwd_ret: 0.03,
            bench_ret: 0.01,
            mfe: 0.0,
            mae: 0.0,
        };
        assert!(decision_quality(&outcome, 0.0, 252).is_none());
        assert!(decision_quality(&outcome, -0.16, 252).is_none());
        assert!(decision_quality(&outcome, f64::NAN, 252).is_none());
        assert!(decision_quality(&outcome, f64::INFINITY, 252).is_none());
        assert!(decision_quality(&outcome, 0.16, 0).is_none());

        let no_horizon = Outcome {
            horizon: 0,
            ..outcome
        };
        assert!(decision_quality(&no_horizon, 0.16, 252).is_none());
    }

    #[test]
    fn regime_needs_a_full_window() {
        let short: Vec<f64> = (0..199).map(|i| 100.0 + f64::from(i)).collect();
        let bars = series(&short);
        assert_eq!(regime(&bars, at(198, 0)), Regime::Unknown);
        assert_eq!(regime(&bars, at(500, 0)), Regime::Unknown);
    }

    #[test]
    fn regime_of_a_rising_series_is_risk_on() {
        // Closes 100..=299; last close 299 > mean 199.5.
        let closes: Vec<f64> = (0..200).map(|i| 100.0 + f64::from(i)).collect();
        let bars = series(&closes);
        assert_eq!(regime(&bars, at(199, 0)), Regime::RiskOn);
    }

    #[test]
    fn regime_of_a_falling_series_is_risk_off() {
        // Closes 300 down to 101; last close 101 < mean 200.5.
        let closes: Vec<f64> = (0..200).map(|i| 300.0 - f64::from(i)).collect();
        let bars = series(&closes);
        assert_eq!(regime(&bars, at(199, 0)), Regime::RiskOff);
    }

    #[test]
    fn regime_of_a_constant_series_is_risk_off() {
        let closes = vec![100.0; 200];
        let bars = series(&closes);
        assert_eq!(regime(&bars, at(199, 0)), Regime::RiskOff);
    }

    #[test]
    fn regime_before_the_first_bar_is_unknown() {
        let closes: Vec<f64> = (0..200).map(|i| 100.0 + f64::from(i)).collect();
        let bars = series_from(1, &closes);
        assert_eq!(regime(&bars, at(0, 12)), Regime::Unknown);
    }

    #[test]
    fn regime_between_bars_uses_the_earlier_bar() {
        // Days 0..=199 rise from 100 to 299; day 200 crashes to 50.
        let mut closes: Vec<f64> = (0..200).map(|i| 100.0 + f64::from(i)).collect();
        closes.push(50.0);
        let bars = series(&closes);

        // Day 199 + 12h: the earlier bar is day 199, RiskOn (299 > 199.5).
        assert_eq!(regime(&bars, at(199, 12)), Regime::RiskOn);
        // Day 200 exactly: window 101..=299 plus 50 has mean 19950 / 200 = 99.75; 50 < 99.75.
        assert_eq!(regime(&bars, at(200, 0)), Regime::RiskOff);
        assert_eq!(regime(&bars, at(200, 12)), Regime::RiskOff);
    }

    #[test]
    fn regime_uses_only_the_last_window_of_bars() {
        // Days 0..=99 at 1000, then days 100..=299 flat at 100.
        // Mean of the last 200 bars up to day 299 is 100; close 100 is not above it → RiskOff.
        // Averaging all 300 bars would give (100 × 1000 + 200 × 100) / 300 = 40, and the close
        // 100 would be above it → RiskOn. So the answer distinguishes the window.
        let mut closes = vec![1000.0; 100];
        closes.extend(std::iter::repeat_n(100.0, 200));
        let bars = series(&closes);
        assert_eq!(regime(&bars, at(299, 0)), Regime::RiskOff);
    }

    #[test]
    fn regime_with_a_non_finite_close_in_the_window_is_unknown() {
        let closes: Vec<f64> = (0..200).map(|i| 100.0 + f64::from(i)).collect();
        let mut bars = series(&closes);
        bars[0].close = f64::NAN;
        assert_eq!(regime(&bars, at(199, 0)), Regime::Unknown);
    }
}
