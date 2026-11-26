//! The nightly scoring job, part two: execution and exit scores. Rules in
//! `docs/phases/phase-5.md`, "Trade-level scores".
//!
//! The scoring arithmetic lives in [`crate::execution`]; this module only loads the rows,
//! groups them, calls those functions and stores the results. Each run reads every table it
//! needs once (a fixed number of queries whatever the number of decisions) and writes all its
//! rows with one `insert_many`. A decision is skipped once its marker metric exists for
//! [`EVAL_VERSION`], so a second run writes nothing.

use std::collections::{BTreeMap, BTreeSet};

use nisse_bars::BarReader;
use nisse_core::{
    DecisionId, EvalScore, SubjectType,
    nautilus::{OrderSide, Symbol, UnixNanos},
};
use nisse_store::tables::{
    eval_score,
    scoring::{self, DecisionFill, DecisionOrders},
};
use rust_decimal::Decimal;
use tokio_postgres::GenericClient;

use crate::EvalError;
use crate::execution::{FillRow, RoundTrip, execution_score, exit_score, match_round_trips};

/// The `eval_version` written with every score this job produces.
pub const EVAL_VERSION: &str = "score-v1";

/// The marker of the execution scores: written for every scored decision.
const METRIC_MISSED_FILL_RATIO: &str = "missed_fill_ratio";
const METRIC_SHORTFALL_BPS: &str = "shortfall_bps";
const METRIC_FEES_BPS: &str = "fees_bps";
/// The marker of the exit scores: written for every entry decision that has been scored.
const METRIC_REALIZED_R: &str = "realized_r";
const METRIC_MFE_R: &str = "mfe_r";
const METRIC_MAE_R: &str = "mae_r";
const METRIC_CAPTURE: &str = "capture";

/// What one run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExecutionReport {
    /// Decisions that got their execution scores in this run. Includes the decisions counted in
    /// `no_arrival`.
    pub decisions_scored: u64,
    /// Decisions with orders still open, left for a later run.
    pub waiting: u64,
    /// Decisions skipped because they have no arrival price. They are still scored (see
    /// [`score_execution`]), so each is also in `decisions_scored`.
    pub no_arrival: u64,
}

/// Scores the execution of every decision whose orders were sent and are all terminal, and that
/// has no `missed_fill_ratio` score yet (that metric is the marker: it is always written). Writes
/// `missed_fill_ratio` and, when they can be computed, `shortfall_bps` and `fees_bps`
/// (`subject_type` decision, `eval_version` [`EVAL_VERSION`]), all in one `insert_many`.
///
/// A decision with no `close` feature has no arrival price: it is counted in `no_arrival` and
/// its `missed_fill_ratio` is still written, with the two price scores left out. A decision
/// whose `missed_fill_ratio` cannot be computed at all (its ordered quantity is zero, or the
/// arithmetic overflowed) writes nothing and is retried by a later run.
///
/// Decisions whose orders have not been sent (only an intent row) are ignored and not counted.
pub async fn score_execution<C: GenericClient>(
    client: &C,
    now: UnixNanos,
) -> Result<ExecutionReport, EvalError> {
    let orders = scoring::decision_orders(client).await?;
    let fills = scoring::fills(client).await?;
    let scored = eval_score::scored_subjects(
        client,
        SubjectType::Decision,
        METRIC_MISSED_FILL_RATIO,
        EVAL_VERSION,
    )
    .await?;

    let mut report = ExecutionReport::default();
    let mut todo: Vec<&DecisionOrders> = Vec::new();
    for order in &orders {
        if !order.sent {
            continue;
        }
        if !order.all_terminal {
            report.waiting += 1;
            continue;
        }
        if scored.binary_search(&order.decision_id.0).is_err() {
            todo.push(order);
        }
    }
    if todo.is_empty() {
        return Ok(report);
    }

    let ids: Vec<DecisionId> = todo.iter().map(|order| order.decision_id).collect();
    let arrivals: BTreeMap<DecisionId, Decimal> = scoring::arrival_prices(client, &ids)
        .await?
        .into_iter()
        .collect();

    let mut by_decision: BTreeMap<DecisionId, Vec<FillRow>> = BTreeMap::new();
    for fill in &fills {
        by_decision
            .entry(fill.decision_id)
            .or_default()
            .push(fill_row(fill));
    }

    let no_fills: &[FillRow] = &[];
    let mut rows: Vec<EvalScore> = Vec::with_capacity(todo.len() * 3);
    for order in todo {
        let decision_fills = by_decision
            .get(&order.decision_id)
            .map_or(no_fills, Vec::as_slice);
        let arrival = arrivals.get(&order.decision_id).copied();
        if arrival.is_none() {
            report.no_arrival += 1;
        }
        // Without an arrival price the price scores come out `None`, and the ratio does not
        // depend on it.
        let score = execution_score(
            order.side,
            decision_fills,
            arrival.unwrap_or_default(),
            order.ordered_qty,
            order.all_terminal,
        );
        let Some(missed) = score.missed_fill_ratio else {
            continue;
        };
        report.decisions_scored += 1;
        rows.push(score_row(
            order.decision_id,
            METRIC_MISSED_FILL_RATIO,
            missed,
            now,
        ));
        if let Some(value) = score.shortfall_bps {
            rows.push(score_row(
                order.decision_id,
                METRIC_SHORTFALL_BPS,
                value,
                now,
            ));
        }
        if let Some(value) = score.fees_bps {
            rows.push(score_row(order.decision_id, METRIC_FEES_BPS, value, now));
        }
    }

    eval_score::insert_many(client, &rows).await?;
    Ok(report)
}

/// What one run of [`score_exits`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitReport {
    /// Entry decisions that got their exit scores in this run.
    pub entries_scored: u64,
    /// Entry decisions whose shares are not all sold yet.
    pub open: u64,
    /// Shares sold with no matching buy, summed over symbols (should be zero).
    pub unmatched_sell_qty: Decimal,
}

/// One entry decision's buys, with the round trips that closed them.
struct Entry {
    symbol: Symbol,
    /// Sum of the buy fill quantities; `None` if the sum overflowed.
    bought: Option<Decimal>,
    /// The round trips that closed this entry's lots, in the order they closed.
    trips: Vec<RoundTrip>,
}

/// An entry ready to be scored: its combined round trip and the R unit for that trip.
struct Candidate {
    decision: DecisionId,
    symbol: Symbol,
    trip: RoundTrip,
    risk: Decimal,
}

/// Scores every entry decision whose bought shares have all been sold and that has no
/// `realized_r` score yet. Writes `realized_r` (the marker, always written when the risk is
/// known) and, when they can be computed, `mfe_r`, `mae_r` and `capture`
/// (`subject_type` decision, `eval_version` [`EVAL_VERSION`]), all in one `insert_many`.
///
/// Round trips are matched per symbol, first in first out, and the entry's round trips are
/// combined into one: the summed quantity, volume-weighted prices, the summed P&L and the
/// entry's first and the exit's last timestamps. The R unit is the entry decision's
/// `risk_per_trade` figure scaled to the bought share of its order. An entry with no such
/// figure (or no `ordered_qty`) has no R unit: it is skipped, counted nowhere and not marked,
/// so it is looked at again on a later run.
///
/// The closes used for the excursions are the symbol's daily bars from the entry's bar through
/// the exit's bar (the last bar at or before the entry, when the entry is not itself a bar
/// time). Bars are read once, for every symbol that needs them.
pub async fn score_exits<C: GenericClient>(
    client: &C,
    bars: &BarReader,
    now: UnixNanos,
) -> Result<ExitReport, EvalError> {
    let fills = scoring::fills(client).await?;
    let risks: BTreeMap<DecisionId, Decimal> =
        scoring::entry_risks(client).await?.into_iter().collect();
    let ordered: BTreeMap<DecisionId, Decimal> = scoring::decision_orders(client)
        .await?
        .into_iter()
        .map(|order| (order.decision_id, order.ordered_qty))
        .collect();
    let scored = eval_score::scored_subjects(
        client,
        SubjectType::Decision,
        METRIC_REALIZED_R,
        EVAL_VERSION,
    )
    .await?;

    let mut report = ExitReport::default();

    // Every entry is a decision with buy fills. Its fills are all on one symbol.
    let mut entries: BTreeMap<DecisionId, Entry> = BTreeMap::new();
    for fill in fills
        .iter()
        .filter(|f| f.side == OrderSide::Buy && f.qty > Decimal::ZERO)
    {
        let entry = entries.entry(fill.decision_id).or_insert_with(|| Entry {
            symbol: fill.symbol,
            bought: Some(Decimal::ZERO),
            trips: Vec::new(),
        });
        entry.bought = entry.bought.and_then(|b| b.checked_add(fill.qty));
    }

    // Match each symbol's fills separately, in fill order.
    let mut by_symbol: BTreeMap<Symbol, Vec<FillRow>> = BTreeMap::new();
    for fill in &fills {
        by_symbol
            .entry(fill.symbol)
            .or_default()
            .push(fill_row(fill));
    }
    for symbol_fills in by_symbol.values() {
        let matched = match_round_trips(symbol_fills);
        report.unmatched_sell_qty = report
            .unmatched_sell_qty
            .checked_add(matched.unmatched_sell_qty)
            .unwrap_or(Decimal::MAX);
        for trip in matched.round_trips {
            if let Some(entry) = entries.get_mut(&trip.entry_decision_id) {
                entry.trips.push(trip);
            }
        }
    }

    // Decide which entries are scored now, and how.
    let mut candidates: Vec<Candidate> = Vec::new();
    for (&decision, entry) in &entries {
        let (Some(bought), Some(closed)) = (entry.bought, closed_qty(&entry.trips)) else {
            continue;
        };
        if closed < bought {
            report.open += 1;
            continue;
        }
        if scored.binary_search(&decision.0).is_ok() {
            continue;
        }
        let (Some(trip), Some(&entry_risk), Some(&ordered_qty)) = (
            combine(&entry.trips),
            risks.get(&decision),
            ordered.get(&decision),
        ) else {
            continue;
        };
        let Some(risk) = scaled_risk(entry_risk, bought, ordered_qty) else {
            continue;
        };
        candidates.push(Candidate {
            decision,
            symbol: entry.symbol,
            trip,
            risk,
        });
    }
    if candidates.is_empty() {
        return Ok(report);
    }

    // One bar read for every symbol that needs closes.
    let symbols: BTreeSet<Symbol> = candidates.iter().map(|c| c.symbol).collect();
    let names: Vec<&str> = symbols.iter().map(Symbol::as_str).collect();
    let bar_rows = bars.read(
        &names,
        UnixNanos::from(0),
        UnixNanos::from(i64::MAX.unsigned_abs()),
    )?;
    let mut series: BTreeMap<Symbol, Vec<(UnixNanos, Decimal)>> = BTreeMap::new();
    for bar in &bar_rows {
        series
            .entry(bar.bar_type.instrument_id().symbol)
            .or_default()
            .push((bar.ts_event, bar.close.as_decimal()));
    }

    let mut rows: Vec<EvalScore> = Vec::with_capacity(candidates.len() * 4);
    let mut closes: Vec<Decimal> = Vec::new();
    for candidate in &candidates {
        closes.clear();
        if let Some(bars_of) = series.get(&candidate.symbol) {
            closes_between(
                bars_of,
                candidate.trip.entry_ts,
                candidate.trip.exit_ts,
                &mut closes,
            );
        }
        let score = exit_score(&candidate.trip, &closes, candidate.risk);
        let Some(realized) = score.realized_r else {
            continue;
        };
        report.entries_scored += 1;
        rows.push(score_row(
            candidate.decision,
            METRIC_REALIZED_R,
            realized,
            now,
        ));
        let optional = [
            (METRIC_MFE_R, score.mfe_r),
            (METRIC_MAE_R, score.mae_r),
            (METRIC_CAPTURE, score.capture),
        ];
        for (metric, value) in optional {
            if let Some(value) = value {
                rows.push(score_row(candidate.decision, metric, value, now));
            }
        }
    }

    eval_score::insert_many(client, &rows).await?;
    Ok(report)
}

/// A store fill as the scoring arithmetic takes it.
fn fill_row(fill: &DecisionFill) -> FillRow {
    FillRow {
        decision_id: fill.decision_id,
        side: fill.side,
        qty: fill.qty,
        px: fill.px,
        fee: fill.fee,
        ts: fill.ts,
    }
}

/// One `eval_score` row for a decision.
fn score_row(decision: DecisionId, metric: &str, value: f64, ts: UnixNanos) -> EvalScore {
    EvalScore {
        subject_id: decision.0,
        subject_type: SubjectType::Decision,
        metric: metric.to_owned(),
        value,
        eval_version: EVAL_VERSION.to_owned(),
        ts,
    }
}

/// The total quantity closed by `trips`; `None` if the sum overflows.
fn closed_qty(trips: &[RoundTrip]) -> Option<Decimal> {
    trips
        .iter()
        .try_fold(Decimal::ZERO, |sum, trip| sum.checked_add(trip.qty))
}

/// One round trip standing for all of `trips`: the summed quantity, the quantity-weighted
/// entry and exit prices, the summed P&L, the earliest entry and the latest exit. `None` when
/// there are no trips or an intermediate value overflows. The exit decision is that of the last
/// trip.
fn combine(trips: &[RoundTrip]) -> Option<RoundTrip> {
    let first = trips.first()?;
    let last = trips.last()?;
    let mut qty = Decimal::ZERO;
    let mut entry_notional = Decimal::ZERO;
    let mut exit_notional = Decimal::ZERO;
    let mut pnl = Decimal::ZERO;
    let mut entry_ts = first.entry_ts;
    let mut exit_ts = first.exit_ts;
    for trip in trips {
        qty = qty.checked_add(trip.qty)?;
        entry_notional = entry_notional.checked_add(trip.entry_px.checked_mul(trip.qty)?)?;
        exit_notional = exit_notional.checked_add(trip.exit_px.checked_mul(trip.qty)?)?;
        pnl = pnl.checked_add(trip.pnl)?;
        entry_ts = entry_ts.min(trip.entry_ts);
        exit_ts = exit_ts.max(trip.exit_ts);
    }
    if qty <= Decimal::ZERO {
        return None;
    }
    Some(RoundTrip {
        entry_decision_id: first.entry_decision_id,
        exit_decision_id: last.exit_decision_id,
        qty,
        entry_px: entry_notional.checked_div(qty)?,
        exit_px: exit_notional.checked_div(qty)?,
        entry_ts,
        exit_ts,
        pnl,
    })
}

/// The R unit of a lot: the entry order's recorded risk scaled to the bought share of the order,
/// `entry_risk × bought / ordered_qty`. `None` when the order quantity is not positive or the
/// arithmetic overflows.
fn scaled_risk(entry_risk: Decimal, bought: Decimal, ordered_qty: Decimal) -> Option<Decimal> {
    if ordered_qty <= Decimal::ZERO {
        return None;
    }
    entry_risk.checked_mul(bought)?.checked_div(ordered_qty)
}

/// Appends to `out` the closes of one symbol's bars (`bars`, in time order) from the entry's
/// bar through the exit's bar: the bars with `entry <= ts <= exit`, plus the last bar at or
/// before `entry` when `entry` is not itself a bar time.
fn closes_between(
    bars: &[(UnixNanos, Decimal)],
    entry: UnixNanos,
    exit: UnixNanos,
    out: &mut Vec<Decimal>,
) {
    let lo = bars.partition_point(|(ts, _)| *ts < entry);
    let at_entry = bars.get(lo).is_some_and(|(ts, _)| *ts == entry);
    // When `entry` is not a bar time, the bar before `lo` (if any) is the last one at or before it.
    let first = if at_entry { lo } else { lo.saturating_sub(1) };
    let end = bars.partition_point(|(ts, _)| *ts <= exit);
    if let Some(window) = bars.get(first..end) {
        out.extend(window.iter().map(|(_, close)| *close));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u64) -> UnixNanos {
        UnixNanos::from(n)
    }

    fn d(s: &str) -> Decimal {
        s.parse().unwrap_or(Decimal::ZERO)
    }

    #[test]
    fn closes_include_the_last_bar_before_an_off_bar_entry() {
        let bars = [
            (t(10), d("1")),
            (t(20), d("2")),
            (t(30), d("3")),
            (t(40), d("4")),
        ];
        let mut out = Vec::new();
        closes_between(&bars, t(25), t(40), &mut out);
        assert_eq!(out, vec![d("2"), d("3"), d("4")]);
    }

    #[test]
    fn closes_start_at_an_on_bar_entry() {
        let bars = [(t(10), d("1")), (t(20), d("2")), (t(30), d("3"))];
        let mut out = Vec::new();
        closes_between(&bars, t(20), t(30), &mut out);
        assert_eq!(out, vec![d("2"), d("3")]);
    }

    #[test]
    fn closes_from_before_the_first_bar_start_at_the_first_bar() {
        let bars = [(t(10), d("1")), (t(20), d("2"))];
        let mut out = Vec::new();
        closes_between(&bars, t(5), t(20), &mut out);
        assert_eq!(out, vec![d("1"), d("2")]);
    }

    #[test]
    fn closes_are_empty_when_no_bar_falls_in_range() {
        let bars = [(t(10), d("1")), (t(20), d("2"))];
        let mut out = Vec::new();
        closes_between(&bars, t(12), t(15), &mut out);
        assert_eq!(out, vec![d("1")]);
        let mut none = Vec::new();
        closes_between(&[], t(12), t(15), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn scaled_risk_takes_the_bought_share() {
        assert_eq!(scaled_risk(d("50"), d("40"), d("100")), Some(d("20")));
        assert_eq!(scaled_risk(d("50"), d("40"), d("0")), None);
        assert_eq!(scaled_risk(d("50"), d("40"), d("-1")), None);
        assert_eq!(scaled_risk(Decimal::MAX, Decimal::MAX, d("1")), None);
    }
}
