//! The awaited writes: decisions and the write-ahead intent. Everything else goes through
//! [`crate::StoreWriter`].

use nisse_core::{
    Decision, DecisionId, DecisionLog, IntentLog, OrderEvent, OrderIntent, RestoredOrder,
    StrategyVersion, StrategyVersionId,
};
use tokio_postgres::Client;

use crate::{
    StoreError,
    tables::{decision, order_event, strategy_version},
};

/// A connection used for writes the engine waits for. Each call is one small statement.
#[derive(Debug)]
pub struct Store {
    client: Client,
}

impl Store {
    #[must_use]
    pub const fn new(client: Client) -> Self {
        Self { client }
    }

    /// The underlying connection, for reads.
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// See [`strategy_version::ensure`].
    pub async fn ensure_strategy_version(
        &self,
        version: &StrategyVersion,
    ) -> Result<StrategyVersionId, StoreError> {
        strategy_version::ensure(&self.client, version).await
    }

    /// See [`order_event::load_open`].
    pub async fn load_open_orders(&self) -> Result<Vec<RestoredOrder>, StoreError> {
        order_event::load_open(&self.client).await
    }
}

impl DecisionLog for Store {
    type Error = StoreError;

    async fn record_decision(&self, decision: &Decision) -> Result<DecisionId, StoreError> {
        decision::insert(&self.client, decision).await
    }
}

impl IntentLog for Store {
    type Error = StoreError;

    /// Writes the intent row. This is the one synchronous store write on the order path.
    async fn record_intent(&self, intent: &OrderIntent) -> Result<(), StoreError> {
        order_event::insert(&self.client, &OrderEvent::intent(intent)).await
    }
}
