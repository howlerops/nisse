//! How well decisions were executed, and what each round trip made. Pure functions; rules
//! in `docs/phases/phase-5.md`, "Trade-level scores". Inputs are exact decimals (they are
//! prices, quantities and fees); the scores are ratios, so they are `f64`.

use std::collections::VecDeque;

use nisse_core::{
    DecisionId,
    nautilus::{OrderSide, UnixNanos},
};
use rust_decimal::{Decimal, prelude::ToPrimitive};

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

/// Basis points per unit: `× 10⁴`.
const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// Scores one decision's execution. `fills` are all the fills of its orders (all on `side`),
/// `arrival` the price the decision was made at, `ordered_qty` the total quantity of its
/// orders, and `all_terminal` whether every one of them has reached a terminal state.
#[must_use]
pub fn execution_score(
    side: OrderSide,
    fills: &[FillRow],
    arrival: Decimal,
    ordered_qty: Decimal,
    all_terminal: bool,
) -> ExecutionScore {
    // Each sum is tracked separately, so an overflow only costs the scores that use it.
    let mut filled = Some(Decimal::ZERO);
    let mut notional = Some(Decimal::ZERO);
    let mut fees = Some(Decimal::ZERO);
    for fill in fills.iter().filter(|f| f.qty > Decimal::ZERO) {
        filled = filled.and_then(|s| s.checked_add(fill.qty));
        notional = notional.and_then(|s| s.checked_add(fill.qty.checked_mul(fill.px)?));
        fees = fees.and_then(|s| s.checked_add(fill.fee));
    }

    let priced = filled
        .filter(|f| *f > Decimal::ZERO)
        .filter(|_| arrival > Decimal::ZERO);

    let shortfall_bps = priced
        .zip(notional)
        .and_then(|(f, n)| shortfall(side, n, f, arrival))
        .and_then(|d| d.to_f64());

    let fees_bps = priced
        .zip(fees)
        .and_then(|(f, fe)| fees_bps_of(fe, f, arrival))
        .and_then(|d| d.to_f64());

    let missed_fill_ratio = if all_terminal && ordered_qty > Decimal::ZERO {
        filled
            .and_then(|f| f.checked_div(ordered_qty))
            .and_then(|r| Decimal::ONE.checked_sub(r))
            .map(|m| m.max(Decimal::ZERO).min(Decimal::ONE))
            .and_then(|d| d.to_f64())
    } else {
        None
    };

    ExecutionScore {
        shortfall_bps,
        fees_bps,
        missed_fill_ratio,
    }
}

/// `side × (avg − arrival) / arrival × 10⁴`, with `avg = notional / filled`.
fn shortfall(
    side: OrderSide,
    notional: Decimal,
    filled: Decimal,
    arrival: Decimal,
) -> Option<Decimal> {
    let avg = notional.checked_div(filled)?;
    let gap = avg.checked_sub(arrival)?;
    let signed = match side {
        OrderSide::Buy => gap,
        OrderSide::Sell => -gap,
    };
    signed.checked_div(arrival)?.checked_mul(BPS)
}

/// `fees / (filled × arrival) × 10⁴`.
fn fees_bps_of(fees: Decimal, filled: Decimal, arrival: Decimal) -> Option<Decimal> {
    let cost_base = filled.checked_mul(arrival)?;
    fees.checked_div(cost_base)?.checked_mul(BPS)
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

/// An open buy lot, waiting to be matched by a sell.
#[derive(Debug, Clone, Copy)]
struct Lot {
    decision_id: DecisionId,
    remaining: Decimal,
    px: Decimal,
    ts: UnixNanos,
    /// `None` when the division overflowed; the trips it touches are then not emitted.
    fee_per_share: Option<Decimal>,
}

/// Matches one symbol's fills (in time order) first in, first out. A sell that closes
/// several buys gives one round trip per buy lot it touches.
///
/// A trip whose P&L overflows `Decimal` cannot be represented, so it is left out of
/// `round_trips`; its quantity is still consumed from the lot, keeping later matches correct.
#[must_use]
pub fn match_round_trips(fills: &[FillRow]) -> Matched {
    let mut matched = Matched::default();
    let mut lots: VecDeque<Lot> = VecDeque::new();
    for fill in fills.iter().filter(|f| f.qty > Decimal::ZERO) {
        match fill.side {
            OrderSide::Buy => lots.push_back(Lot {
                decision_id: fill.decision_id,
                remaining: fill.qty,
                px: fill.px,
                ts: fill.ts,
                fee_per_share: fill.fee.checked_div(fill.qty),
            }),
            OrderSide::Sell => {
                let sell_fee_per_share = fill.fee.checked_div(fill.qty);
                let mut left = fill.qty;
                while left > Decimal::ZERO {
                    let Some(lot) = lots.front_mut() else {
                        break;
                    };
                    let q = lot.remaining.min(left);
                    if let Some(pnl) = trip_pnl(lot, fill.px, q, sell_fee_per_share) {
                        matched.round_trips.push(RoundTrip {
                            entry_decision_id: lot.decision_id,
                            exit_decision_id: fill.decision_id,
                            qty: q,
                            entry_px: lot.px,
                            exit_px: fill.px,
                            entry_ts: lot.ts,
                            exit_ts: fill.ts,
                            pnl,
                        });
                    }
                    // `q <= remaining` and `q <= left`, so neither subtraction can overflow.
                    lot.remaining = lot.remaining.checked_sub(q).unwrap_or(Decimal::ZERO);
                    left = left.checked_sub(q).unwrap_or(Decimal::ZERO);
                    let exhausted = lot.remaining.is_zero();
                    if exhausted {
                        lots.pop_front();
                    }
                }
                if left > Decimal::ZERO {
                    matched.unmatched_sell_qty = matched
                        .unmatched_sell_qty
                        .checked_add(left)
                        .unwrap_or(Decimal::MAX);
                }
            }
        }
    }
    matched
}

/// P&L of closing `q` shares of `lot` at `exit_px`, net of both fees' shares.
fn trip_pnl(
    lot: &Lot,
    exit_px: Decimal,
    q: Decimal,
    sell_fee_per_share: Option<Decimal>,
) -> Option<Decimal> {
    let gross = exit_px.checked_sub(lot.px)?.checked_mul(q)?;
    let entry_fee = lot.fee_per_share?.checked_mul(q)?;
    let exit_fee = sell_fee_per_share?.checked_mul(q)?;
    gross.checked_sub(entry_fee)?.checked_sub(exit_fee)
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
#[must_use]
pub fn exit_score(trip: &RoundTrip, closes: &[Decimal], risk: Decimal) -> ExitScore {
    if risk <= Decimal::ZERO {
        return ExitScore::default();
    }
    let realized_r = trip.pnl.checked_div(risk).and_then(|r| r.to_f64());

    let (Some(&highest), Some(&lowest)) = (closes.iter().max(), closes.iter().min()) else {
        return ExitScore {
            realized_r,
            ..ExitScore::default()
        };
    };

    let mfe_dollars = highest
        .checked_sub(trip.entry_px)
        .and_then(|d| d.checked_mul(trip.qty));
    let mae_dollars = lowest
        .checked_sub(trip.entry_px)
        .and_then(|d| d.checked_mul(trip.qty));

    let mfe_r = mfe_dollars
        .and_then(|m| m.checked_div(risk))
        .and_then(|r| r.to_f64());
    let mae_r = mae_dollars
        .and_then(|m| m.checked_div(risk))
        .and_then(|r| r.to_f64());
    let capture = mfe_dollars
        .filter(|m| *m > Decimal::ZERO)
        .and_then(|m| trip.pnl.checked_div(m))
        .and_then(|r| r.to_f64());

    ExitScore {
        realized_r,
        mfe_r,
        mae_r,
        capture,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_NANOS: u64 = 86_400_000_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap_or(Decimal::ZERO)
    }

    fn fill(decision: i64, side: OrderSide, qty: &str, px: &str, fee: &str, day: u64) -> FillRow {
        FillRow {
            decision_id: DecisionId(decision),
            side,
            qty: d(qty),
            px: d(px),
            fee: d(fee),
            ts: UnixNanos::from(day * DAY_NANOS),
        }
    }

    /// Asserts an `Option<f64>` is `Some` and within 1e-9 of `expected`.
    fn assert_close(actual: Option<f64>, expected: f64) {
        assert!(
            actual.is_some_and(|a| (a - expected).abs() < 1e-9),
            "expected Some({expected}), got {actual:?}"
        );
    }

    fn trip(qty: &str, entry_px: &str, exit_px: &str, pnl: &str) -> RoundTrip {
        RoundTrip {
            entry_decision_id: DecisionId(1),
            exit_decision_id: DecisionId(2),
            qty: d(qty),
            entry_px: d(entry_px),
            exit_px: d(exit_px),
            entry_ts: UnixNanos::from(1_u64),
            exit_ts: UnixNanos::from(2_u64),
            pnl: d(pnl),
        }
    }

    // ---- execution_score ----

    #[test]
    fn buy_execution_costs_fourteen_bps_over_arrival() {
        // avg = (60 × 100.10 + 40 × 100.20) / 100 = 100.14; (100.14 − 100) / 100 × 10⁴ = 14
        let fills = [
            fill(1, OrderSide::Buy, "60", "100.10", "0.3", 1),
            fill(1, OrderSide::Buy, "40", "100.20", "0.2", 1),
        ];
        let s = execution_score(OrderSide::Buy, &fills, d("100"), d("100"), true);
        assert_close(s.shortfall_bps, 14.0);
        // fees 0.5 / (100 × 100) × 10⁴ = 0.5
        assert_close(s.fees_bps, 0.5);
        assert_close(s.missed_fill_ratio, 0.0);
    }

    #[test]
    fn sell_above_arrival_is_negative_shortfall() {
        let fills = [
            fill(1, OrderSide::Sell, "60", "100.10", "0.3", 1),
            fill(1, OrderSide::Sell, "40", "100.20", "0.2", 1),
        ];
        let s = execution_score(OrderSide::Sell, &fills, d("100"), d("100"), true);
        assert_close(s.shortfall_bps, -14.0);
        assert_close(s.fees_bps, 0.5);
    }

    #[test]
    fn partial_fill_missed_ratio_depends_on_terminal() {
        let fills = [fill(1, OrderSide::Buy, "25", "100", "0", 1)];
        let done = execution_score(OrderSide::Buy, &fills, d("100"), d("100"), true);
        assert_close(done.missed_fill_ratio, 0.75);
        let open = execution_score(OrderSide::Buy, &fills, d("100"), d("100"), false);
        assert_eq!(open.missed_fill_ratio, None);
        // The price-based scores do not depend on terminal state.
        assert_close(open.shortfall_bps, 0.0);
    }

    #[test]
    fn no_fills_gives_none_for_prices_and_full_miss_when_terminal() {
        let s = execution_score(OrderSide::Buy, &[], d("100"), d("100"), true);
        assert_eq!(s.shortfall_bps, None);
        assert_eq!(s.fees_bps, None);
        assert_close(s.missed_fill_ratio, 1.0);
        let open = execution_score(OrderSide::Buy, &[], d("100"), d("100"), false);
        assert_eq!(open.missed_fill_ratio, None);
    }

    #[test]
    fn bad_arrival_ordered_and_overfill() {
        let fills = [fill(1, OrderSide::Buy, "110", "100", "1", 1)];
        for arrival in ["0", "-5"] {
            let s = execution_score(OrderSide::Buy, &fills, d(arrival), d("100"), true);
            assert_eq!(s.shortfall_bps, None);
            assert_eq!(s.fees_bps, None);
        }
        let no_order = execution_score(OrderSide::Buy, &fills, d("100"), d("0"), true);
        assert_eq!(no_order.missed_fill_ratio, None);
        // Filled 110 of 100 ordered: missed is clamped to 0.
        let over = execution_score(OrderSide::Buy, &fills, d("100"), d("100"), true);
        assert_close(over.missed_fill_ratio, 0.0);
    }

    #[test]
    fn non_positive_fills_are_ignored() {
        let fills = [
            fill(1, OrderSide::Buy, "0", "500", "9", 1),
            fill(1, OrderSide::Buy, "-10", "500", "9", 1),
            fill(1, OrderSide::Buy, "100", "100", "0.5", 1),
        ];
        let s = execution_score(OrderSide::Buy, &fills, d("100"), d("100"), true);
        assert_close(s.shortfall_bps, 0.0);
        assert_close(s.fees_bps, 0.5);
        assert_close(s.missed_fill_ratio, 0.0);
    }

    #[test]
    fn overflowing_values_give_none_without_panic() {
        // Notional = MAX × MAX overflows, and filled × arrival overflows.
        let fills = [FillRow {
            qty: Decimal::MAX,
            px: Decimal::MAX,
            ..fill(1, OrderSide::Buy, "1", "1", "0", 1)
        }];
        let s = execution_score(OrderSide::Buy, &fills, d("100"), Decimal::MAX, true);
        assert_eq!(s.shortfall_bps, None);
        assert_eq!(s.fees_bps, None);
        // filled / ordered = 1, so missed is 0 and still computed.
        assert_close(s.missed_fill_ratio, 0.0);
    }

    // ---- match_round_trips ----

    #[test]
    fn single_buy_then_sell_is_one_trip() {
        let fills = [
            fill(1, OrderSide::Buy, "100", "10", "1", 1),
            fill(2, OrderSide::Sell, "100", "12", "1", 2),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 1);
        let t = m.round_trips[0];
        assert_eq!(t.qty, d("100"));
        // (12 − 10) × 100 − 0.01 × 100 − 0.01 × 100 = 198
        assert_eq!(t.pnl, d("198"));
        assert_eq!(t.entry_decision_id, DecisionId(1));
        assert_eq!(t.exit_decision_id, DecisionId(2));
        assert_eq!(t.entry_ts, UnixNanos::from(DAY_NANOS));
        assert_eq!(t.exit_ts, UnixNanos::from(2 * DAY_NANOS));
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
    }

    #[test]
    fn fifo_across_two_lots_leaves_the_rest_open() {
        let fills = [
            fill(1, OrderSide::Buy, "100", "10", "0", 1),
            fill(2, OrderSide::Buy, "50", "11", "0", 1),
            fill(3, OrderSide::Sell, "120", "12", "0", 2),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 2);
        assert_eq!(m.round_trips[0].entry_decision_id, DecisionId(1));
        assert_eq!(m.round_trips[0].qty, d("100"));
        assert_eq!(m.round_trips[0].pnl, d("200"));
        assert_eq!(m.round_trips[1].entry_decision_id, DecisionId(2));
        assert_eq!(m.round_trips[1].qty, d("20"));
        assert_eq!(m.round_trips[1].pnl, d("20"));
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
    }

    #[test]
    fn later_sell_closes_the_remaining_open_lot() {
        let fills = [
            fill(1, OrderSide::Buy, "100", "10", "0", 1),
            fill(2, OrderSide::Buy, "50", "11", "0", 1),
            fill(3, OrderSide::Sell, "120", "12", "0", 2),
            fill(4, OrderSide::Sell, "30", "9", "0", 3),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 3);
        let last = m.round_trips[2];
        assert_eq!(last.entry_decision_id, DecisionId(2));
        assert_eq!(last.exit_decision_id, DecisionId(4));
        assert_eq!(last.qty, d("30"));
        assert_eq!(last.pnl, d("-60"));
    }

    #[test]
    fn fees_are_split_by_share() {
        let fills = [
            fill(1, OrderSide::Buy, "100", "10", "2", 1),
            fill(2, OrderSide::Sell, "40", "11", "0.4", 2),
            fill(3, OrderSide::Sell, "60", "11", "0.6", 2),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 2);
        // 40 − 0.8 − 0.4 = 38.8
        assert_eq!(m.round_trips[0].qty, d("40"));
        assert_eq!(m.round_trips[0].pnl, d("38.8"));
        // 60 − 1.2 − 0.6 = 58.2
        assert_eq!(m.round_trips[1].qty, d("60"));
        assert_eq!(m.round_trips[1].pnl, d("58.2"));
        assert_eq!(
            m.round_trips[0].pnl + m.round_trips[1].pnl,
            d("100") - d("2") - d("1")
        );
    }

    #[test]
    fn sell_without_open_lot_is_unmatched() {
        let fills = [fill(1, OrderSide::Sell, "30", "10", "0", 1)];
        let m = match_round_trips(&fills);
        assert!(m.round_trips.is_empty());
        assert_eq!(m.unmatched_sell_qty, d("30"));
    }

    #[test]
    fn sell_larger_than_open_lots_leaves_the_rest_unmatched() {
        let fills = [
            fill(1, OrderSide::Buy, "50", "10", "0", 1),
            fill(2, OrderSide::Sell, "80", "11", "0", 2),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 1);
        assert_eq!(m.round_trips[0].qty, d("50"));
        assert_eq!(m.unmatched_sell_qty, d("30"));
    }

    #[test]
    fn empty_input_and_buys_only() {
        assert_eq!(match_round_trips(&[]), Matched::default());
        let fills = [fill(1, OrderSide::Buy, "100", "10", "1", 1)];
        let m = match_round_trips(&fills);
        assert!(m.round_trips.is_empty());
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
    }

    #[test]
    fn non_positive_fills_are_ignored_in_matching() {
        let fills = [
            fill(1, OrderSide::Buy, "0", "10", "0", 1),
            fill(2, OrderSide::Buy, "100", "10", "0", 1),
            fill(3, OrderSide::Sell, "0", "12", "0", 2),
            fill(3, OrderSide::Sell, "-5", "12", "0", 2),
            fill(3, OrderSide::Sell, "100", "12", "0", 2),
        ];
        let m = match_round_trips(&fills);
        assert_eq!(m.round_trips.len(), 1);
        assert_eq!(m.round_trips[0].entry_decision_id, DecisionId(2));
        assert_eq!(m.round_trips[0].pnl, d("200"));
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
    }

    #[test]
    fn conservation_over_a_scripted_sequence() {
        let fills = [
            fill(1, OrderSide::Buy, "100", "10", "0", 1),
            fill(2, OrderSide::Buy, "50", "10.5", "0", 1),
            fill(3, OrderSide::Sell, "40", "11", "0", 2),
            fill(4, OrderSide::Buy, "30", "11", "0", 2),
            fill(5, OrderSide::Sell, "90", "12", "0", 3),
            fill(6, OrderSide::Buy, "25", "12", "0", 3),
            fill(7, OrderSide::Sell, "20", "11", "0", 4),
            fill(8, OrderSide::Buy, "10", "11", "0", 4),
            fill(9, OrderSide::Sell, "3", "11", "0", 5),
            fill(10, OrderSide::Sell, "17", "11", "0", 5),
        ];
        let m = match_round_trips(&fills);
        let bought = d("215");
        let sold = d("170");
        // Open lots after the script: 10 of decision 4, 25 of decision 6, 10 of decision 8.
        let open = d("45");

        let traded: Decimal = m.round_trips.iter().map(|t| t.qty).sum();
        assert_eq!(traded, sold);
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
        assert_eq!(traded + open, bought);
    }

    // ---- exit_score ----

    #[test]
    fn exit_scores_for_a_winning_trip() {
        let t = trip("100", "10", "12", "198");
        let closes = [d("10"), d("13"), d("9"), d("12")];
        let s = exit_score(&t, &closes, d("50"));
        assert_close(s.realized_r, 3.96);
        // MFE = (13 − 10) × 100 = 300; MAE = (9 − 10) × 100 = −100
        assert_close(s.mfe_r, 6.0);
        assert_close(s.mae_r, -2.0);
        assert_close(s.capture, 0.66);
    }

    #[test]
    fn no_positive_excursion_gives_no_capture() {
        let t = trip("100", "10", "12", "198");
        let s = exit_score(&t, &[d("10"), d("9"), d("8")], d("50"));
        assert_eq!(s.capture, None);
        assert_close(s.mfe_r, 0.0);
        assert_close(s.mae_r, -4.0);
    }

    #[test]
    fn non_positive_risk_gives_all_none() {
        let t = trip("100", "10", "12", "198");
        for risk in ["0", "-1"] {
            let s = exit_score(&t, &[d("13")], d(risk));
            assert_eq!(s, ExitScore::default());
        }
    }

    #[test]
    fn empty_closes_keep_only_realized_r() {
        let t = trip("100", "10", "12", "198");
        let s = exit_score(&t, &[], d("50"));
        assert_close(s.realized_r, 3.96);
        assert_eq!(s.mfe_r, None);
        assert_eq!(s.mae_r, None);
        assert_eq!(s.capture, None);
    }

    #[test]
    fn losing_trip_has_negative_realized_r_and_capture() {
        // Bought at 10, sold at 9, but it rose to 11 in between: MFE 100 > 0.
        let t = trip("100", "10", "9", "-100");
        let s = exit_score(&t, &[d("10"), d("11"), d("9")], d("50"));
        assert_close(s.realized_r, -2.0);
        assert_close(s.mfe_r, 2.0);
        assert_close(s.capture, -1.0);
    }

    #[test]
    fn overflowing_excursion_gives_none_without_panic() {
        // MFE = MAX × 1 dollars; MAX / risk with risk = 1e-28 overflows, so mfe_r is None.
        // realized_r = 1 / 1e-28 = 1e28 still fits.
        let t = trip("1", "0", "1", "1");
        let tiny = d("0.0000000000000000000000000001");
        let s = exit_score(&t, &[Decimal::MAX], tiny);
        assert_eq!(s.mfe_r, None);
        assert_close(s.realized_r, 1e28);
    }

    #[test]
    fn overflowing_pnl_is_not_emitted_without_panic() {
        // (MAX − MIN) × 1 overflows, so the trip is left out; the lot is still consumed.
        let fills = [
            FillRow {
                px: Decimal::MIN,
                ..fill(1, OrderSide::Buy, "1", "0", "0", 1)
            },
            FillRow {
                px: Decimal::MAX,
                ..fill(2, OrderSide::Sell, "1", "0", "0", 2)
            },
        ];
        let m = match_round_trips(&fills);
        assert!(m.round_trips.is_empty());
        assert_eq!(m.unmatched_sell_qty, Decimal::ZERO);
    }
}
