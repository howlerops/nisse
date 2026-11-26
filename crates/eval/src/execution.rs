//! How well decisions were executed, and what each round trip made. Pure functions; rules
//! in `docs/phases/phase-5.md`, "Trade-level scores". Inputs are exact decimals (they are
//! prices, quantities and fees); the scores are ratios, so they are `f64`.

use nisse_core::{
    DecisionId,
    nautilus::{OrderSide, UnixNanos},
};
use rust_decimal::Decimal;

/// One fill of one of a decision's orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillRow {
    /// The decision whose order this fill belongs to.
    pub decision_id: DecisionId,
    pub side: OrderSide,
    pub qty: Decimal,
    pub px: Decimal,
    pub fee: Decimal,
    pub ts: UnixNanos,
}

/// The execution scores of one decision. Each is `None` when it cannot be computed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExecutionScore {
    /// `side × (average fill price − arrival) / arrival × 10⁴`: positive is a cost.
    pub shortfall_bps: Option<f64>,
    /// `fees / (filled quantity × arrival) × 10⁴`.
    pub fees_bps: Option<f64>,
    /// `1 − filled / ordered`. Only once every order of the decision is terminal.
    pub missed_fill_ratio: Option<f64>,
}

/// Scores one decision's execution. `fills` are all the fills of its orders (all on `side`),
/// `arrival` the price the decision was made at, `ordered_qty` the total quantity of its
/// orders, and `all_terminal` whether every one of them has reached a terminal state.
#[allow(unused_variables, reason = "stub: Phase 5 task EX1")]
#[must_use]
pub fn execution_score(
    side: OrderSide,
    fills: &[FillRow],
    arrival: Decimal,
    ordered_qty: Decimal,
    all_terminal: bool,
) -> ExecutionScore {
    todo!("Phase 5 task EX1")
}

/// One closed lot: shares bought by one decision's order and sold later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundTrip {
    pub entry_decision_id: DecisionId,
    pub exit_decision_id: DecisionId,
    pub qty: Decimal,
    pub entry_px: Decimal,
    pub exit_px: Decimal,
    pub entry_ts: UnixNanos,
    pub exit_ts: UnixNanos,
    /// `(exit_px − entry_px) × qty`, minus the share of both fills' fees that belongs to
    /// this quantity.
    pub pnl: Decimal,
}

/// What first-in-first-out matching of one symbol's fills produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Matched {
    /// Closed lots, in the order they were closed.
    pub round_trips: Vec<RoundTrip>,
    /// Shares sold with no earlier buy to match (the account is long-only, so this should
    /// be zero; a non-zero figure means fills are missing).
    pub unmatched_sell_qty: Decimal,
}

/// Matches one symbol's fills (in time order) first in, first out. A sell that closes
/// several buys gives one round trip per buy lot it touches.
#[allow(unused_variables, reason = "stub: Phase 5 task EX1")]
#[must_use]
pub fn match_round_trips(fills: &[FillRow]) -> Matched {
    todo!("Phase 5 task EX1")
}

/// The exit diagnostics of one round trip, in units of R.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExitScore {
    pub realized_r: Option<f64>,
    pub mfe_r: Option<f64>,
    pub mae_r: Option<f64>,
    /// `pnl / MFE in dollars`, when the MFE is positive.
    pub capture: Option<f64>,
}

/// Scores one round trip. `closes` are the symbol's closes from the entry's session up to
/// and including the exit's session, and `risk` is one R in dollars for this lot (the entry
/// order's recorded risk, scaled to the lot's share of that order).
#[allow(unused_variables, reason = "stub: Phase 5 task EX1")]
#[must_use]
pub fn exit_score(trip: &RoundTrip, closes: &[Decimal], risk: Decimal) -> ExitScore {
    todo!("Phase 5 task EX1")
}
