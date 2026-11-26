//! An in-memory store for backtests and tests: the same three write interfaces the Postgres
//! store implements, backed by vectors. A backtest's decisions are not real decisions and are
//! never written to the decision store; only the trial is.

use std::sync::{Mutex, MutexGuard, PoisonError};

use nisse_core::{
    Decision, DecisionId, DecisionLog, IntentLog, OrderIntent, RecordSink, StoreRecord,
};

/// Never returned: writing to memory cannot fail.
#[derive(Debug, thiserror::Error)]
#[error("memory store cannot fail")]
pub struct MemoryStoreError;

#[derive(Debug, Default)]
struct Inner {
    decisions: Vec<Decision>,
    intents: Vec<OrderIntent>,
    records: Vec<StoreRecord>,
}

/// Decisions, intents and records held in memory, in arrival order.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock only means another thread panicked mid-push; the data is still a
        // valid prefix, and this type is used single-threaded.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every decision recorded so far; a decision's id is its 1-based position.
    #[must_use]
    pub fn decisions(&self) -> Vec<Decision> {
        self.lock().decisions.clone()
    }

    #[must_use]
    pub fn intents(&self) -> Vec<OrderIntent> {
        self.lock().intents.clone()
    }

    #[must_use]
    pub fn records(&self) -> Vec<StoreRecord> {
        self.lock().records.clone()
    }
}

impl DecisionLog for MemoryStore {
    type Error = MemoryStoreError;

    async fn record_decision(&self, decision: &Decision) -> Result<DecisionId, MemoryStoreError> {
        let mut inner = self.lock();
        inner.decisions.push(decision.clone());
        Ok(DecisionId(
            i64::try_from(inner.decisions.len()).unwrap_or(i64::MAX),
        ))
    }
}

impl IntentLog for MemoryStore {
    type Error = MemoryStoreError;

    async fn record_intent(&self, intent: &OrderIntent) -> Result<(), MemoryStoreError> {
        self.lock().intents.push(*intent);
        Ok(())
    }
}

impl RecordSink for MemoryStore {
    type Error = MemoryStoreError;

    async fn emit(&self, record: StoreRecord) -> Result<(), MemoryStoreError> {
        self.lock().records.push(record);
        Ok(())
    }
}

// The engine holds one store and hands it out three times; references implement the traits
// so that needs no `Arc`.
impl DecisionLog for &MemoryStore {
    type Error = MemoryStoreError;

    async fn record_decision(&self, decision: &Decision) -> Result<DecisionId, MemoryStoreError> {
        (**self).record_decision(decision).await
    }
}

impl IntentLog for &MemoryStore {
    type Error = MemoryStoreError;

    async fn record_intent(&self, intent: &OrderIntent) -> Result<(), MemoryStoreError> {
        (**self).record_intent(intent).await
    }
}

impl RecordSink for &MemoryStore {
    type Error = MemoryStoreError;

    async fn emit(&self, record: StoreRecord) -> Result<(), MemoryStoreError> {
        (**self).emit(record).await
    }
}

#[cfg(test)]
mod tests {
    use nisse_core::{
        Action, Features, PositionEvent, StrategyVersionId, feature_hash,
        nautilus::{Symbol, UnixNanos},
    };

    use super::*;

    /// Records through the `&MemoryStore` implementation, as the engine does.
    async fn by_ref(store: &MemoryStore, decision: &Decision) -> DecisionId {
        async fn record<L: DecisionLog>(log: L, decision: &Decision) -> DecisionId {
            log.record_decision(decision).await.ok().unwrap()
        }
        record(store, decision).await
    }

    #[tokio::test]
    async fn keeps_what_it_is_given_in_order_and_numbers_decisions_from_one() {
        let store = MemoryStore::new();
        let features = Features::new();
        let decision = Decision {
            strategy_version_id: StrategyVersionId(1),
            symbol: Symbol::new("SPY"),
            action: Action::Hold,
            intended_qty: 0.into(),
            expected_edge_bps: None,
            expected_vol: None,
            feature_hash: feature_hash(&features),
            features,
            data_snapshot_id: "snap".to_owned(),
            rng_seed: 1,
            reason_code: "within_band".to_owned(),
            ts: UnixNanos::from(1),
        };
        assert_eq!(by_ref(&store, &decision).await, DecisionId(1));
        assert_eq!(
            store.record_decision(&decision).await.unwrap(),
            DecisionId(2)
        );
        let event = PositionEvent {
            symbol: Symbol::new("SPY"),
            qty_after: 1.into(),
            avg_cost: 2.into(),
            ts: UnixNanos::from(3),
        };
        store.emit(StoreRecord::Position(event)).await.unwrap();
        assert_eq!(store.decisions().len(), 2);
        assert_eq!(store.records(), vec![StoreRecord::Position(event)]);
        assert!(store.intents().is_empty());
    }
}
