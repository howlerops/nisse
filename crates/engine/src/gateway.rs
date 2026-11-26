//! The only way an order leaves the engine: write the intent, then submit.

use nisse_core::{Broker, BrokerOrder, IntentLog, OrderIntent, ReplaceRequest};

use crate::{EngineError, OrderTracker};

/// Sends orders through a [`Broker`] with the write-ahead rule enforced: the intent is in the
/// [`IntentLog`] and in the [`OrderTracker`] before the broker hears about it.
#[derive(Debug)]
#[allow(dead_code, reason = "stub: Phase 1 task T7 reads the fields")]
pub struct OrderGateway<B, L> {
    broker: B,
    intents: L,
}

#[allow(unused_variables, reason = "stub: Phase 1 task T7")]
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
    ///    - `Ok(order)`: apply it to the tracker (event time `order.ts`) and return it.
    ///    - `Rejected`: mark the order never-arrived at `intent.ts`; return `OrderRejected`.
    ///    - `Unknown`: leave the tracker in `Intent` (reconciliation resolves it by client
    ///      order id) and return `OrderOutcomeUnknown`.
    pub async fn submit(
        &self,
        tracker: &mut OrderTracker,
        intent: &OrderIntent,
    ) -> Result<BrokerOrder, EngineError> {
        todo!("Phase 1 task T7")
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
    ) -> Result<BrokerOrder, EngineError> {
        todo!("Phase 1 task T7")
    }
}

/// The [`ReplaceRequest`] that turns the live order into `replacement`.
#[allow(dead_code, reason = "stub: Phase 1 task T7 uses it")]
fn replace_request(replacement: &OrderIntent) -> ReplaceRequest {
    ReplaceRequest {
        new_client_order_id: replacement.client_order_id,
        qty: Some(replacement.qty),
        limit_px: replacement.limit_px,
    }
}
