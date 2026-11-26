//! The awaited writes: decisions and the write-ahead intent. Everything else goes through
//! [`crate::StoreWriter`].

use nisse_core::nautilus::UnixNanos;
use nisse_core::{
    Decision, DecisionId, DecisionLog, IntentLog, KillSwitchSource, OrderEvent, OrderIntent,
    RestoredOrder, RiskEvent, StrategyVersion, StrategyVersionId,
};
use tokio_postgres::Client;

use crate::{
    StoreError,
    tables::{decision, heartbeat, order_event, risk_event, strategy_version},
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

    /// The orders a restarting engine must track. See [`order_event::load_live`]; pass the
    /// start of the never-arrived recheck window as `ended_since`.
    pub async fn load_live_orders(
        &self,
        ended_since: Option<UnixNanos>,
    ) -> Result<Vec<RestoredOrder>, StoreError> {
        order_event::load_live(&self.client, ended_since).await
    }

    /// Every risk event in id order. See [`risk_event::load_all`].
    pub async fn load_risk_events(&self) -> Result<Vec<RiskEvent>, StoreError> {
        risk_event::load_all(&self.client).await
    }

    /// Records that `source` was alive at `ts`. See [`heartbeat::insert`].
    pub async fn heartbeat(&self, source: &str, ts: UnixNanos) -> Result<(), StoreError> {
        heartbeat::insert(&self.client, source, ts).await
    }

    /// Writes one risk event now, awaited (for the kill switch and other changes that must
    /// not sit in a queue).
    pub async fn record_risk_event(&self, event: &RiskEvent) -> Result<(), StoreError> {
        risk_event::insert_many(&self.client, std::slice::from_ref(event)).await
    }
}

impl KillSwitchSource for Store {
    type Error = StoreError;

    /// See [`risk_event::kill_switch_engaged`]. An error means the caller must fail closed.
    async fn kill_switch_engaged(&self) -> Result<bool, StoreError> {
        risk_event::kill_switch_engaged(&self.client).await
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
