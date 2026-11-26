//! Acceptance tests for broker sync (phase 1, T10).

#![allow(
    clippy::expect_used,
    reason = "builders parse literals; a malformed literal is a bug in the test"
)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use nisse_core::{
    BrokerPosition, OrderState,
    nautilus::{ClientOrderId, PositionSide, Price, Quantity, Symbol},
};
use nisse_engine::{
    EngineError, Finding, OrderTracker, SyncReport, TrackedOrder, sync_with_broker,
};
use rust_decimal::Decimal;
use support::{Call, FakeBroker, accepted_from, calls, intent, new_call_log, snapshot};

fn cid(id: &str) -> ClientOrderId {
    ClientOrderId::new(id)
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().expect("valid test decimal")
}

fn symbol(s: &str) -> Symbol {
    Symbol::new(s)
}

fn now() -> nisse_core::nautilus::UnixNanos {
    nisse_core::nautilus::UnixNanos::from(9_000_u64)
}

fn position(symbol_name: &str, side: PositionSide, qty: &str) -> BrokerPosition {
    BrokerPosition {
        symbol: symbol(symbol_name),
        side,
        qty: Quantity::from(qty),
        avg_entry_px: "100".parse::<Price>().expect("valid test price"),
    }
}

fn local(entries: &[(&str, &str)]) -> BTreeMap<Symbol, Decimal> {
    entries
        .iter()
        .map(|(name, qty)| (symbol(name), dec(qty)))
        .collect()
}

/// A fresh fixture: a broker and the call log it writes to.
struct Fixture {
    log: support::CallLog,
    broker: FakeBroker,
}

fn fixture() -> Fixture {
    let log = new_call_log();
    let broker = FakeBroker::new(Arc::clone(&log));
    Fixture { log, broker }
}

fn tracked(tracker: &OrderTracker, id: &str) -> Option<TrackedOrder> {
    tracker.get(cid(id)).copied()
}

#[tokio::test]
async fn clean_state_reports_nothing_and_looks_up_no_orders() {
    let f = fixture();
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    tracker.apply(&accepted_from(&i), None, now()).unwrap();
    f.broker.set_open_orders(vec![accepted_from(&i)]);
    f.broker
        .set_positions(vec![position("SPY", PositionSide::Long, "5")]);
    let mut positions = local(&[("SPY", "5")]);

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(report, SyncReport::default());
    assert_eq!(calls(&f.log), vec![Call::OpenOrders, Call::Positions]);
    assert_eq!(positions, local(&[("SPY", "5")]));

    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert_eq!(again, SyncReport::default());
}

#[tokio::test]
async fn restart_mid_order_applies_drift_and_reports_the_fill() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    let partial = snapshot(&i, OrderState::PartiallyFilled, "2", Some("10"));
    f.broker.set_open_orders(vec![partial]);
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::OrderDrift { order: partial }]
    );
    assert_eq!(report.fills.len(), 1);
    let fill = report.fills[0];
    assert_eq!(fill.client_order_id, cid("nisse-1-0"));
    assert_eq!(fill.qty.as_decimal(), dec("2"));
    assert_eq!(fill.px.as_decimal(), dec("10"));
    assert_eq!(
        tracked(&tracker, "nisse-1-0").unwrap().state,
        OrderState::PartiallyFilled
    );
    assert!(positions.is_empty());

    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert!(again.fills.is_empty());
    assert!(again.findings.is_empty());
}

#[tokio::test]
async fn filled_while_disconnected_is_found_by_id_and_reported() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    tracker.apply(&accepted_from(&i), None, now()).unwrap();
    let filled = snapshot(&i, OrderState::Filled, "5", Some("10"));
    f.broker.insert_order(filled);
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::NotOpenAtBroker {
            client_order_id: cid("nisse-1-0"),
        }]
    );
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].qty.as_decimal(), dec("5"));
    assert_eq!(report.fills[0].px.as_decimal(), dec("10"));
    assert_eq!(
        tracked(&tracker, "nisse-1-0").unwrap().state,
        OrderState::Filled
    );
    assert_eq!(
        calls(&f.log),
        vec![
            Call::OpenOrders,
            Call::Positions,
            Call::OrderByClientId(cid("nisse-1-0")),
        ]
    );

    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert!(again.fills.is_empty());
    assert!(again.findings.is_empty());
}

#[tokio::test]
async fn stale_open_list_applies_non_terminal_lookup_as_a_snapshot() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    tracker.apply(&accepted_from(&i), None, now()).unwrap();
    // The open-orders list does not show the order yet, but a lookup finds it part filled.
    let partial = snapshot(&i, OrderState::PartiallyFilled, "2", Some("10"));
    f.broker.insert_order(partial);
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::NotOpenAtBroker {
            client_order_id: cid("nisse-1-0"),
        }]
    );
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].qty.as_decimal(), dec("2"));
    assert_eq!(
        tracked(&tracker, "nisse-1-0").unwrap().state,
        OrderState::PartiallyFilled
    );
}

#[tokio::test]
async fn stale_open_list_with_unchanged_lookup_changes_nothing() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    tracker.apply(&accepted_from(&i), None, now()).unwrap();
    f.broker.insert_order(accepted_from(&i));
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::NotOpenAtBroker {
            client_order_id: cid("nisse-1-0"),
        }]
    );
    assert!(report.fills.is_empty());
    let order = tracked(&tracker, "nisse-1-0").unwrap();
    assert_eq!(order.state, OrderState::Accepted);
    assert_eq!(order.cum_filled_qty.as_decimal(), dec("0"));
}

#[tokio::test]
async fn submit_that_never_arrived_is_rejected_at_now() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::NotOpenAtBroker {
            client_order_id: cid("nisse-1-0"),
        }]
    );
    assert!(report.fills.is_empty());
    let order = tracked(&tracker, "nisse-1-0").unwrap();
    assert_eq!(order.state, OrderState::Rejected);
    assert_eq!(order.last_update_ts, now());

    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert!(again.fills.is_empty());
    assert!(again.findings.is_empty());
}

#[tokio::test]
async fn unknown_broker_order_is_reported_and_untouched() {
    let f = fixture();
    let stranger_intent = intent("manual-7", "3", None);
    let stranger = accepted_from(&stranger_intent);
    f.broker.set_open_orders(vec![stranger]);
    let mut tracker = OrderTracker::new();
    let mut positions = BTreeMap::new();

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report,
        SyncReport {
            findings: vec![Finding::UnknownAtBroker { order: stranger }],
            fills: Vec::new(),
        }
    );
    assert!(tracker.get(cid("manual-7")).is_none());
    assert!(tracker.open_orders().next().is_none());
    assert!(positions.is_empty());
    assert_eq!(calls(&f.log), vec![Call::OpenOrders, Call::Positions]);

    // Still reported on every pass, because nothing is changed automatically.
    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert_eq!(
        again.findings,
        vec![Finding::UnknownAtBroker { order: stranger }]
    );
    assert!(again.fills.is_empty());
    assert!(tracker.get(cid("manual-7")).is_none());
}

#[tokio::test]
async fn positions_are_overwritten_and_flat_symbols_are_removed() {
    let f = fixture();
    f.broker.set_positions(vec![
        position("SPY", PositionSide::Long, "7"),
        position("TLT", PositionSide::Short, "2"),
    ]);
    let mut tracker = OrderTracker::new();
    let mut positions = local(&[("SPY", "5"), ("QQQ", "3")]);

    let report = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![
            Finding::PositionMismatch {
                symbol: symbol("QQQ"),
                local: dec("3"),
                broker: dec("0"),
            },
            Finding::PositionMismatch {
                symbol: symbol("SPY"),
                local: dec("5"),
                broker: dec("7"),
            },
            Finding::PositionMismatch {
                symbol: symbol("TLT"),
                local: dec("0"),
                broker: dec("-2"),
            },
        ]
    );
    assert!(report.fills.is_empty());
    assert_eq!(positions, local(&[("SPY", "7"), ("TLT", "-2")]));

    let again = sync_with_broker(&f.broker, &mut tracker, &mut positions, now())
        .await
        .unwrap();
    assert!(again.fills.is_empty());
    assert!(again.findings.is_empty());
}

#[tokio::test]
async fn broker_error_aborts_and_leaves_state_unchanged() {
    let f = fixture();
    let i = intent("nisse-1-0", "5", Some("10"));
    let mut tracker = OrderTracker::new();
    tracker.track_intent(i).unwrap();
    tracker.apply(&accepted_from(&i), None, now()).unwrap();
    let before = tracked(&tracker, "nisse-1-0");
    let mut positions = local(&[("SPY", "5")]);
    f.broker.fail_reads("down");

    let result = sync_with_broker(&f.broker, &mut tracker, &mut positions, now()).await;

    match result {
        Err(EngineError::Broker(_)) => {}
        other => panic!("expected a broker error, got {other:?}"),
    }
    assert_eq!(tracked(&tracker, "nisse-1-0"), before);
    assert_eq!(positions, local(&[("SPY", "5")]));
}
