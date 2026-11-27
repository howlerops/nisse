//! What happened after a decision: forward returns, the benchmark's, and the excursions on
//! the way. Pure functions on bar series; rules in `docs/phases/phase-5.md`, "Trade-level
//! scores".

use nisse_core::nautilus::UnixNanos;

/// The horizons every decision is scored at, in sessions. Fixed before any trade: changing
/// them is a code change with a new `eval_version`.
pub const OUTCOME_HORIZONS: [u32; 3] = [1, 5, 21];

/// The oldest a decision's base bar may be, in nanoseconds: 20 hours. The engine decides at a
/// session's close, and no two consecutive closes are less than 21 hours apart (a half-day
/// closes at 18:00 UTC after a 21:00 close; daylight-saving changes give 23 hours), so a base
/// bar this old means the decision's own session is missing from the bar file.
pub const MAX_BASE_AGE_NANOS: u64 = 20 * 3_600 * 1_000_000_000;

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

/// Whether a decision at `decision_ts` can never be scored from these bars because its base bar
/// (the last symbol bar at or before it) is [`MAX_BASE_AGE_NANOS`] or more older than the
/// decision. `false` when there is no bar at or before the decision yet (it may still arrive) or
/// the base bar is recent enough. "Never" means never with the bars as they are: a later-arriving
/// bar for the decision's own session would make the base bar recent again.
#[must_use]
pub fn base_too_old(symbol_bars: &[BarPoint], decision_ts: UnixNanos) -> bool {
    let Some(base) = index_at_or_before(symbol_bars, decision_ts).and_then(|t| symbol_bars.get(t))
    else {
        return false;
    };
    decision_ts.as_u64().saturating_sub(base.ts.as_u64()) >= MAX_BASE_AGE_NANOS
}

/// The outcome of a decision made at `decision_ts` over `horizon` sessions. The base bar is the
/// last symbol bar at or before the decision. `None` when it cannot be computed yet or at all:
/// no symbol bar at or before the decision, a base bar [`MAX_BASE_AGE_NANOS`] or more older than
/// the decision, fewer than `horizon` symbol bars after the base bar, the benchmark lacking a bar
/// at the base bar's timestamp, the benchmark having a later bar at or before the decision (the
/// symbol is missing a session the benchmark has, so the decision waits for its bar), a
/// non-positive or non-finite price, or `horizon == 0`.
///
/// Two rules keep a decision off the wrong bar. The benchmark rule: the base bar must be the
/// benchmark's newest bar too. The age rule: the base bar must be less than 20 hours older than
/// the decision, which catches a session missing from every series, including when the
/// benchmark is the symbol itself. Sessions, not hours: a weekend, a holiday or a half-day
/// between the base bar and the decision is only a gap when the age rule catches it. A decision
/// made off-session, 20 hours or more after its bar (for example before Monday's open from
/// Friday's close), is left unscored until the scoring job is given the exchange calendar.
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
    if base_too_old(symbol_bars, decision_ts) {
        return None;
    }
    let t = index_at_or_before(symbol_bars, decision_ts)?;
    let base_ts = symbol_bars.get(t)?.ts;
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

    let bench_start = benchmark_bars.get(index_at(benchmark_bars, base_ts)?)?;
    let newest_bench_ts = benchmark_bars
        .get(index_at_or_before(benchmark_bars, decision_ts)?)?
        .ts;
    if newest_bench_ts > base_ts {
        return None;
    }
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

    /// A bar at `day` plus `hours`, closing at `close`, with 1% high and low.
    fn point_at(day: u64, hours: u64, close: f64) -> BarPoint {
        BarPoint {
            ts: at(day, hours),
            high: close * 1.01,
            low: close * 0.99,
            close,
        }
    }

    #[test]
    fn decision_just_before_the_next_session_is_scored_from_the_earlier_bar() {
        let sym = symbol();
        let bench = bench();

        // Twelve hours after the day 0 bar, before the day 1 bar, no benchmark bar since: still
        // day 0's session, so the decision is scored from the day 0 bar.
        let between = UnixNanos::from(DAY_NANOS / 2);
        let result = outcome(&sym, &bench, between, 1).expect("12h");
        assert_close(result.fwd_ret, 0.02);
        assert_close(result.bench_ret, 0.01);
    }

    #[test]
    fn a_decision_at_a_close_waits_for_the_symbols_own_bar() {
        // Day 1 is missing for the symbol only; the benchmark has it. Decision at day 1's close.
        let mut sym = symbol();
        sym.remove(1);
        let bench = bench();
        assert!(outcome(&sym, &bench, at(1, 0), 1).is_none());
        assert!(outcome(&sym, &bench, at(1, 0), 2).is_none());

        // Once the symbol's day 1 bar is present, the decision is measured from day 1's close
        // (102) to day 2's close (101), against the benchmark's 202 → 204.
        let full = symbol();
        let result = outcome(&full, &bench, at(1, 0), 1).expect("day 1 bar present");
        assert_close(result.fwd_ret, 101.0 / 102.0 - 1.0);
        assert_close(result.bench_ret, 204.0 / 202.0 - 1.0);
    }

    #[test]
    fn a_half_day_without_the_symbols_bar_waits() {
        // The symbol has no bar on day 1; the benchmark closes early that day, at 18:00.
        let sym = vec![point_at(0, 21, 100.0), point_at(2, 21, 103.0)];
        let bench = vec![
            point_at(0, 21, 200.0),
            point_at(1, 18, 202.0),
            point_at(2, 21, 204.0),
        ];
        // The newest symbol bar is 21 hours before the decision, but the benchmark's day 1 bar
        // is later than it: the decision waits.
        assert!(outcome(&sym, &bench, at(1, 18), 1).is_none());

        // With the symbol's own day 1 bar, it is scored from that bar: 103 / 101 - 1.
        let full = vec![
            point_at(0, 21, 100.0),
            point_at(1, 18, 101.0),
            point_at(2, 21, 103.0),
        ];
        let result = outcome(&full, &bench, at(1, 18), 1).expect("day 1 bar present");
        assert_close(result.fwd_ret, 103.0 / 101.0 - 1.0);
        assert_close(result.bench_ret, 204.0 / 202.0 - 1.0);
    }

    #[test]
    fn a_decision_before_monday_open_from_friday_close_is_left_unscored() {
        // Friday close (day 0, 21:00), Monday pre-open (day 3, 09:00): 60 hours later. Neither
        // series has a bar in between, so the benchmark rule would accept Friday's bar. The age
        // rule rejects it: this is the documented cost of the rule, and the decision stays
        // unscored until the scoring job is given the exchange calendar.
        let sym = vec![point_at(0, 21, 100.0), point_at(3, 21, 102.0)];
        let bench = vec![point_at(0, 21, 200.0), point_at(3, 21, 202.0)];
        assert!(outcome(&sym, &bench, at(3, 9), 1).is_none());
    }

    #[test]
    fn a_session_missing_from_both_series_is_left_unscored_at_its_close() {
        // Day 3 has no bar in either series. A decision stamped at that session's close (day 3,
        // 00:00, 24 hours after the day 2 bar) is left unscored: the age rule sees the gap.
        let mut sym_missing = symbol();
        sym_missing.remove(3);
        let mut bench_missing = bench();
        bench_missing.remove(3);
        assert!(outcome(&sym_missing, &bench_missing, at(3, 0), 1).is_none());

        // Once the day 3 bars are present in both series, the decision is measured from day 3's
        // close (105): 103 / 105 - 1 over one session, against the benchmark's 208 / 206 - 1.
        let sym = symbol();
        let bench_full = bench();
        let result = outcome(&sym, &bench_full, at(3, 0), 1).expect("day 3 present");
        assert_close(result.fwd_ret, 103.0 / 105.0 - 1.0);
        assert_close(result.bench_ret, 208.0 / 206.0 - 1.0);
    }

    #[test]
    fn a_missing_session_is_unscored_when_the_benchmark_is_the_symbol() {
        // The symbol is the benchmark, so the benchmark rule cannot catch day 3's missing bar.
        // The age rule does.
        let mut series_missing_day_three = bench();
        series_missing_day_three.remove(3);
        assert!(
            outcome(
                &series_missing_day_three,
                &series_missing_day_three,
                at(3, 0),
                1
            )
            .is_none()
        );

        let full = bench();
        let result = outcome(&full, &full, at(3, 0), 1).expect("day 3 present");
        assert_close(result.fwd_ret, 208.0 / 206.0 - 1.0);
        assert_close(result.bench_ret, 208.0 / 206.0 - 1.0);
    }

    #[test]
    fn the_age_limit_is_strict_at_twenty_hours() {
        let sym = symbol();
        let bench = bench();
        let hour = HOUR_NANOS;
        let minute = 60_000_000_000;

        // 19 hours 59 minutes after the day 0 bar: scored.
        let just_under = UnixNanos::from(19 * hour + 59 * minute);
        let result = outcome(&sym, &bench, just_under, 1).expect("19h59m");
        assert_close(result.fwd_ret, 0.02);

        // Exactly 20 hours after the day 0 bar: unscored.
        assert!(outcome(&sym, &bench, UnixNanos::from(20 * hour), 1).is_none());
    }

    #[test]
    fn a_half_day_missing_from_both_series_is_left_unscored() {
        // Day 1 closes early at 18:00 and neither series has its bar. The previous close is
        // day 0 at 21:00, 21 hours before the decision: the age rule rejects it.
        let sym = vec![point_at(0, 21, 100.0), point_at(2, 21, 103.0)];
        let bench = vec![point_at(0, 21, 200.0), point_at(2, 21, 204.0)];
        assert!(outcome(&sym, &bench, at(1, 18), 1).is_none());
    }

    #[test]
    fn base_too_old_follows_the_age_limit() {
        let sym = symbol();
        let minute = 60_000_000_000;

        // No bar at or before the decision: it may still arrive, so not "too old".
        assert!(!base_too_old(&[], at(0, 0)));
        let late_start = series_from(1, &SYMBOL_CLOSES);
        assert!(!base_too_old(&late_start, at(0, 12)));

        // Age zero, and 19 hours 59 minutes: recent enough.
        assert!(!base_too_old(&sym, at(0, 0)));
        assert!(!base_too_old(
            &sym,
            UnixNanos::from(19 * HOUR_NANOS + 59 * minute)
        ));

        // Exactly 20 hours after the day 0 bar, which is then the newest bar.
        let day_zero_only = series(&SYMBOL_CLOSES[..1]);
        assert!(base_too_old(
            &day_zero_only,
            UnixNanos::from(20 * HOUR_NANOS)
        ));
        // 64 hours after the day 0 bar, with no later bar: too old.
        assert!(base_too_old(&day_zero_only, at(2, 16)));
        // The full series has a day 2 bar, so at day 2 16:00 the base is 16 hours old.
        assert!(!base_too_old(&sym, at(2, 16)));
    }

    #[test]
    fn outcome_is_none_exactly_when_the_base_bar_is_too_old() {
        let sym = symbol();
        let bench = bench();

        // Otherwise scorable (horizon 1 has its end bar): too old, so None.
        let too_old = at(0, 20);
        assert!(base_too_old(&sym, too_old));
        assert!(outcome(&sym, &bench, too_old, 1).is_none());

        // Otherwise scorable and recent: Some, and not too old.
        let recent = UnixNanos::from(19 * HOUR_NANOS + 59 * 60_000_000_000);
        assert!(!base_too_old(&sym, recent));
        assert!(outcome(&sym, &bench, recent, 1).is_some());
    }

    #[test]
    fn a_decision_at_its_base_bar_is_scored() {
        // Age zero: the decision is stamped at the bar's own timestamp, the normal case.
        let sym = symbol();
        let bench = bench();
        let result = outcome(&sym, &bench, at(1, 0), 1).expect("age zero");
        assert_close(result.fwd_ret, 101.0 / 102.0 - 1.0);
        assert_close(result.bench_ret, 204.0 / 202.0 - 1.0);
    }

    #[test]
    fn benchmark_bars_only_after_the_decision_do_not_hold_it_back() {
        // The benchmark has days 1..=5 after the decision at day 0 12:00; none is at or before
        // it, so the decision is scored from day 0 over five sessions.
        let sym = symbol();
        let bench = bench();
        let result = outcome(&sym, &bench, at(0, 12), 5).expect("later benchmark bars");
        assert_close(result.fwd_ret, 0.10);
        assert_close(result.bench_ret, 0.05);
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
