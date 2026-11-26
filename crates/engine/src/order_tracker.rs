//! In-memory order state, driven by the broker's cumulative figures.
//!
//! The broker is the source of truth. Every update or snapshot carries the order's cumulative
//! filled quantity, and fills are derived from how far that figure moved, so a message that is
//! duplicated, replayed after a reconnect, or delivered out of order can never be counted
//! twice, and a message that was missed is made up by the next one.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Write;

use nisse_core::{
    BrokerOrder, Fill, FillDetail, OrderIntent, OrderState, RestoredOrder,
    nautilus::{ClientOrderId, Price, Quantity, TradeId, UnixNanos, VenueOrderId},
};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::EngineError;

/// Decimal places of the synthetic fill price.
const SYNTHETIC_PX_DP: u32 = 9;

/// Characters taken from the venue order id (dashes removed) for a synthetic trade id.
const SYNTHETIC_ID_VENUE_CHARS: usize = 18;

/// What the engine knows about one of its orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedOrder {
    pub intent: OrderIntent,
    /// Unknown until the broker first reports the order.
    pub venue_order_id: Option<VenueOrderId>,
    pub state: OrderState,
    pub cum_filled_qty: Quantity,
    pub avg_fill_px: Option<Price>,
    pub last_update_ts: UnixNanos,
    /// Set while the order is *presumed* never to have reached the broker (a lookup by client
    /// order id found nothing). The state is `Rejected`, but the verdict is provisional: any
    /// broker message about the order overturns it, and reconciliation keeps looking the
    /// order up until the caller's recheck window has passed.
    pub never_arrived_at: Option<UnixNanos>,
}

/// The effect of one broker update on a tracked order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Applied {
    /// `(before, after)` when the state changed.
    pub state_change: Option<(OrderState, OrderState)>,
    /// The newly filled quantity, when the cumulative figure moved forward.
    pub fill: Option<Fill>,
}

/// All orders the engine has intended, keyed by client order id. A `BTreeMap` so iteration
/// order is deterministic.
#[derive(Debug, Default)]
pub struct OrderTracker {
    orders: BTreeMap<ClientOrderId, TrackedOrder>,
}

impl OrderTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts tracking an order in state `Intent` with nothing filled. Call it when the
    /// intent is recorded, before submission. Errors with `DuplicateOrder` if the client
    /// order id is already tracked.
    pub fn track_intent(&mut self, intent: OrderIntent) -> Result<(), EngineError> {
        match self.orders.entry(intent.client_order_id) {
            Entry::Occupied(_) => Err(EngineError::DuplicateOrder(intent.client_order_id)),
            Entry::Vacant(slot) => {
                slot.insert(TrackedOrder {
                    intent,
                    venue_order_id: None,
                    state: OrderState::Intent,
                    cum_filled_qty: Quantity::zero(intent.qty.precision),
                    avg_fill_px: None,
                    last_update_ts: intent.ts,
                    never_arrived_at: None,
                });
                Ok(())
            }
        }
    }

    /// Restores an order from the store after a restart, with the cumulative figures already
    /// recorded for it, so the first broker snapshot only yields fills that were not recorded
    /// before the crash. Errors with `DuplicateOrder` if the id is already tracked.
    pub fn restore(&mut self, order: TrackedOrder) -> Result<(), EngineError> {
        match self.orders.entry(order.intent.client_order_id) {
            Entry::Occupied(_) => Err(EngineError::DuplicateOrder(order.intent.client_order_id)),
            Entry::Vacant(slot) => {
                slot.insert(order);
                Ok(())
            }
        }
    }

    /// Restores every order the store reports as possibly live. See [`Self::restore`].
    pub fn restore_all(&mut self, orders: &[RestoredOrder]) -> Result<(), EngineError> {
        for order in orders {
            self.restore(TrackedOrder {
                intent: order.intent,
                venue_order_id: order.venue_order_id,
                state: order.state,
                cum_filled_qty: order.cum_filled_qty,
                avg_fill_px: order.avg_fill_px,
                last_update_ts: order.last_update_ts,
                // A rejected order the broker never acknowledged may be a never-arrived
                // verdict that was still provisional when the engine stopped. Treating every
                // such order as provisional costs a few lookups and loses nothing.
                never_arrived_at: (order.state == OrderState::Rejected
                    && order.venue_order_id.is_none())
                .then_some(order.last_update_ts),
            })?;
        }
        Ok(())
    }

    /// Applies one message from the broker's order stream. `fill` is the single execution the
    /// message reported, if any; `ts` is the event time. Stream messages can arrive late or
    /// out of order, so one that reports no new fill never moves the state backwards and
    /// never reopens a terminal order. The rules are in `docs/phases/phase-1.md`, "Order
    /// tracker rules". Errors with `UnknownOrder` when the client order id is not tracked.
    pub fn apply(
        &mut self,
        order: &BrokerOrder,
        fill: Option<&FillDetail>,
        ts: UnixNanos,
    ) -> Result<Applied, EngineError> {
        self.apply_from(order, fill, ts, false)
    }

    /// Applies a REST snapshot of an order (a submit response, an open-orders entry, a lookup
    /// by client order id). A snapshot is the broker's current word, so unless it is older
    /// than what we know (lower cumulative fill) its state is adopted even when that moves
    /// the order "backwards": a rejected cancel returns `PendingCancel` to `Accepted`, and an
    /// order wrongly marked never-arrived comes back to life.
    pub fn apply_snapshot(&mut self, order: &BrokerOrder) -> Result<Applied, EngineError> {
        self.apply_from(order, None, order.ts, true)
    }

    fn apply_from(
        &mut self,
        order: &BrokerOrder,
        fill: Option<&FillDetail>,
        ts: UnixNanos,
        authoritative: bool,
    ) -> Result<Applied, EngineError> {
        let client_order_id = order.client_order_id;
        let tracked = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| EngineError::UnknownOrder(client_order_id))?;

        let delta = order
            .cum_filled_qty
            .as_decimal()
            .checked_sub(tracked.cum_filled_qty.as_decimal())
            .ok_or_else(|| inconsistent(client_order_id, "cumulative fill quantity overflows"))?;

        // Any broker message about an order presumed never-arrived proves the broker has it,
        // so it is treated like a snapshot and overturns the verdict.
        let unconfirmed = tracked.never_arrived_at.is_some();
        // A snapshot stamped before our latest broker-sourced update is a lagging read (for
        // example an open-orders list that has not caught up with a cancel the stream already
        // delivered). Local timestamps (an intent, a never-arrived verdict) do not count.
        let lagging = authoritative
            && !unconfirmed
            && tracked.venue_order_id.is_some()
            && ts < tracked.last_update_ts;
        let authoritative = (authoritative && !lagging) || unconfirmed;

        // Rule 1: the broker reports less filled than we hold. From the stream or a lagging
        // read that is just an old message. From a current snapshot it is a contradiction the
        // tracker cannot resolve (a busted trade, or a bad restore): report it.
        if delta < Decimal::ZERO {
            if authoritative {
                return Err(inconsistent(
                    client_order_id,
                    "broker cumulative fill is lower than recorded",
                ));
            }
            return Ok(Applied::default());
        }

        // Rule 3: no new fill. A stream message is ignored when we are already terminal or its
        // state is behind ours (it is late); a snapshot is the broker's current word and wins.
        if delta.is_zero() {
            let stale = tracked.state.is_terminal() || order.state.rank() < tracked.state.rank();
            if (stale && !authoritative) || lagging {
                return Ok(Applied::default());
            }
            let before = tracked.state;
            tracked.state = order.state;
            tracked.venue_order_id = Some(order.venue_order_id);
            tracked.last_update_ts = ts;
            tracked.never_arrived_at = None;
            return Ok(Applied {
                state_change: state_change(before, order.state),
                fill: None,
            });
        }

        // Rule 2: a new fill. Everything that can fail is computed before any field changes, so
        // an error leaves the tracked order as it was.
        let precision = order
            .cum_filled_qty
            .precision
            .max(tracked.cum_filled_qty.precision);
        let qty = Quantity::from_decimal_dp(delta, precision)
            .map_err(|_| inconsistent(client_order_id, "fill quantity is not representable"))?;
        let (px, trade_id) = match fill {
            Some(detail) if detail.qty.as_decimal() == delta => (detail.px, detail.trade_id),
            _ => (
                synthetic_px(tracked, order, delta)?,
                synthetic_trade_id(order)?,
            ),
        };
        let emitted = Fill {
            client_order_id,
            trade_id,
            px,
            qty,
            ts,
        };

        let before = tracked.state;
        tracked.cum_filled_qty = order.cum_filled_qty;
        tracked.avg_fill_px = order.avg_fill_px;
        tracked.venue_order_id = Some(order.venue_order_id);
        tracked.last_update_ts = ts;
        tracked.state = order.state;
        tracked.never_arrived_at = None;
        Ok(Applied {
            state_change: state_change(before, order.state),
            fill: Some(emitted),
        })
    }

    /// Marks a tracked order `Rejected` for a definite reason: the broker refused it, or it
    /// was never sent because the intent could not be recorded. No-op with
    /// `state_change: None` if the order is already terminal.
    pub fn mark_rejected(
        &mut self,
        client_order_id: ClientOrderId,
        ts: UnixNanos,
    ) -> Result<Applied, EngineError> {
        self.reject(client_order_id, ts, false)
    }

    /// Marks a tracked order `Rejected` because the broker has no record of it, so the submit
    /// is presumed never to have arrived. The verdict is provisional (see
    /// [`TrackedOrder::never_arrived_at`]). No-op if the order is already terminal.
    pub fn mark_never_arrived(
        &mut self,
        client_order_id: ClientOrderId,
        ts: UnixNanos,
    ) -> Result<Applied, EngineError> {
        self.reject(client_order_id, ts, true)
    }

    /// Makes a never-arrived verdict final: the order is no longer looked up. No-op if the
    /// order carries no such verdict.
    pub fn settle_never_arrived(
        &mut self,
        client_order_id: ClientOrderId,
    ) -> Result<(), EngineError> {
        let tracked = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| EngineError::UnknownOrder(client_order_id))?;
        tracked.never_arrived_at = None;
        Ok(())
    }

    fn reject(
        &mut self,
        client_order_id: ClientOrderId,
        ts: UnixNanos,
        provisional: bool,
    ) -> Result<Applied, EngineError> {
        let tracked = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| EngineError::UnknownOrder(client_order_id))?;
        if tracked.state.is_terminal() {
            return Ok(Applied::default());
        }
        let before = tracked.state;
        tracked.state = OrderState::Rejected;
        tracked.last_update_ts = ts;
        tracked.never_arrived_at = provisional.then_some(ts);
        Ok(Applied {
            state_change: state_change(before, OrderState::Rejected),
            fill: None,
        })
    }

    #[must_use]
    pub fn get(&self, client_order_id: ClientOrderId) -> Option<&TrackedOrder> {
        self.orders.get(&client_order_id)
    }

    /// Orders that are not in a terminal state, in client order id order.
    pub fn open_orders(&self) -> impl Iterator<Item = &TrackedOrder> {
        self.orders.values().filter(|o| !o.state.is_terminal())
    }

    /// Orders that reconciliation must still look up at the broker: the open ones, plus those
    /// with a provisional never-arrived verdict. In client order id order.
    pub fn unresolved_orders(&self) -> impl Iterator<Item = &TrackedOrder> {
        self.orders
            .values()
            .filter(|o| !o.state.is_terminal() || o.never_arrived_at.is_some())
    }
}

fn inconsistent(client_order_id: ClientOrderId, reason: &'static str) -> EngineError {
    EngineError::InconsistentBrokerData {
        client_order_id,
        reason,
    }
}

/// `Some((before, after))` only when the state actually changed.
fn state_change(before: OrderState, after: OrderState) -> Option<(OrderState, OrderState)> {
    (before != after).then_some((before, after))
}

/// Average price of the `delta` quantity that arrived between the tracked and the broker's
/// cumulative figures: `(avg_new·cum_new − avg_old·cum_old) / delta`, rounded half-even to
/// 9 decimal places.
fn synthetic_px(
    tracked: &TrackedOrder,
    order: &BrokerOrder,
    delta: Decimal,
) -> Result<Price, EngineError> {
    let cid = order.client_order_id;
    let overflow = || inconsistent(cid, "synthetic fill price overflows");

    let avg_new = order
        .avg_fill_px
        .ok_or_else(|| inconsistent(cid, "missing average price for a fill without detail"))?;
    let cost_new = avg_new
        .as_decimal()
        .checked_mul(order.cum_filled_qty.as_decimal())
        .ok_or_else(overflow)?;

    let cum_old = tracked.cum_filled_qty.as_decimal();
    let cost_old = if cum_old.is_zero() {
        Decimal::ZERO
    } else {
        let avg_old = tracked
            .avg_fill_px
            .ok_or_else(|| inconsistent(cid, "tracked fill has no average price"))?;
        avg_old
            .as_decimal()
            .checked_mul(cum_old)
            .ok_or_else(overflow)?
    };

    let px = cost_new
        .checked_sub(cost_old)
        .and_then(|total| total.checked_div(delta))
        .ok_or_else(overflow)?
        .round_dp_with_strategy(SYNTHETIC_PX_DP, RoundingStrategy::MidpointNearestEven)
        .normalize();
    Price::from_decimal(px)
        .map_err(|_| inconsistent(cid, "synthetic fill price is not representable"))
}

/// Deterministic id for a fill inferred from a cumulative figure: the first 18 characters of
/// the venue order id with dashes removed, a dash, then the raw fixed-point cumulative quantity
/// in lower hex. At most 36 characters with the standard-precision build.
fn synthetic_trade_id(order: &BrokerOrder) -> Result<TradeId, EngineError> {
    let cid = order.client_order_id;
    let mut id = String::with_capacity(36);
    id.extend(
        order
            .venue_order_id
            .as_str()
            .chars()
            .filter(|c| *c != '-')
            .take(SYNTHETIC_ID_VENUE_CHARS),
    );
    id.push('-');
    write!(id, "{:x}", order.cum_filled_qty.raw())
        .map_err(|_| inconsistent(cid, "synthetic trade id could not be formatted"))?;
    TradeId::new_checked(&id).map_err(|_| inconsistent(cid, "synthetic trade id is too long"))
}

#[cfg(test)]
mod tests {
    use nisse_core::{
        DecisionId,
        nautilus::{OrderSide, OrderType, Symbol, TimeInForce},
    };
    use rust_decimal::Decimal;

    use super::*;

    const CID: &str = "nisse-1-0";
    const VENUE_ID: &str = "7b08df51-c1ac-453c-99f9-323a5f075f0d";

    fn ts(n: u64) -> UnixNanos {
        UnixNanos::from(n)
    }

    fn cid(id: &str) -> ClientOrderId {
        ClientOrderId::new(id)
    }

    fn price(s: &str) -> Price {
        s.parse::<Price>().unwrap()
    }

    fn intent(id: &str, qty: &str) -> OrderIntent {
        OrderIntent {
            decision_id: DecisionId(1),
            client_order_id: cid(id),
            symbol: Symbol::new("SPY"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            qty: Quantity::from(qty),
            limit_px: Some(price("500.10")),
            time_in_force: TimeInForce::Day,
            ts: ts(1_000),
        }
    }

    fn snapshot(id: &str, state: OrderState, cum: &str, avg: Option<&str>) -> BrokerOrder {
        BrokerOrder {
            client_order_id: cid(id),
            venue_order_id: VenueOrderId::new(VENUE_ID),
            symbol: Symbol::new("SPY"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            qty: Quantity::from("10"),
            limit_px: Some(price("500.10")),
            time_in_force: TimeInForce::Day,
            state,
            cum_filled_qty: Quantity::from(cum),
            avg_fill_px: avg.map(price),
            ts: ts(2_000),
        }
    }

    fn detail(trade: &str, px: &str, qty: &str) -> FillDetail {
        FillDetail {
            trade_id: TradeId::new(trade),
            px: price(px),
            qty: Quantity::from(qty),
        }
    }

    fn tracker_with(id: &str, qty: &str) -> OrderTracker {
        let mut tracker = OrderTracker::new();
        tracker.track_intent(intent(id, qty)).unwrap();
        tracker
    }

    /// Applies each `(order, detail)` step at `ts(3_000)` and returns the results in order.
    fn run(
        tracker: &mut OrderTracker,
        steps: &[(BrokerOrder, Option<FillDetail>)],
    ) -> Vec<Applied> {
        steps
            .iter()
            .map(|(order, fill)| tracker.apply(order, fill.as_ref(), ts(3_000)).unwrap())
            .collect()
    }

    /// Sum of the emitted fill quantities, checked against the tracked cumulative quantity.
    fn assert_fills_sum_to_cum(tracker: &OrderTracker, applied: &[Applied], id: &str) {
        let sum = applied
            .iter()
            .filter_map(|a| a.fill)
            .fold(Decimal::ZERO, |acc, f| acc + f.qty.as_decimal());
        let tracked = tracker.get(cid(id)).unwrap();
        assert_eq!(sum, tracked.cum_filled_qty.as_decimal());
    }

    #[test]
    fn track_intent_starts_in_intent_state_with_nothing_filled() {
        let tracker = tracker_with(CID, "10");
        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.intent, intent(CID, "10"));
        assert_eq!(tracked.venue_order_id, None);
        assert_eq!(tracked.state, OrderState::Intent);
        assert_eq!(tracked.cum_filled_qty.to_string(), "0");
        assert_eq!(tracked.cum_filled_qty.precision, 0);
        assert_eq!(tracked.avg_fill_px, None);
        assert_eq!(tracked.last_update_ts, ts(1_000));
    }

    #[test]
    fn track_intent_rejects_duplicate_and_keeps_original() {
        let mut tracker = tracker_with(CID, "10");
        let err = tracker.track_intent(intent(CID, "5")).unwrap_err();
        assert!(matches!(err, EngineError::DuplicateOrder(c) if c == cid(CID)));
        assert_eq!(tracker.get(cid(CID)).unwrap().intent.qty.to_string(), "10");
    }

    #[test]
    fn apply_unknown_order_errors() {
        let mut tracker = OrderTracker::new();
        let err = tracker
            .apply(&snapshot(CID, OrderState::Accepted, "0", None), None, ts(2))
            .unwrap_err();
        assert!(matches!(err, EngineError::UnknownOrder(c) if c == cid(CID)));
    }

    #[test]
    fn mark_never_arrived_unknown_order_errors() {
        let mut tracker = OrderTracker::new();
        let err = tracker.mark_never_arrived(cid(CID), ts(2)).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOrder(c) if c == cid(CID)));
    }

    #[test]
    fn accept_then_fill_with_matching_detail() {
        let mut tracker = tracker_with(CID, "10");
        let accepted = tracker
            .apply(&snapshot(CID, OrderState::Accepted, "0", None), None, ts(2))
            .unwrap();
        assert_eq!(
            accepted,
            Applied {
                state_change: Some((OrderState::Intent, OrderState::Accepted)),
                fill: None,
            }
        );

        let d = detail("exec-1", "500.10", "2");
        let filled = tracker
            .apply(
                &snapshot(CID, OrderState::Filled, "2", Some("500.10")),
                Some(&d),
                ts(3),
            )
            .unwrap();
        assert_eq!(
            filled.state_change,
            Some((OrderState::Accepted, OrderState::Filled))
        );
        assert_eq!(
            filled.fill,
            Some(Fill {
                client_order_id: cid(CID),
                trade_id: TradeId::new("exec-1"),
                px: price("500.10"),
                qty: Quantity::from("2"),
                ts: ts(3),
            })
        );

        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.state, OrderState::Filled);
        assert_eq!(tracked.cum_filled_qty.to_string(), "2");
        assert_eq!(tracked.avg_fill_px, Some(price("500.10")));
        assert_eq!(tracked.venue_order_id, Some(VenueOrderId::new(VENUE_ID)));
        assert_eq!(tracked.last_update_ts, ts(3));
    }

    #[test]
    fn two_partial_fills_then_final_fill_sum_to_order_quantity() {
        let mut tracker = tracker_with(CID, "10");
        let steps = [
            (snapshot(CID, OrderState::Accepted, "0", None), None),
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (
                snapshot(CID, OrderState::PartiallyFilled, "7", Some("10.2")),
                Some(detail("t2", "10.4", "4")),
            ),
            (
                snapshot(CID, OrderState::Filled, "10", Some("10.5")),
                Some(detail("t3", "10.5", "3")),
            ),
        ];
        let applied = run(&mut tracker, &steps);

        let qtys: Vec<String> = applied
            .iter()
            .filter_map(|a| a.fill.map(|f| f.qty.to_string()))
            .collect();
        assert_eq!(qtys, ["3", "4", "3"]);
        assert_eq!(
            applied[3].state_change,
            Some((OrderState::PartiallyFilled, OrderState::Filled))
        );
        assert_eq!(tracker.get(cid(CID)).unwrap().state, OrderState::Filled);
        assert_fills_sum_to_cum(&tracker, &applied, CID);
        assert_eq!(
            Decimal::from(10),
            tracker.get(cid(CID)).unwrap().cum_filled_qty.as_decimal()
        );
    }

    #[test]
    fn same_fill_message_applied_twice_emits_one_fill() {
        let mut tracker = tracker_with(CID, "10");
        let order = snapshot(CID, OrderState::PartiallyFilled, "3", Some("10"));
        let d = detail("t1", "10", "3");

        let first = tracker.apply(&order, Some(&d), ts(3)).unwrap();
        let second = tracker.apply(&order, Some(&d), ts(4)).unwrap();

        assert!(first.fill.is_some());
        assert_eq!(second, Applied::default());
        assert_eq!(
            tracker.get(cid(CID)).unwrap().cum_filled_qty.to_string(),
            "3"
        );
        // The second message is a same-state no-fill update, so it refreshes the timestamp.
        assert_eq!(tracker.get(cid(CID)).unwrap().last_update_ts, ts(4));
    }

    #[test]
    fn older_snapshot_changes_nothing() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "5", Some("10")),
                Some(&detail("t1", "10", "5")),
                ts(3),
            )
            .unwrap();
        let before = *tracker.get(cid(CID)).unwrap();

        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::Accepted, "3", Some("9")),
                None,
                ts(4),
            )
            .unwrap();

        assert_eq!(applied, Applied::default());
        assert_eq!(*tracker.get(cid(CID)).unwrap(), before);
    }

    #[test]
    fn missed_partial_fill_is_recovered_with_synthetic_price() {
        let mut tracker = tracker_with(CID, "10");
        let first = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(&detail("t1", "10", "3")),
                ts(3),
            )
            .unwrap();

        let recovered = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "5", Some("10.4")),
                None,
                ts(4),
            )
            .unwrap();

        let fill = recovered.fill.unwrap();
        assert_eq!(fill.qty.to_string(), "2");
        assert_eq!(fill.px.to_string(), "11");
        // First 18 venue id characters without dashes, then raw 5 * 10^9 in hex.
        assert_eq!(fill.trade_id.to_string(), "7b08df51c1ac453c99-12a05f200");
        assert!(fill.trade_id.to_string().len() <= 36);
        assert_eq!(fill.ts, ts(4));
        // The earlier detailed fill of 3 is part of the total as well.
        assert_fills_sum_to_cum(&tracker, &[first, recovered], CID);
    }

    #[test]
    fn synthetic_trade_id_is_identical_when_recomputed() {
        let steps = [
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (
                snapshot(CID, OrderState::PartiallyFilled, "5", Some("10.4")),
                None,
            ),
        ];
        let mut first = tracker_with(CID, "10");
        let mut second = tracker_with(CID, "10");
        let a = run(&mut first, &steps);
        let b = run(&mut second, &steps);
        assert_eq!(a[1].fill, b[1].fill);
        assert_eq!(a[1].fill.unwrap().trade_id, b[1].fill.unwrap().trade_id);
    }

    #[test]
    fn detail_with_mismatched_qty_uses_synthetic_price_and_id() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(&detail("t1", "10", "3")),
                ts(3),
            )
            .unwrap();

        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "5", Some("10.4")),
                Some(&detail("exec-x", "99", "1")),
                ts(4),
            )
            .unwrap();

        let fill = applied.fill.unwrap();
        assert_eq!(fill.qty.to_string(), "2");
        assert_eq!(fill.px.to_string(), "11");
        assert_eq!(fill.trade_id.to_string(), "7b08df51c1ac453c99-12a05f200");
    }

    #[test]
    fn missing_average_without_detail_is_inconsistent_and_changes_nothing() {
        let mut tracker = tracker_with(CID, "10");
        let before = *tracker.get(cid(CID)).unwrap();

        let err = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "2", None),
                None,
                ts(3),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::InconsistentBrokerData { client_order_id, .. } if client_order_id == cid(CID)
        ));
        assert_eq!(*tracker.get(cid(CID)).unwrap(), before);
    }

    #[test]
    fn late_accepted_after_canceled_is_ignored() {
        let mut tracker = tracker_with(CID, "10");
        let canceled = tracker
            .apply(&snapshot(CID, OrderState::Canceled, "0", None), None, ts(2))
            .unwrap();
        assert_eq!(
            canceled.state_change,
            Some((OrderState::Intent, OrderState::Canceled))
        );

        let late = tracker
            .apply(&snapshot(CID, OrderState::Accepted, "0", None), None, ts(3))
            .unwrap();

        assert_eq!(late, Applied::default());
        assert_eq!(tracker.get(cid(CID)).unwrap().state, OrderState::Canceled);
        assert_eq!(tracker.get(cid(CID)).unwrap().last_update_ts, ts(2));
    }

    #[test]
    fn fill_after_canceled_is_recorded_and_state_follows_broker() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(&snapshot(CID, OrderState::Canceled, "0", None), None, ts(2))
            .unwrap();

        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "2", Some("10")),
                Some(&detail("late-1", "10", "2")),
                ts(3),
            )
            .unwrap();

        assert_eq!(
            applied.state_change,
            Some((OrderState::Canceled, OrderState::PartiallyFilled))
        );
        assert_eq!(applied.fill.unwrap().qty.to_string(), "2");
        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.state, OrderState::PartiallyFilled);
        assert_eq!(tracked.cum_filled_qty.to_string(), "2");
    }

    #[test]
    fn pending_cancel_then_partially_filled_keeps_the_fill() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::PendingCancel, "0", None),
                None,
                ts(2),
            )
            .unwrap();

        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "4", Some("10")),
                Some(&detail("t1", "10", "4")),
                ts(3),
            )
            .unwrap();

        assert_eq!(
            applied.state_change,
            Some((OrderState::PendingCancel, OrderState::PartiallyFilled))
        );
        let fill = applied.fill.unwrap();
        assert_eq!(fill.qty.to_string(), "4");
        assert_eq!(fill.trade_id.to_string(), "t1");
        assert_eq!(
            tracker.get(cid(CID)).unwrap().cum_filled_qty.to_string(),
            "4"
        );
    }

    #[test]
    fn mark_never_arrived_sets_rejected() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::Submitted, "0", None),
                None,
                ts(2),
            )
            .unwrap();

        let applied = tracker.mark_never_arrived(cid(CID), ts(5)).unwrap();

        assert_eq!(
            applied,
            Applied {
                state_change: Some((OrderState::Submitted, OrderState::Rejected)),
                fill: None,
            }
        );
        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.state, OrderState::Rejected);
        assert_eq!(tracked.last_update_ts, ts(5));
        // The Submitted update above acknowledged the venue id, so it is kept.
        assert_eq!(tracked.venue_order_id, Some(VenueOrderId::new(VENUE_ID)));
    }

    #[test]
    fn mark_never_arrived_is_noop_when_terminal() {
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(&snapshot(CID, OrderState::Canceled, "0", None), None, ts(2))
            .unwrap();

        let applied = tracker.mark_never_arrived(cid(CID), ts(5)).unwrap();

        assert_eq!(applied, Applied::default());
        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.state, OrderState::Canceled);
        assert_eq!(tracked.last_update_ts, ts(2));
    }

    #[test]
    fn open_orders_excludes_terminal_and_iterates_in_id_order() {
        let mut tracker = OrderTracker::new();
        for id in ["nisse-3-0", "nisse-1-0", "nisse-2-0"] {
            tracker.track_intent(intent(id, "10")).unwrap();
        }
        tracker.mark_never_arrived(cid("nisse-2-0"), ts(5)).unwrap();

        let ids: Vec<&str> = tracker
            .open_orders()
            .map(|o| o.intent.client_order_id.as_str())
            .collect();
        assert_eq!(ids, ["nisse-1-0", "nisse-3-0"]);
    }

    #[test]
    fn restart_with_fresh_tracker_emits_one_fill_from_snapshot() {
        let mut tracker = tracker_with(CID, "10");

        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "2", Some("10")),
                None,
                ts(3),
            )
            .unwrap();

        let fill = applied.fill.unwrap();
        assert_eq!(fill.qty.to_string(), "2");
        assert_eq!(fill.px.to_string(), "10");
        // Raw 2 * 10^9 in hex.
        assert_eq!(fill.trade_id.to_string(), "7b08df51c1ac453c99-77359400");
        assert_eq!(
            applied.state_change,
            Some((OrderState::Intent, OrderState::PartiallyFilled))
        );
    }

    #[test]
    fn fill_message_before_accepted_yields_one_fill_and_stale_accepted_changes_nothing() {
        let mut tracker = tracker_with(CID, "10");
        let d = detail("exec-1", "500.10", "2");

        let fill_msg = tracker
            .apply(
                &snapshot(CID, OrderState::Filled, "2", Some("500.10")),
                Some(&d),
                ts(3),
            )
            .unwrap();
        let late_accepted = tracker
            .apply(&snapshot(CID, OrderState::Accepted, "0", None), None, ts(4))
            .unwrap();

        assert_eq!(fill_msg.fill.unwrap().qty.to_string(), "2");
        assert_eq!(
            fill_msg.state_change,
            Some((OrderState::Intent, OrderState::Filled))
        );
        assert_eq!(late_accepted, Applied::default());
        let tracked = tracker.get(cid(CID)).unwrap();
        assert_eq!(tracked.state, OrderState::Filled);
        assert_eq!(tracked.cum_filled_qty.to_string(), "2");
    }

    #[test]
    fn fill_quantities_sum_to_cumulative_over_sequences() {
        // In order, with details.
        let mut tracker = tracker_with(CID, "10");
        let steps = [
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (
                snapshot(CID, OrderState::Filled, "10", Some("10.5")),
                Some(detail("t3", "10.5", "7")),
            ),
        ];
        let applied = run(&mut tracker, &steps);
        assert_fills_sum_to_cum(&tracker, &applied, CID);

        // Duplicate, stale and missed messages mixed in.
        let mut tracker = tracker_with(CID, "10");
        let steps = [
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (snapshot(CID, OrderState::Accepted, "0", None), None),
            (snapshot(CID, OrderState::Filled, "10", Some("10.3")), None),
        ];
        let applied = run(&mut tracker, &steps);
        assert_fills_sum_to_cum(&tracker, &applied, CID);
        assert_eq!(
            tracker.get(cid(CID)).unwrap().cum_filled_qty.to_string(),
            "10"
        );

        // Synthetic recovery between two detailed fills.
        let mut tracker = tracker_with(CID, "10");
        let steps = [
            (
                snapshot(CID, OrderState::PartiallyFilled, "3", Some("10")),
                Some(detail("t1", "10", "3")),
            ),
            (
                snapshot(CID, OrderState::PartiallyFilled, "5", Some("10.4")),
                None,
            ),
            (
                snapshot(CID, OrderState::Filled, "10", Some("10.5")),
                Some(detail("t9", "11", "5")),
            ),
        ];
        let applied = run(&mut tracker, &steps);
        assert_fills_sum_to_cum(&tracker, &applied, CID);
        assert_eq!(
            tracker.get(cid(CID)).unwrap().cum_filled_qty.to_string(),
            "10"
        );
    }

    #[test]
    fn equal_quantities_with_different_precision_compare_equal() {
        let mut tracker = tracker_with(CID, "10");
        let applied = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "2.0", Some("500.10")),
                Some(&detail("exec-2", "500.10", "2")),
                ts(3),
            )
            .unwrap();

        // The detail matched, so the broker's execution id is kept and no synthetic id is made.
        let fill = applied.fill.unwrap();
        assert_eq!(fill.trade_id.to_string(), "exec-2");
        assert_eq!(fill.qty.as_decimal(), Decimal::from(2));
    }

    #[test]
    fn synthetic_price_rounds_half_even_at_nine_places() {
        // (2e-9·3 − 1e-9·1) / 2 = 2.5e-9 -> 2e-9 (half to even)
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "1", Some("0.000000001")),
                Some(&detail("t1", "0.000000001", "1")),
                ts(3),
            )
            .unwrap();
        let even = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "3", Some("0.000000002")),
                None,
                ts(4),
            )
            .unwrap();
        assert_eq!(even.fill.unwrap().px.to_string(), "0.000000002");

        // (3e-9·3 − 2e-9·1) / 2 = 3.5e-9 -> 4e-9 (half to even)
        let mut tracker = tracker_with(CID, "10");
        tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "1", Some("0.000000002")),
                Some(&detail("t1", "0.000000002", "1")),
                ts(3),
            )
            .unwrap();
        let odd = tracker
            .apply(
                &snapshot(CID, OrderState::PartiallyFilled, "3", Some("0.000000003")),
                None,
                ts(4),
            )
            .unwrap();
        assert_eq!(odd.fill.unwrap().px.to_string(), "0.000000004");
    }
}
