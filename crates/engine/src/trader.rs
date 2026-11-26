//! The trading loop: bars in, decisions and orders out. One code path for backtest and live;
//! only the broker, the clock and the store behind the traits differ.

use std::collections::BTreeMap;

use nisse_core::{
    Action, Broker, Clock, DecisionId, DecisionLog, IntentLog, OrderUpdate, RecordSink,
    StrategyVersionId,
    nautilus::{Bar, Symbol},
};

use crate::{
    EngineError, OrderBookState, OrderGateway, Strategy, SyncOptions,
    features::{RollingStats, WindowSpec},
};

/// What the trader is told once, at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraderConfig {
    pub strategy_version_id: StrategyVersionId,
    /// The universe, in the order decisions are made (sorted, so it is deterministic).
    pub symbols: Vec<Symbol>,
    pub window: WindowSpec,
    /// Names the market data behind the features; stored on every decision.
    pub data_snapshot_id: String,
    /// Base seed; decision `n` (counting from 0) gets `seed + n`.
    pub seed: u64,
    pub sync: SyncOptions,
}

/// What one rebalance did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RebalanceReport {
    /// Every decision made, in universe order.
    pub decisions: Vec<(DecisionId, Symbol, Action)>,
    /// Orders the broker accepted.
    pub orders_submitted: u32,
    /// Orders the broker refused or whose outcome is unknown. The decisions stand; the
    /// orders are in the tracker and the store.
    pub orders_failed: u32,
}

/// The trading loop for one strategy.
#[derive(Debug)]
pub struct Trader<S, B, L, D, K, C> {
    strategy: S,
    gateway: OrderGateway<B, L>,
    decisions: D,
    sink: K,
    clock: C,
    config: TraderConfig,
    state: OrderBookState,
    /// One preallocated window per symbol in the universe.
    windows: BTreeMap<Symbol, RollingStats>,
    decisions_made: u64,
}

#[allow(unused_variables, reason = "stub: Phase 3 task EN1")]
impl<S, B, L, D, K, C> Trader<S, B, L, D, K, C>
where
    S: Strategy,
    B: Broker,
    L: IntentLog,
    D: DecisionLog,
    K: RecordSink,
    C: Clock,
{
    /// Builds the trader and allocates one rolling window per symbol. Errors with
    /// `InvalidConfig` when the universe is empty, unsorted or has a duplicate.
    pub fn new(
        strategy: S,
        gateway: OrderGateway<B, L>,
        decisions: D,
        sink: K,
        clock: C,
        config: TraderConfig,
        state: OrderBookState,
    ) -> Result<Self, EngineError> {
        todo!("Phase 3 task EN1")
    }

    /// Feeds one daily bar. This is the per-bar path: one map lookup and one O(1) window
    /// update, no allocation. A bar for a symbol outside the universe is ignored.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<(), EngineError> {
        todo!("Phase 3 task EN1")
    }

    /// Applies one broker order update (live: from the order stream; backtest: from the
    /// simulated broker) and records what it changed.
    pub async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<(), EngineError> {
        todo!("Phase 3 task EN1")
    }

    /// Decides for every symbol and sends the resulting orders. Steps are in
    /// `docs/phases/phase-3.md`, "Rebalance steps".
    pub async fn rebalance(&mut self) -> Result<RebalanceReport, EngineError> {
        todo!("Phase 3 task EN1")
    }

    #[must_use]
    pub const fn state(&self) -> &OrderBookState {
        &self.state
    }

    #[must_use]
    pub fn broker(&self) -> &B {
        self.gateway.broker()
    }
}
