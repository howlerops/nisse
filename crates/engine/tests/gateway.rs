//! Acceptance tests for the write-ahead order gateway (phase 1, T7).

mod support;

use std::sync::Arc;

use nisse_core::{
    OrderState, SubmitError,
    nautilus::{ClientOrderId, VenueOrderId},
};
use nisse_engine::{EngineError, OrderGateway, OrderTracker};
use support::{
    Call, FakeBroker, FakeError, MemoryIntentLog, accepted_from, calls, intent, new_call_log,
    snapshot,
};

type Gateway = OrderGateway<FakeBroker, MemoryIntentLog>;

/// A gateway with its broker and intent log wired to one shared call log. The returned
/// handles share state with the gateway's copies, so tests can script and inspect them.
struct Fixture {
    log: support::CallLog,
    broker: FakeBroker,
    intents: MemoryIntentLog,
    gateway: Gateway,
}

fn fixture() -> Fixture {
    let log = new_call_log();
    let broker = FakeBroker::new(Arc::clone(&log));
    let intents = MemoryIntentLog::new(Arc::clone(&log));
    let gateway = OrderGateway::new(broker.clone(), intents.clone());
    Fixture {
        log,
        broker,
        intents,
        gateway,
    }
}

fn cid(id: &str) -> ClientOrderId {
    ClientOrderId::new(id)
}

fn venue(id: &str) -> VenueOrderId {
    VenueOrderId::new(format!("venue-{id}"))
}

fn tracked_state(tracker: &OrderTracker, id: &str) -> Option<OrderState> {
    tracker.get(cid(id)).map(|tracked| tracked.state)
}

#[tokio::test]
async fn intent_is_recorded_before_submit() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    let i = intent("nisse-1-0", "10", Some("500.10"));

    f.gateway.submit(&mut tracker, &i).await.unwrap();

    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
        ]
    );
    assert_eq!(f.intents.intents(), [i]);
}

#[tokio::test]
async fn intent_log_failure_does_not_call_broker() {
    let f = fixture();
    f.intents.fail_with("disk full");
    let mut tracker = OrderTracker::new();
    let i = intent("nisse-1-0", "10", Some("500.10"));

    let err = f.gateway.submit(&mut tracker, &i).await.unwrap_err();

    assert!(matches!(
        err,
        EngineError::IntentNotRecorded { client_order_id, .. } if client_order_id == cid("nisse-1-0")
    ));
    assert_eq!(calls(&f.log), [Call::RecordIntent(cid("nisse-1-0"))]);
    assert!(f.intents.intents().is_empty());
    assert_eq!(
        tracked_state(&tracker, "nisse-1-0"),
        Some(OrderState::Rejected)
    );
    assert_eq!(tracker.open_orders().count(), 0);
}

#[tokio::test]
async fn duplicate_client_order_id_is_refused_without_recording_or_sending() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap();

    let err = f
        .gateway
        .submit(&mut tracker, &intent("nisse-1-0", "5", Some("499.00")))
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateOrder(c) if c == cid("nisse-1-0")
    ));
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
        ]
    );
    assert_eq!(f.intents.intents().len(), 1);
    // The first intent is kept, not overwritten by the duplicate.
    assert_eq!(
        tracker
            .get(cid("nisse-1-0"))
            .unwrap()
            .intent
            .qty
            .to_string(),
        "10"
    );
}

#[tokio::test]
async fn rejected_submit_returns_order_rejected_and_marks_tracker_rejected() {
    let f = fixture();
    f.broker
        .push_submit_result(Err(SubmitError::Rejected(FakeError(
            "insufficient bp".into(),
        ))));
    let mut tracker = OrderTracker::new();

    let err = f
        .gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::OrderRejected { client_order_id, .. } if client_order_id == cid("nisse-1-0")
    ));
    assert_eq!(
        tracked_state(&tracker, "nisse-1-0"),
        Some(OrderState::Rejected)
    );
    assert_eq!(
        calls(&f.log)
            .iter()
            .filter(|c| matches!(c, Call::Submit(_)))
            .count(),
        1
    );
    assert_eq!(tracker.open_orders().count(), 0);
}

#[tokio::test]
async fn unknown_submit_outcome_leaves_intent_open_and_is_not_retried() {
    let f = fixture();
    f.broker
        .push_submit_result(Err(SubmitError::Unknown(FakeError("timeout".into()))));
    let mut tracker = OrderTracker::new();

    let err = f
        .gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::OrderOutcomeUnknown { client_order_id, .. } if client_order_id == cid("nisse-1-0")
    ));
    assert_eq!(
        tracked_state(&tracker, "nisse-1-0"),
        Some(OrderState::Intent)
    );
    let open: Vec<ClientOrderId> = tracker
        .open_orders()
        .map(|o| o.intent.client_order_id)
        .collect();
    assert_eq!(open, [cid("nisse-1-0")]);
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
        ]
    );
}

#[tokio::test]
async fn accepted_submit_returns_scripted_order_and_applies_it() {
    let f = fixture();
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let scripted = accepted_from(&i);
    f.broker.push_submit_result(Ok(scripted));
    let mut tracker = OrderTracker::new();

    let order = f.gateway.submit(&mut tracker, &i).await.unwrap();

    assert_eq!(order, scripted);
    let tracked = tracker.get(cid("nisse-1-0")).unwrap();
    assert_eq!(tracked.venue_order_id, Some(venue("nisse-1-0")));
    assert_eq!(tracked.state, OrderState::Accepted);
    assert_eq!(tracked.cum_filled_qty.to_string(), "0");
}

#[tokio::test]
async fn default_submit_is_accepted_from_the_intent() {
    let f = fixture();
    let i = intent("nisse-1-0", "10", Some("500.10"));
    let mut tracker = OrderTracker::new();

    let order = f.gateway.submit(&mut tracker, &i).await.unwrap();

    assert_eq!(order, accepted_from(&i));
    assert_eq!(order.state, OrderState::Accepted);
    assert_eq!(order.venue_order_id, venue("nisse-1-0"));
    assert_eq!(order.ts, nisse_core::nautilus::UnixNanos::from(1_001_u64));
}

#[tokio::test]
async fn immediately_filled_submit_moves_tracker_to_filled() {
    let f = fixture();
    let i = intent("nisse-1-0", "10", Some("500.10"));
    f.broker
        .push_submit_result(Ok(snapshot(&i, OrderState::Filled, "10", Some("500.10"))));
    let mut tracker = OrderTracker::new();

    let order = f.gateway.submit(&mut tracker, &i).await.unwrap();

    assert_eq!(order.state, OrderState::Filled);
    let tracked = tracker.get(cid("nisse-1-0")).unwrap();
    assert_eq!(tracked.state, OrderState::Filled);
    assert_eq!(tracked.cum_filled_qty.to_string(), "10");
    assert_eq!(tracked.cum_filled_qty, i.qty);
    assert_eq!(tracked.avg_fill_px, order.avg_fill_px);
}

#[tokio::test]
async fn replace_records_replacement_then_calls_broker_with_original_venue_id() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    let original = intent("nisse-1-0", "10", Some("500.10"));
    f.gateway.submit(&mut tracker, &original).await.unwrap();

    let replacement = intent("nisse-1-1", "8", Some("499.90"));
    let scripted = snapshot(&replacement, OrderState::Accepted, "0", None);
    f.broker.push_replace_result(Ok(scripted));

    let order = f
        .gateway
        .replace(&mut tracker, cid("nisse-1-0"), &replacement)
        .await
        .unwrap();

    assert_eq!(order, scripted);
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
            Call::RecordIntent(cid("nisse-1-1")),
            Call::Replace(venue("nisse-1-0"), cid("nisse-1-1")),
        ]
    );
    assert_eq!(f.intents.intents(), [original, replacement]);
    let tracked = tracker.get(cid("nisse-1-1")).unwrap();
    assert_eq!(tracked.state, OrderState::Accepted);
    assert_eq!(tracked.venue_order_id, Some(venue("nisse-1-1")));
}

#[tokio::test]
async fn replace_of_untracked_order_is_unknown_and_logs_nothing() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    let replacement = intent("nisse-1-1", "8", Some("499.90"));

    let err = f
        .gateway
        .replace(&mut tracker, cid("ghost"), &replacement)
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::UnknownOrder(c) if c == cid("ghost")
    ));
    assert!(calls(&f.log).is_empty());
    assert!(f.intents.intents().is_empty());
    assert!(tracker.get(cid("nisse-1-1")).is_none());
}

#[tokio::test]
async fn replace_of_order_without_venue_id_is_unknown_and_logs_nothing_more() {
    let f = fixture();
    f.broker
        .push_submit_result(Err(SubmitError::Unknown(FakeError("timeout".into()))));
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap_err();
    let replacement = intent("nisse-1-1", "8", Some("499.90"));

    let err = f
        .gateway
        .replace(&mut tracker, cid("nisse-1-0"), &replacement)
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::UnknownOrder(c) if c == cid("nisse-1-0")
    ));
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
        ]
    );
    assert!(tracker.get(cid("nisse-1-1")).is_none());
}

#[tokio::test]
async fn rejected_replace_reports_new_id_and_leaves_original_untouched() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap();
    let before = *tracker.get(cid("nisse-1-0")).unwrap();
    f.broker
        .push_replace_result(Err(SubmitError::Rejected(FakeError("bad qty".into()))));

    let err = f
        .gateway
        .replace(
            &mut tracker,
            cid("nisse-1-0"),
            &intent("nisse-1-1", "8", Some("499.90")),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::OrderRejected { client_order_id, .. } if client_order_id == cid("nisse-1-1")
    ));
    assert_eq!(
        tracked_state(&tracker, "nisse-1-1"),
        Some(OrderState::Rejected)
    );
    assert_eq!(*tracker.get(cid("nisse-1-0")).unwrap(), before);
}

#[tokio::test]
async fn unknown_replace_outcome_leaves_new_intent_open_and_original_untouched() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap();
    let before = *tracker.get(cid("nisse-1-0")).unwrap();
    f.broker
        .push_replace_result(Err(SubmitError::Unknown(FakeError("reset".into()))));

    let err = f
        .gateway
        .replace(
            &mut tracker,
            cid("nisse-1-0"),
            &intent("nisse-1-1", "8", Some("499.90")),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::OrderOutcomeUnknown { client_order_id, .. } if client_order_id == cid("nisse-1-1")
    ));
    assert_eq!(
        tracked_state(&tracker, "nisse-1-1"),
        Some(OrderState::Intent)
    );
    assert_eq!(*tracker.get(cid("nisse-1-0")).unwrap(), before);
    assert_eq!(
        calls(&f.log)
            .iter()
            .filter(|c| matches!(c, Call::Replace(..)))
            .count(),
        1
    );
}

#[tokio::test]
async fn replace_with_failing_intent_log_does_not_call_broker() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap();
    let before = *tracker.get(cid("nisse-1-0")).unwrap();
    f.intents.fail_with("disk full");

    let err = f
        .gateway
        .replace(
            &mut tracker,
            cid("nisse-1-0"),
            &intent("nisse-1-1", "8", Some("499.90")),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::IntentNotRecorded { client_order_id, .. } if client_order_id == cid("nisse-1-1")
    ));
    assert_eq!(
        tracked_state(&tracker, "nisse-1-1"),
        Some(OrderState::Rejected)
    );
    assert_eq!(*tracker.get(cid("nisse-1-0")).unwrap(), before);
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
            Call::RecordIntent(cid("nisse-1-1")),
        ]
    );
}

#[tokio::test]
async fn replace_reusing_a_tracked_id_is_duplicate_and_sends_nothing() {
    let f = fixture();
    let mut tracker = OrderTracker::new();
    f.gateway
        .submit(&mut tracker, &intent("nisse-1-0", "10", Some("500.10")))
        .await
        .unwrap();

    let err = f
        .gateway
        .replace(
            &mut tracker,
            cid("nisse-1-0"),
            &intent("nisse-1-0", "8", Some("499.90")),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateOrder(c) if c == cid("nisse-1-0")
    ));
    assert_eq!(
        calls(&f.log),
        [
            Call::RecordIntent(cid("nisse-1-0")),
            Call::Submit(cid("nisse-1-0")),
        ]
    );
}

#[tokio::test]
async fn fake_reads_answer_from_scripted_state_and_fail_when_set() {
    use nisse_core::Broker;

    let f = fixture();
    let i = intent("nisse-1-0", "10", Some("500.10"));
    f.broker.insert_order(accepted_from(&i));

    let found = f.broker.order_by_client_id(cid("nisse-1-0")).await.unwrap();
    let missing = f.broker.order_by_client_id(cid("nisse-9-9")).await.unwrap();
    assert_eq!(found, Some(accepted_from(&i)));
    assert_eq!(missing, None);

    f.broker.fail_reads("connection reset");
    let err = f.broker.open_orders().await.unwrap_err();
    assert_eq!(err.to_string(), "connection reset");
    let err = f
        .broker
        .order_by_client_id(cid("nisse-1-0"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "connection reset");
    assert_eq!(
        calls(&f.log),
        [
            Call::OrderByClientId(cid("nisse-1-0")),
            Call::OrderByClientId(cid("nisse-9-9")),
            Call::OpenOrders,
            Call::OrderByClientId(cid("nisse-1-0")),
        ]
    );
}
