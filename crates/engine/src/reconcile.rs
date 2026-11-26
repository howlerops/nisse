//! Comparing the engine's view with the broker's. Pure: no I/O, no clock.

use std::collections::{BTreeMap, BTreeSet};

use nisse_core::{
    BrokerOrder, BrokerPosition,
    nautilus::{ClientOrderId, PositionSide, Symbol},
};
use rust_decimal::Decimal;

use crate::OrderTracker;

/// One difference between local and broker state. The broker wins every time; a finding says
/// what has to be done locally to agree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// The broker has this order open and its state or cumulative fill differs from the
    /// tracker's: apply the snapshot.
    OrderDrift { order: BrokerOrder },
    /// The tracker thinks this order is open (or still only an intent, or only provisionally
    /// never-arrived) but it is not among
    /// the broker's open orders: look it up by client order id. It was filled, cancelled or
    /// expired while we were not listening, or it never arrived.
    NotOpenAtBroker { client_order_id: ClientOrderId },
    /// The broker has an open order the engine never intended. Someone or something else is
    /// trading this account: alert, and do not touch it automatically.
    UnknownAtBroker { order: BrokerOrder },
    /// Signed quantities differ (long positive, short negative, absent zero): adopt the
    /// broker's and record a `position_event`.
    PositionMismatch {
        symbol: Symbol,
        local: Decimal,
        broker: Decimal,
    },
}

/// Lists every difference, in a deterministic order: order findings sorted by client order
/// id, then position findings sorted by symbol. An empty result means the views agree.
///
/// `local_positions` holds signed quantities; a missing symbol means flat.
#[must_use]
pub fn reconcile(
    tracker: &OrderTracker,
    local_positions: &BTreeMap<Symbol, Decimal>,
    broker_open_orders: &[BrokerOrder],
    broker_positions: &[BrokerPosition],
) -> Vec<Finding> {
    let mut findings = Vec::with_capacity(
        broker_open_orders.len() + broker_positions.len() + local_positions.len(),
    );

    push_order_findings(tracker, broker_open_orders, &mut findings);
    let order_count = findings.len();
    findings[..order_count].sort_unstable_by(|a, b| sort_key(a).cmp(sort_key(b)));

    push_position_findings(local_positions, broker_positions, &mut findings);
    findings[order_count..].sort_unstable_by(|a, b| sort_key(a).cmp(sort_key(b)));

    findings
}

/// Order findings, in no particular order: the caller sorts them.
fn push_order_findings(
    tracker: &OrderTracker,
    broker_open_orders: &[BrokerOrder],
    out: &mut Vec<Finding>,
) {
    let broker_ids: BTreeSet<ClientOrderId> = broker_open_orders
        .iter()
        .map(|order| order.client_order_id)
        .collect();

    for order in broker_open_orders {
        match tracker.get(order.client_order_id) {
            None => out.push(Finding::UnknownAtBroker { order: *order }),
            Some(tracked)
                if tracked.state != order.state
                    || tracked.cum_filled_qty.as_decimal() != order.cum_filled_qty.as_decimal() =>
            {
                out.push(Finding::OrderDrift { order: *order });
            }
            Some(_) => {}
        }
    }

    // Includes orders presumed never-arrived: that verdict is provisional and they must keep
    // being looked up, or a fill on one of them would never be found.
    for tracked in tracker.unresolved_orders() {
        let client_order_id = tracked.intent.client_order_id;
        if !broker_ids.contains(&client_order_id) {
            out.push(Finding::NotOpenAtBroker { client_order_id });
        }
    }
}

/// Position findings, in no particular order: the caller sorts them. Broker lines for the same
/// symbol are summed first; a missing symbol on either side counts as zero.
fn push_position_findings(
    local_positions: &BTreeMap<Symbol, Decimal>,
    broker_positions: &[BrokerPosition],
    out: &mut Vec<Finding>,
) {
    let mut broker: BTreeMap<Symbol, Decimal> = BTreeMap::new();
    for position in broker_positions {
        let signed = signed_qty(position);
        let total = broker.entry(position.symbol).or_insert(Decimal::ZERO);
        // Overflow cannot happen with real quantities. If it did, the sum saturates to the
        // sign's extreme so the symbol still shows up as a mismatch.
        *total = total
            .checked_add(signed)
            .unwrap_or(if signed.is_sign_negative() {
                Decimal::MIN
            } else {
                Decimal::MAX
            });
    }

    let start = out.len();
    for (symbol, local_qty) in local_positions {
        let broker_qty = broker.get(symbol).copied().unwrap_or(Decimal::ZERO);
        if *local_qty != broker_qty {
            out.push(Finding::PositionMismatch {
                symbol: *symbol,
                local: *local_qty,
                broker: broker_qty,
            });
        }
    }
    for (symbol, broker_qty) in &broker {
        if !broker_qty.is_zero() && !local_positions.contains_key(symbol) {
            out.push(Finding::PositionMismatch {
                symbol: *symbol,
                local: Decimal::ZERO,
                broker: *broker_qty,
            });
        }
    }
    out[start..].sort_unstable_by(|a, b| sort_key(a).cmp(sort_key(b)));
}

/// Signed broker quantity: long positive, short negative, flat zero.
fn signed_qty(position: &BrokerPosition) -> Decimal {
    let qty = position.qty.as_decimal();
    match position.side {
        PositionSide::Long => qty,
        PositionSide::Short => -qty,
        PositionSide::Flat => Decimal::ZERO,
    }
}

/// The string the finding is ordered by: the client order id, or the symbol for a position.
/// Sorting by the string keeps the order independent of how the id types compare.
fn sort_key(finding: &Finding) -> &str {
    match finding {
        Finding::OrderDrift { order } | Finding::UnknownAtBroker { order } => {
            order.client_order_id.as_str()
        }
        Finding::NotOpenAtBroker { client_order_id } => client_order_id.as_str(),
        Finding::PositionMismatch { symbol, .. } => symbol.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use nisse_core::{
        DecisionId, OrderIntent, OrderState,
        nautilus::{OrderSide, OrderType, Price, Quantity, TimeInForce, UnixNanos, VenueOrderId},
    };

    use super::*;

    const VENUE_ID: &str = "7b08df51-c1ac-453c-99f9-323a5f075f0d";

    fn ts(n: u64) -> UnixNanos {
        UnixNanos::from(n)
    }

    fn cid(id: &str) -> ClientOrderId {
        ClientOrderId::new(id)
    }

    fn dec(s: &str) -> Decimal {
        s.parse::<Decimal>().unwrap()
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

    fn position(symbol: &str, side: PositionSide, qty: &str) -> BrokerPosition {
        BrokerPosition {
            symbol: Symbol::new(symbol),
            side,
            qty: Quantity::from(qty),
            avg_entry_px: price("100"),
        }
    }

    fn local(entries: &[(&str, &str)]) -> BTreeMap<Symbol, Decimal> {
        entries
            .iter()
            .map(|(symbol, qty)| (Symbol::new(symbol), dec(qty)))
            .collect()
    }

    /// Tracks an intent for `id` and applies one broker snapshot to it. A zero cumulative
    /// quantity carries no average price.
    fn track(tracker: &mut OrderTracker, id: &str, state: OrderState, cum: &str) {
        tracker.track_intent(intent(id, "10")).unwrap();
        let avg = (cum != "0").then_some("10");
        tracker
            .apply(&snapshot(id, state, cum, avg), None, ts(2))
            .unwrap();
    }

    #[test]
    fn agreement_is_empty() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::Accepted, "0");
        track(&mut tracker, "nisse-2-0", OrderState::PartiallyFilled, "2");

        let findings = reconcile(
            &tracker,
            &local(&[("SPY", "5")]),
            &[
                snapshot("nisse-1-0", OrderState::Accepted, "0", None),
                snapshot("nisse-2-0", OrderState::PartiallyFilled, "2", Some("10")),
            ],
            &[position("SPY", PositionSide::Long, "5")],
        );

        assert_eq!(findings, Vec::<Finding>::new());
    }

    #[test]
    fn drift_by_state() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::Accepted, "0");

        let findings = reconcile(
            &tracker,
            &BTreeMap::new(),
            &[snapshot("nisse-1-0", OrderState::Canceled, "0", None)],
            &[],
        );

        assert_eq!(
            findings,
            vec![Finding::OrderDrift {
                order: snapshot("nisse-1-0", OrderState::Canceled, "0", None),
            }]
        );
    }

    #[test]
    fn drift_by_cum() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::PartiallyFilled, "2");

        let findings = reconcile(
            &tracker,
            &BTreeMap::new(),
            &[snapshot(
                "nisse-1-0",
                OrderState::PartiallyFilled,
                "3",
                Some("10"),
            )],
            &[],
        );

        assert_eq!(
            findings,
            vec![Finding::OrderDrift {
                order: snapshot("nisse-1-0", OrderState::PartiallyFilled, "3", Some("10")),
            }]
        );
    }

    #[test]
    fn precision_difference_alone_is_not_drift() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::PartiallyFilled, "2");

        let findings = reconcile(
            &tracker,
            &BTreeMap::new(),
            &[snapshot(
                "nisse-1-0",
                OrderState::PartiallyFilled,
                "2.0",
                Some("10"),
            )],
            &[],
        );

        assert_eq!(findings, Vec::<Finding>::new());
    }

    #[test]
    fn broker_order_not_tracked_is_unknown() {
        let tracker = OrderTracker::new();
        let stranger = snapshot("manual-7", OrderState::Accepted, "0", None);

        let findings = reconcile(&tracker, &BTreeMap::new(), &[stranger], &[]);

        assert_eq!(findings, vec![Finding::UnknownAtBroker { order: stranger }]);
    }

    #[test]
    fn tracked_intent_not_at_broker_is_not_open() {
        let mut tracker = OrderTracker::new();
        tracker.track_intent(intent("nisse-1-0", "10")).unwrap();

        let findings = reconcile(&tracker, &BTreeMap::new(), &[], &[]);

        assert_eq!(
            findings,
            vec![Finding::NotOpenAtBroker {
                client_order_id: cid("nisse-1-0"),
            }]
        );
    }

    #[test]
    fn terminal_tracked_orders_are_not_reported() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::Filled, "10");
        track(&mut tracker, "nisse-2-0", OrderState::Canceled, "0");
        track(&mut tracker, "nisse-3-0", OrderState::Rejected, "0");

        let findings = reconcile(&tracker, &BTreeMap::new(), &[], &[]);

        assert_eq!(findings, Vec::<Finding>::new());
    }

    #[test]
    fn long_position_mismatch() {
        let findings = reconcile(
            &OrderTracker::new(),
            &local(&[("SPY", "5")]),
            &[],
            &[position("SPY", PositionSide::Long, "7")],
        );

        assert_eq!(
            findings,
            vec![Finding::PositionMismatch {
                symbol: Symbol::new("SPY"),
                local: dec("5"),
                broker: dec("7"),
            }]
        );
    }

    #[test]
    fn short_position_is_negative_and_mismatch_is_reported() {
        let findings = reconcile(
            &OrderTracker::new(),
            &local(&[("QQQ", "-2")]),
            &[],
            &[position("QQQ", PositionSide::Short, "5")],
        );

        assert_eq!(
            findings,
            vec![Finding::PositionMismatch {
                symbol: Symbol::new("QQQ"),
                local: dec("-2"),
                broker: dec("-5"),
            }]
        );
    }

    #[test]
    fn short_position_equal_is_not_a_finding() {
        let findings = reconcile(
            &OrderTracker::new(),
            &local(&[("QQQ", "-5")]),
            &[],
            &[position("QQQ", PositionSide::Short, "5.0")],
        );

        assert_eq!(findings, Vec::<Finding>::new());
    }

    #[test]
    fn broker_only_symbol_is_a_mismatch_from_zero() {
        let findings = reconcile(
            &OrderTracker::new(),
            &BTreeMap::new(),
            &[],
            &[position("IWM", PositionSide::Long, "4")],
        );

        assert_eq!(
            findings,
            vec![Finding::PositionMismatch {
                symbol: Symbol::new("IWM"),
                local: dec("0"),
                broker: dec("4"),
            }]
        );
    }

    #[test]
    fn local_only_symbol_is_a_mismatch_to_zero() {
        let findings = reconcile(&OrderTracker::new(), &local(&[("DIA", "6")]), &[], &[]);

        assert_eq!(
            findings,
            vec![Finding::PositionMismatch {
                symbol: Symbol::new("DIA"),
                local: dec("6"),
                broker: dec("0"),
            }]
        );
    }

    #[test]
    fn flat_equals_absent_and_zero_local_equals_absent() {
        let findings = reconcile(
            &OrderTracker::new(),
            &local(&[("TLT", "0")]),
            &[],
            &[position("SPY", PositionSide::Flat, "0")],
        );

        assert_eq!(findings, Vec::<Finding>::new());
    }

    #[test]
    fn broker_lines_for_one_symbol_are_summed() {
        let findings = reconcile(
            &OrderTracker::new(),
            &local(&[("SPY", "5")]),
            &[],
            &[
                position("SPY", PositionSide::Long, "2"),
                position("SPY", PositionSide::Long, "3"),
            ],
        );
        assert_eq!(findings, Vec::<Finding>::new());

        let netted = reconcile(
            &OrderTracker::new(),
            &BTreeMap::new(),
            &[],
            &[
                position("SPY", PositionSide::Long, "5"),
                position("SPY", PositionSide::Short, "5"),
            ],
        );
        assert_eq!(netted, Vec::<Finding>::new());
    }

    #[test]
    fn mixed_result_is_sorted_by_id_then_symbol() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::Accepted, "0");
        track(&mut tracker, "nisse-2-0", OrderState::Accepted, "0");
        track(&mut tracker, "nisse-4-0", OrderState::Filled, "10");
        tracker.track_intent(intent("nisse-5-0", "10")).unwrap();

        let findings = reconcile(
            &tracker,
            &local(&[("SPY", "5"), ("TLT", "1")]),
            &[
                snapshot("nisse-3-0", OrderState::Accepted, "0", None),
                snapshot("nisse-1-0", OrderState::PartiallyFilled, "3", Some("10")),
            ],
            &[
                position("IWM", PositionSide::Long, "4"),
                position("TLT", PositionSide::Short, "1"),
            ],
        );

        assert_eq!(
            findings,
            vec![
                Finding::OrderDrift {
                    order: snapshot("nisse-1-0", OrderState::PartiallyFilled, "3", Some("10")),
                },
                Finding::NotOpenAtBroker {
                    client_order_id: cid("nisse-2-0"),
                },
                Finding::UnknownAtBroker {
                    order: snapshot("nisse-3-0", OrderState::Accepted, "0", None),
                },
                Finding::NotOpenAtBroker {
                    client_order_id: cid("nisse-5-0"),
                },
                Finding::PositionMismatch {
                    symbol: Symbol::new("IWM"),
                    local: dec("0"),
                    broker: dec("4"),
                },
                Finding::PositionMismatch {
                    symbol: Symbol::new("SPY"),
                    local: dec("5"),
                    broker: dec("0"),
                },
                Finding::PositionMismatch {
                    symbol: Symbol::new("TLT"),
                    local: dec("1"),
                    broker: dec("-1"),
                },
            ]
        );
    }

    #[test]
    fn output_order_is_deterministic_under_scrambled_input() {
        let mut tracker = OrderTracker::new();
        track(&mut tracker, "nisse-1-0", OrderState::Accepted, "0");
        track(&mut tracker, "nisse-2-0", OrderState::Accepted, "0");

        let local_positions = local(&[("TLT", "1"), ("SPY", "2"), ("QQQ", "3")]);
        let orders_a = [
            snapshot("nisse-9-0", OrderState::Accepted, "0", None),
            snapshot("nisse-0-0", OrderState::Accepted, "0", None),
        ];
        let orders_b = [orders_a[1], orders_a[0]];
        let positions_a = [
            position("SPY", PositionSide::Long, "9"),
            position("IWM", PositionSide::Long, "1"),
            position("QQQ", PositionSide::Short, "1"),
        ];
        let positions_b = [positions_a[2], positions_a[1], positions_a[0]];

        let expected = vec![
            Finding::UnknownAtBroker {
                order: snapshot("nisse-0-0", OrderState::Accepted, "0", None),
            },
            Finding::NotOpenAtBroker {
                client_order_id: cid("nisse-1-0"),
            },
            Finding::NotOpenAtBroker {
                client_order_id: cid("nisse-2-0"),
            },
            Finding::UnknownAtBroker {
                order: snapshot("nisse-9-0", OrderState::Accepted, "0", None),
            },
            Finding::PositionMismatch {
                symbol: Symbol::new("IWM"),
                local: dec("0"),
                broker: dec("1"),
            },
            Finding::PositionMismatch {
                symbol: Symbol::new("QQQ"),
                local: dec("3"),
                broker: dec("-1"),
            },
            Finding::PositionMismatch {
                symbol: Symbol::new("SPY"),
                local: dec("2"),
                broker: dec("9"),
            },
            Finding::PositionMismatch {
                symbol: Symbol::new("TLT"),
                local: dec("1"),
                broker: dec("0"),
            },
        ];

        assert_eq!(
            reconcile(&tracker, &local_positions, &orders_a, &positions_a),
            expected
        );
        assert_eq!(
            reconcile(&tracker, &local_positions, &orders_b, &positions_b),
            expected
        );
    }
}
