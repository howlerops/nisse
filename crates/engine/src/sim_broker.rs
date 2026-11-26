//! A simulated broker for backtests, behind the same [`Broker`] trait as the real one, so the
//! engine's order path is identical in backtest and live.

use std::collections::BTreeMap;

use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, OpenOrders, OrderIntent, OrderUpdate,
    ReplaceRequest, SubmitError,
    nautilus::{Bar, ClientOrderId, Symbol, VenueOrderId},
};
use rust_decimal::Decimal;

use crate::{CostModel, EngineError};

/// The simulated account. Interior state sits behind a `Mutex` because [`Broker`] methods
/// take `&self`; the lock is never held across an `.await` (no method awaits anything).
#[derive(Debug)]
pub struct SimBroker {
    state: std::sync::Mutex<SimState>,
    costs: CostModel,
}

/// Everything the simulation tracks.
#[derive(Debug, Default)]
struct SimState {
    cash: Decimal,
    /// Signed shares and average cost per symbol.
    positions: BTreeMap<Symbol, (Decimal, Decimal)>,
    /// Last close seen per symbol, used to mark positions.
    marks: BTreeMap<Symbol, Decimal>,
    /// Orders accepted and not yet filled or cancelled, in acceptance order.
    open: Vec<BrokerOrder>,
    /// Every order ever accepted, by client order id.
    orders: BTreeMap<ClientOrderId, BrokerOrder>,
    /// Updates produced by fills and cancels, waiting for the engine to drain them.
    updates: Vec<OrderUpdate>,
    next_order: u64,
    next_fill: u64,
    fees_paid: Decimal,
}

#[allow(unused_variables, reason = "stub: Phase 3 task SB1")]
impl SimBroker {
    /// A new account holding `starting_cash` and nothing else.
    #[must_use]
    pub fn new(starting_cash: Decimal, costs: CostModel) -> Self {
        todo!("Phase 3 task SB1")
    }

    /// Advances the simulation by one bar: fills every open order for the bar's symbol at the
    /// bar's **open** (through the cost model), then marks the symbol at the bar's close.
    /// Market orders always fill; a limit order fills only if the open is at or better than
    /// its limit, at the open. Fills queue `OrderUpdate`s for [`Self::drain_updates`].
    /// Rules are in `docs/phases/phase-3.md`, "Simulated broker rules".
    pub fn on_bar(&self, bar: &Bar) -> Result<(), EngineError> {
        todo!("Phase 3 task SB1")
    }

    /// Takes the updates produced since the last call, oldest first. The engine applies them
    /// exactly as it applies the live order stream.
    #[must_use]
    pub fn drain_updates(&self) -> Vec<OrderUpdate> {
        todo!("Phase 3 task SB1")
    }

    /// Total fees charged so far.
    #[must_use]
    pub fn fees_paid(&self) -> Decimal {
        todo!("Phase 3 task SB1")
    }
}

/// The simulated broker never fails; this type only satisfies the trait.
#[derive(Debug, thiserror::Error)]
#[error("simulated broker: {0}")]
pub struct SimError(pub String);

#[allow(unused_variables, reason = "stub: Phase 3 task SB1")]
impl Broker for SimBroker {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        todo!("Phase 3 task SB1")
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        todo!("Phase 3 task SB1")
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        todo!("Phase 3 task SB1")
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        todo!("Phase 3 task SB1")
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        todo!("Phase 3 task SB1")
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        todo!("Phase 3 task SB1")
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        todo!("Phase 3 task SB1")
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        todo!("Phase 3 task SB1")
    }
}
