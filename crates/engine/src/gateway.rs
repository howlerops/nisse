//! The only way an order leaves the engine: write the intent, then submit.

use nisse_core::{Broker, BrokerOrder, IntentLog, OrderIntent, ReplaceRequest, SubmitError};

use crate::{Applied, EngineError, OrderTracker};

/// The broker's answer to a submit or replace, and what it changed in the tracker. `applied.fill`
/// is set when the order came back already (partly) filled: the caller records it like any
/// other fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Submitted {
    pub order: BrokerOrder,
    pub applied: Applied,
}

/// Sends orders through a [`Broker`] with the write-ahead rule enforced: the intent is in the
/// [`IntentLog`] and in the [`OrderTracker`] before the broker hears about it.
#[derive(Debug)]
pub struct OrderGateway<B, L> {
    broker: B,
    intents: L,
}

impl<B: Broker, L: IntentLog> OrderGateway<B, L> {
    #[must_use]
    pub fn new(broker: B, intents: L) -> Self {
        Self { broker, intents }
    }

    #[must_use]
    pub fn broker(&self) -> &B {
        &self.broker
    }

    /// Submits an order. Steps, in this order:
    ///
    /// 1. `tracker.track_intent(*intent)`; a duplicate client order id stops here.
    /// 2. `intents.record_intent(intent)`. If it fails, return `IntentNotRecorded` and do not
    ///    call the broker (the tracker marks the order never-arrived at `intent.ts`).
    /// 3. `broker.submit(intent)`, exactly once.
    ///    - `Ok(order)`: apply it to the tracker (event time `order.ts`) and return it with the
    ///      tracker's [`Applied`] (which carries the fill if it came back filled).
    ///    - `Rejected`: mark the order never-arrived at `intent.ts`; return `OrderRejected`.
    ///    - `Unknown`: leave the tracker in `Intent` (reconciliation resolves it by client
    ///      order id) and return `OrderOutcomeUnknown`.
    pub async fn submit(
        &self,
        tracker: &mut OrderTracker,
        intent: &OrderIntent,
    ) -> Result<Submitted, EngineError> {
        self.record(tracker, intent).await?;
        let result = self.broker.submit(intent).await;
        settle(tracker, intent, result)
    }

    /// Replaces a live order. `replacement` is the full intent of the new order (its
    /// `client_order_id` is the new id) and goes through the same three steps as `submit`,
    /// with `broker.replace(venue_order_id_of(original), ..)` as step 3. Errors with
    /// `UnknownOrder` if `original` is not tracked or has no venue order id yet.
    pub async fn replace(
        &self,
        tracker: &mut OrderTracker,
        original: nisse_core::nautilus::ClientOrderId,
        replacement: &OrderIntent,
    ) -> Result<Submitted, EngineError> {
        let venue_order_id = tracker
            .get(original)
            .and_then(|tracked| tracked.venue_order_id)
            .ok_or(EngineError::UnknownOrder(original))?;
        self.record(tracker, replacement).await?;
        let result = self
            .broker
            .replace(venue_order_id, &replace_request(replacement))
            .await;
        settle(tracker, replacement, result)
    }

    /// Steps 1 and 2 of the write-ahead rule: track the intent, then write it to the intent
    /// log. On `Err` the broker has not been called.
    async fn record(
        &self,
        tracker: &mut OrderTracker,
        intent: &OrderIntent,
    ) -> Result<(), EngineError> {
        tracker.track_intent(*intent)?;
        if let Err(source) = self.intents.record_intent(intent).await {
            tracker.mark_never_arrived(intent.client_order_id, intent.ts)?;
            return Err(EngineError::IntentNotRecorded {
                client_order_id: intent.client_order_id,
                source: Box::new(source),
            });
        }
        Ok(())
    }
}

/// Step 3 of the write-ahead rule: applies the broker's answer for an intent that is already
/// recorded, and maps it to the caller's result. Shared by `submit` and `replace`.
fn settle<E>(
    tracker: &mut OrderTracker,
    intent: &OrderIntent,
    result: Result<BrokerOrder, SubmitError<E>>,
) -> Result<Submitted, EngineError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let client_order_id = intent.client_order_id;
    match result {
        Ok(order) => {
            let applied = tracker.apply(&order, None, order.ts)?;
            Ok(Submitted { order, applied })
        }
        Err(SubmitError::Rejected(source)) => {
            tracker.mark_never_arrived(client_order_id, intent.ts)?;
            Err(EngineError::OrderRejected {
                client_order_id,
                source: Box::new(source),
            })
        }
        Err(SubmitError::Unknown(source)) => Err(EngineError::OrderOutcomeUnknown {
            client_order_id,
            source: Box::new(source),
        }),
    }
}

/// The [`ReplaceRequest`] that turns the live order into `replacement`.
fn replace_request(replacement: &OrderIntent) -> ReplaceRequest {
    ReplaceRequest {
        new_client_order_id: replacement.client_order_id,
        qty: Some(replacement.qty),
        limit_px: replacement.limit_px,
    }
}
