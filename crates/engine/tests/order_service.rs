//! Acceptance tests for the order service (phase 2, E3): broker activity turned into store
//! records.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test; builders parse literals"
)]

mod support;

use nisse_core::{
    BrokerPosition, Fill, FillDetail, ManualClock, OrderEvent, OrderIntent, OrderState,
    OrderUpdate, PositionEvent, StoreRecord,
    nautilus::{
        ClientOrderId, OrderSide, PositionSide, Price, Quantity, Symbol, TradeId, UnixNanos,
        VenueOrderId,
    },
};
use nisse_engine::{
    EngineError, Finding, OrderBookState, SyncOptions, SyncReport, reconcile_and_record,
    record_stream_update,
};
use rust_decimal::Decimal;
use support::{FakeBroker, MemorySink, accepted_from, intent, new_call_log, snapshot};

fn cid(id: &str) -> ClientOrderId {
    ClientOrderId::new(id)
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().expect("valid test decimal")
}

fn price(s: &str) -> Price {
    s.parse::<Price>().expect("valid test price")
}

fn spy() -> Symbol {
    Symbol::new("SPY")
}

fn at(n: u64) -> UnixNanos {
    UnixNanos::from(n)
}

/// The injected clock reading used by every reconciliation test.
fn now() -> UnixNanos {
    at(9_000)
}

fn clock() -> ManualClock {
    ManualClock::new(now())
}

fn broker() -> FakeBroker {
    FakeBroker::new(new_call_log())
}

/// Sync options with the given grace and a recheck window far longer than any test.
fn opts(never_arrived_grace_nanos: u64) -> SyncOptions {
    SyncOptions {
        never_arrived_grace_nanos,
        never_arrived_recheck_nanos: 1_000_000_000_000,
    }
}

fn detail(trade: &str, px: &str, qty: &str) -> FillDetail {
    FillDetail {
        trade_id: TradeId::new(trade),
        px: price(px),
        qty: Quantity::from(qty),
    }
}

fn update(order: nisse_core::BrokerOrder, fill: Option<FillDetail>, ts: u64) -> OrderUpdate {
    OrderUpdate {
        order,
        fill,
        ts: at(ts),
    }
}

/// The fill the tracker derives from `detail`, stamped `ts`.
fn fill_of(i: &OrderIntent, detail: FillDetail, ts: u64) -> Fill {
    Fill {
        client_order_id: i.client_order_id,
        trade_id: detail.trade_id,
        px: detail.px,
        qty: detail.qty,
        ts: at(ts),
    }
}

fn order_rec(
    i: &OrderIntent,
    state: OrderState,
    venue: Option<VenueOrderId>,
    cum: &str,
    ts: u64,
    fill: Option<Fill>,
) -> StoreRecord {
    StoreRecord::Order {
        event: OrderEvent::of(i, state, venue, Quantity::from(cum), at(ts)),
        fill,
    }
}

fn position_rec(qty_after: &str, avg_cost: &str, ts: u64) -> StoreRecord {
    StoreRecord::Position(PositionEvent {
        symbol: spy(),
        qty_after: dec(qty_after),
        avg_cost: dec(avg_cost),
        ts: at(ts),
    })
}

fn broker_long(qty: &str, avg: &str) -> BrokerPosition {
    BrokerPosition {
        symbol: spy(),
        side: PositionSide::Long,
        qty: Quantity::from(qty),
        avg_entry_px: price(avg),
    }
}

#[tokio::test]
async fn stream_fill_emits_order_row_then_position_and_updates_the_book() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, at(1_500))
        .unwrap();
    let sink = MemorySink::new();

    let partial = snapshot(&i, OrderState::PartiallyFilled, "4", Some("500.05"));
    let d = detail("t-1", "500.05", "4");
    let msg = update(partial, Some(d), 2_000);
    assert!(record_stream_update(&mut state, &sink, &msg).await.unwrap());

    assert_eq!(
        sink.records(),
        vec![
            order_rec(
                &i,
                OrderState::PartiallyFilled,
                Some(partial.venue_order_id),
                "4",
                2_000,
                Some(fill_of(&i, d, 2_000)),
            ),
            position_rec("4", "500.05", 2_000),
        ]
    );
    let held = state.positions.get(spy());
    assert_eq!(held.qty, dec("4"));
    assert_eq!(held.avg_cost, dec("500.05"));
}

#[tokio::test]
async fn state_change_without_fill_emits_one_order_row_and_no_position() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    let sink = MemorySink::new();

    let acc = accepted_from(&i);
    assert!(
        record_stream_update(&mut state, &sink, &update(acc, None, 2_000))
            .await
            .unwrap()
    );

    assert_eq!(
        sink.records(),
        vec![order_rec(
            &i,
            OrderState::Accepted,
            Some(acc.venue_order_id),
            "0",
            2_000,
            None,
        )]
    );
    assert!(state.positions.quantities().is_empty());
}

#[tokio::test]
async fn duplicate_stream_message_emits_nothing_the_second_time() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    let sink = MemorySink::new();
    let msg = update(
        snapshot(&i, OrderState::PartiallyFilled, "4", Some("500.05")),
        Some(detail("t-1", "500.05", "4")),
        2_000,
    );
    record_stream_update(&mut state, &sink, &msg).await.unwrap();
    let after_first = sink.records();
    assert_eq!(after_first.len(), 2);

    assert!(record_stream_update(&mut state, &sink, &msg).await.unwrap());

    assert_eq!(sink.records(), after_first);
    assert_eq!(state.positions.get(spy()).qty, dec("4"));
}

#[tokio::test]
async fn update_for_an_untracked_order_is_ignored() {
    let mut state = OrderBookState::default();
    let sink = MemorySink::new();
    let stranger = intent("nisse-9-0", "3", None);
    let msg = update(
        accepted_from(&stranger),
        Some(detail("t-9", "10", "3")),
        2_000,
    );

    assert!(!record_stream_update(&mut state, &sink, &msg).await.unwrap());

    assert!(sink.records().is_empty());
    assert!(state.tracker.get(cid("nisse-9-0")).is_none());
    assert!(state.tracker.open_orders().next().is_none());
    assert!(state.positions.quantities().is_empty());
}

#[tokio::test]
async fn untracked_update_is_ok_even_when_the_sink_is_failing() {
    let mut state = OrderBookState::default();
    let sink = MemorySink::new();
    sink.fail_with("store queue closed");
    let stranger = intent("nisse-9-0", "3", None);

    let foreign = update(accepted_from(&stranger), None, 2_000);
    assert!(
        !record_stream_update(&mut state, &sink, &foreign)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn sell_fill_reduces_the_position_and_records_the_remainder() {
    let buy = intent("nisse-1-0", "10", Some("500"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(buy).unwrap();
    let sink = MemorySink::new();
    let bought = snapshot(&buy, OrderState::Filled, "10", Some("500"));
    let buy_detail = detail("t-b", "500", "10");
    record_stream_update(&mut state, &sink, &update(bought, Some(buy_detail), 2_000))
        .await
        .unwrap();

    let sell = OrderIntent {
        side: OrderSide::Sell,
        ..intent("nisse-2-0", "4", Some("501"))
    };
    state.tracker.track_intent(sell).unwrap();
    let sold = snapshot(&sell, OrderState::Filled, "4", Some("501"));
    let sell_detail = detail("t-s", "501", "4");
    assert!(
        record_stream_update(&mut state, &sink, &update(sold, Some(sell_detail), 3_000))
            .await
            .unwrap()
    );

    assert_eq!(
        sink.records(),
        vec![
            order_rec(
                &buy,
                OrderState::Filled,
                Some(bought.venue_order_id),
                "10",
                2_000,
                Some(fill_of(&buy, buy_detail, 2_000)),
            ),
            position_rec("10", "500", 2_000),
            order_rec(
                &sell,
                OrderState::Filled,
                Some(sold.venue_order_id),
                "4",
                3_000,
                Some(fill_of(&sell, sell_detail, 3_000)),
            ),
            position_rec("6", "500", 3_000),
        ]
    );
    assert_eq!(state.positions.get(spy()).qty, dec("6"));
}

#[tokio::test]
async fn two_partial_fills_emit_two_order_rows_and_two_positions() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, at(1_500))
        .unwrap();
    let sink = MemorySink::new();

    let first = snapshot(&i, OrderState::PartiallyFilled, "3", Some("500"));
    let first_detail = detail("t-1", "500", "3");
    record_stream_update(&mut state, &sink, &update(first, Some(first_detail), 2_000))
        .await
        .unwrap();

    let second = snapshot(&i, OrderState::PartiallyFilled, "5", Some("501.2"));
    let second_detail = detail("t-2", "503", "2");
    record_stream_update(
        &mut state,
        &sink,
        &update(second, Some(second_detail), 3_000),
    )
    .await
    .unwrap();

    assert_eq!(
        sink.records(),
        vec![
            order_rec(
                &i,
                OrderState::PartiallyFilled,
                Some(first.venue_order_id),
                "3",
                2_000,
                Some(fill_of(&i, first_detail, 2_000)),
            ),
            position_rec("3", "500", 2_000),
            order_rec(
                &i,
                OrderState::PartiallyFilled,
                Some(second.venue_order_id),
                "5",
                3_000,
                Some(fill_of(&i, second_detail, 3_000)),
            ),
            position_rec("5", "501.2", 3_000),
        ]
    );
}

#[tokio::test]
async fn failing_sink_on_stream_update_is_a_store_error() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    let sink = MemorySink::new();
    sink.fail_with("store queue closed");
    let msg = update(
        snapshot(&i, OrderState::PartiallyFilled, "4", Some("500.05")),
        Some(detail("t-1", "500.05", "4")),
        2_000,
    );

    let err = record_stream_update(&mut state, &sink, &msg)
        .await
        .unwrap_err();

    assert!(matches!(err, EngineError::Store(_)));
    assert!(sink.records().is_empty());
}

#[tokio::test]
async fn clean_reconciliation_emits_nothing() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, now())
        .unwrap();
    let broker = broker();
    broker.set_open_orders(vec![accepted_from(&i)]);
    let sink = MemorySink::new();

    let report = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(report, SyncReport::default());
    assert!(sink.records().is_empty());
}

#[tokio::test]
async fn fill_found_by_the_pass_is_recorded_and_the_position_comes_from_the_broker() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, now())
        .unwrap();
    let broker = broker();
    let filled = snapshot(&i, OrderState::Filled, "10", Some("500.05"));
    broker.insert_order(filled);
    // The broker's average (500.2) differs from the fill price (500.05) on purpose.
    broker.set_positions(vec![broker_long("10", "500.2")]);
    let sink = MemorySink::new();

    let report = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let fill = report.fills[0];
    assert_eq!(fill.qty.as_decimal(), dec("10"));
    assert_eq!(fill.px.as_decimal(), dec("500.05"));
    assert_eq!(
        report.findings,
        vec![
            Finding::NotOpenAtBroker {
                client_order_id: cid("nisse-1-0"),
            },
            Finding::PositionMismatch {
                symbol: spy(),
                local: dec("0"),
                broker: dec("10"),
                broker_avg_px: Some(dec("500.2")),
            },
        ]
    );
    assert_eq!(
        sink.records(),
        vec![
            order_rec(
                &i,
                OrderState::Filled,
                Some(filled.venue_order_id),
                "10",
                // The broker's time for the order, not the engine clock.
                filled.ts.as_u64(),
                Some(fill),
            ),
            position_rec("10", "500.2", 9_000),
        ]
    );
    let held = state.positions.get(spy());
    assert_eq!(held.qty, dec("10"));
    assert_eq!(held.avg_cost, dec("500.2"));
}

#[tokio::test]
async fn second_reconciliation_after_a_found_fill_emits_nothing() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, now())
        .unwrap();
    let broker = broker();
    broker.insert_order(snapshot(&i, OrderState::Filled, "10", Some("500.05")));
    broker.set_positions(vec![broker_long("10", "500.2")]);
    let sink = MemorySink::new();
    reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();
    let after_first = sink.records();
    assert_eq!(after_first.len(), 2);

    let again = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(again, SyncReport::default());
    assert_eq!(sink.records(), after_first);
}

#[tokio::test]
async fn failing_sink_during_reconciliation_is_a_store_error() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, now())
        .unwrap();
    let broker = broker();
    broker.insert_order(snapshot(&i, OrderState::Filled, "10", Some("500.05")));
    broker.set_positions(vec![broker_long("10", "500.2")]);
    let sink = MemorySink::new();
    sink.fail_with("store queue closed");

    let err = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap_err();

    assert!(matches!(err, EngineError::Store(_)));
    assert!(sink.records().is_empty());
    assert!(state.positions.quantities().is_empty());
}

#[tokio::test]
async fn never_arrived_order_emits_a_rejected_row() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    let broker = broker();
    let sink = MemorySink::new();

    reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(
        sink.records(),
        vec![order_rec(&i, OrderState::Rejected, None, "0", 9_000, None)]
    );
}

#[tokio::test]
async fn revived_order_emits_a_row_with_its_new_state() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    let broker = broker();
    let sink = MemorySink::new();
    reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    let live = accepted_from(&i);
    broker.set_open_orders(vec![live]);
    reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(
        sink.records(),
        vec![
            order_rec(&i, OrderState::Rejected, None, "0", 9_000, None),
            order_rec(
                &i,
                OrderState::Accepted,
                Some(live.venue_order_id),
                "0",
                live.ts.as_u64(),
                None,
            ),
        ]
    );
}

#[tokio::test]
async fn broker_failure_is_a_broker_error_and_emits_nothing() {
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(i).unwrap();
    state
        .tracker
        .apply(&accepted_from(&i), None, now())
        .unwrap();
    let broker = broker();
    broker.fail_reads("down");
    let sink = MemorySink::new();

    let err = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap_err();

    assert!(matches!(err, EngineError::Broker(_)));
    assert!(sink.records().is_empty());
}

#[tokio::test]
async fn foreign_open_order_is_reported_but_records_nothing() {
    let stranger = intent("manual-7", "3", None);
    let mut state = OrderBookState::default();
    let broker = broker();
    let foreign = accepted_from(&stranger);
    broker.set_open_orders(vec![foreign]);
    let sink = MemorySink::new();

    let report = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();

    assert_eq!(
        report.findings,
        vec![Finding::UnknownAtBroker { order: foreign }]
    );
    assert!(sink.records().is_empty());
    assert!(state.tracker.get(cid("manual-7")).is_none());
}

// Regression tests for the Phase 2 independent review.

#[tokio::test]
async fn a_failed_lookup_does_not_lose_fills_already_found_in_the_pass() {
    // Two orders. The first has drifted (a fill we missed); the second is not in the open
    // list and its lookup fails. The first order's fill must still be recorded.
    let first = intent("nisse-1-0", "10", Some("500.10"));
    let second = intent("nisse-2-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    state.tracker.track_intent(first).unwrap();
    state.tracker.track_intent(second).unwrap();
    state
        .tracker
        .apply_snapshot(&accepted_from(&second))
        .unwrap();
    let broker = broker();
    broker.set_open_orders(vec![snapshot(
        &first,
        OrderState::PartiallyFilled,
        "5",
        Some("500.05"),
    )]);
    broker.fail_lookups("timeout");
    let sink = MemorySink::new();

    let report = reconcile_and_record(&broker, &mut state, &sink, &clock(), opts(0))
        .await
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.problems.len(), 1);
    assert_eq!(report.problems[0].client_order_id, cid("nisse-2-0"));
    let recorded_fills = sink
        .records()
        .iter()
        .filter(|record| matches!(record, StoreRecord::Order { fill: Some(_), .. }))
        .count();
    assert_eq!(
        recorded_fills, 1,
        "the fill found before the failure is recorded"
    );
}

#[tokio::test]
async fn an_order_filled_on_submit_gets_its_fill_and_position_recorded() {
    let log = support::new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    let i = intent("nisse-1-0", "10", None);
    broker.push_submit_result(Ok(snapshot(&i, OrderState::Filled, "10", Some("500.05"))));
    let gateway = nisse_engine::OrderGateway::new(broker, support::MemoryIntentLog::new(log));
    let mut state = OrderBookState::default();
    let sink = MemorySink::new();

    let submitted = nisse_engine::submit_and_record(&gateway, &mut state, &sink, &i)
        .await
        .unwrap();
    let fill = submitted.applied.fill.unwrap();
    let records = sink.records();
    assert_eq!(records.len(), 2);
    assert!(
        matches!(&records[0], StoreRecord::Order { event, fill: Some(f) }
            if event.state == OrderState::Filled && *f == fill)
    );
    assert_eq!(records[1], position_rec("10", "500.05", fill.ts.as_u64()));
    assert_eq!(state.positions.get(spy()).qty, dec("10"));

    // The stream then repeats the fill: nothing more is recorded or counted.
    let repeat = update(submitted.order, None, submitted.order.ts.as_u64());
    record_stream_update(&mut state, &sink, &repeat)
        .await
        .unwrap();
    assert_eq!(sink.records().len(), 2);
    assert_eq!(state.positions.get(spy()).qty, dec("10"));
}

#[tokio::test]
async fn a_rejected_submit_records_the_rejection_and_returns_the_error() {
    let log = support::new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    broker.push_submit_result(Err(nisse_core::SubmitError::Rejected(support::FakeError(
        "insufficient buying power".to_owned(),
    ))));
    let gateway = nisse_engine::OrderGateway::new(broker, support::MemoryIntentLog::new(log));
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    let sink = MemorySink::new();

    let err = nisse_engine::submit_and_record(&gateway, &mut state, &sink, &i)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::OrderRejected { .. }));
    assert_eq!(
        sink.records(),
        vec![order_rec(
            &i,
            OrderState::Rejected,
            None,
            "0",
            i.ts.as_u64(),
            None
        )]
    );
    assert_eq!(state.positions.get(spy()).qty, dec("0"));
}

#[tokio::test]
async fn an_unknown_submit_outcome_records_nothing() {
    let log = support::new_call_log();
    let broker = FakeBroker::new(std::sync::Arc::clone(&log));
    broker.push_submit_result(Err(nisse_core::SubmitError::Unknown(support::FakeError(
        "timeout".to_owned(),
    ))));
    let gateway = nisse_engine::OrderGateway::new(broker, support::MemoryIntentLog::new(log));
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut state = OrderBookState::default();
    let sink = MemorySink::new();

    let err = nisse_engine::submit_and_record(&gateway, &mut state, &sink, &i)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::OrderOutcomeUnknown { .. }));
    assert!(sink.records().is_empty());
}
