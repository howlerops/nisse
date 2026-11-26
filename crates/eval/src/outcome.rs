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
#[allow(unused_variables, reason = "stub: Phase 5 task OW1")]
#[must_use]
pub fn outcome(
    symbol_bars: &[BarPoint],
    benchmark_bars: &[BarPoint],
    decision_ts: UnixNanos,
    horizon: u32,
) -> Option<Outcome> {
    todo!("Phase 5 task OW1")
}

/// The decision-quality score: `(fwd_ret - bench_ret) / (expected_vol × sqrt(horizon /
/// periods_per_year))`, where `expected_vol` is the decision's annualised volatility.
/// `None` when `expected_vol` is not positive, `periods_per_year` or `horizon` is zero, or
/// the result is not finite.
#[allow(unused_variables, reason = "stub: Phase 5 task OW1")]
#[must_use]
pub fn decision_quality(
    outcome: &Outcome,
    expected_vol: f64,
    periods_per_year: u32,
) -> Option<f64> {
    todo!("Phase 5 task OW1")
}

/// The regime at `ts`: `RiskOn` when the benchmark's last close at or before `ts` is above
/// the mean of its last [`REGIME_WINDOW`] closes up to and including that bar, `RiskOff`
/// when it is at or below it, `Unknown` when there are fewer bars than the window.
#[allow(unused_variables, reason = "stub: Phase 5 task OW1")]
#[must_use]
pub fn regime(benchmark_bars: &[BarPoint], ts: UnixNanos) -> Regime {
    todo!("Phase 5 task OW1")
}
