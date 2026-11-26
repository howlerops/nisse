//! The trading loop: bars in, decisions and orders out. One code path for backtest and live;
//! only the broker, the clock and the store behind the traits differ.

use std::collections::BTreeMap;

use nisse_core::{
    Action, Broker, Clock, Decision, DecisionId, DecisionLog, Features, IntentLog, OrderIntent,
    OrderUpdate, RecordSink, StrategyVersionId,
    nautilus::{Bar, ClientOrderId, OrderSide, OrderType, Quantity, Symbol, TimeInForce},
};
use rust_decimal::Decimal;

use crate::{
    DecisionContext, EngineError, OrderBookState, OrderGateway, Strategy, SyncOptions,
    decide_and_record,
    features::{RollingStats, WindowSpec},
    reconcile_and_record, record_stream_update,
    strategies::feature,
    submit_and_record,
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
        if config.symbols.is_empty() {
            return Err(EngineError::InvalidConfig(
                "the universe has no symbols".to_owned(),
            ));
        }
        if config
            .symbols
            .windows(2)
            .any(|pair| pair[0].as_str() >= pair[1].as_str())
        {
            return Err(EngineError::InvalidConfig(
                "the universe must be sorted by symbol with no duplicates".to_owned(),
            ));
        }
        let mut windows = BTreeMap::new();
        for &symbol in &config.symbols {
            windows.insert(symbol, RollingStats::new(config.window)?);
        }
        Ok(Self {
            strategy,
            gateway,
            decisions,
            sink,
            clock,
            config,
            state,
            windows,
            decisions_made: 0,
        })
    }

    /// Feeds one daily bar. This is the per-bar path: one map lookup and one O(1) window
    /// update, no allocation. A bar for a symbol outside the universe is ignored.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<(), EngineError> {
        let symbol = bar.bar_type.instrument_id().symbol;
        match self.windows.get_mut(&symbol) {
            Some(window) => window.push(bar.close.as_decimal()),
            None => Ok(()),
        }
    }

    /// Applies one broker order update (live: from the order stream; backtest: from the
    /// simulated broker) and records what it changed.
    pub async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<(), EngineError> {
        record_stream_update(&mut self.state, &self.sink, update)
            .await
            .map(|_| ())
    }

    /// Decides for every symbol and sends the resulting orders. Steps are in
    /// `docs/phases/phase-3.md`, "Rebalance steps".
    pub async fn rebalance(&mut self) -> Result<RebalanceReport, EngineError> {
        let report = reconcile_and_record(
            self.gateway.broker(),
            &mut self.state,
            &self.sink,
            &self.clock,
            self.config.sync,
        )
        .await?;
        if !report.is_reconciled() {
            return Err(EngineError::NotReconciled(report.problems.len()));
        }

        let equity = self
            .gateway
            .broker()
            .account()
            .await
            .map_err(|e| EngineError::Broker(Box::new(e)))?
            .equity
            .as_decimal();

        // Phase A: decide for every symbol before any order is sent, all from one equity.
        let universe = self.config.symbols.len();
        let mut decided: Vec<(DecisionId, Decision)> = Vec::with_capacity(universe);
        let mut out = RebalanceReport {
            decisions: Vec::with_capacity(universe),
            ..RebalanceReport::default()
        };
        for &symbol in &self.config.symbols {
            let features = self.features_for(symbol, equity);
            let context = DecisionContext {
                strategy_version_id: self.config.strategy_version_id,
                symbol,
                features,
                data_snapshot_id: self.config.data_snapshot_id.clone(),
                rng_seed: self.config.seed.wrapping_add(self.decisions_made),
                ts: self.clock.now(),
            };
            self.decisions_made = self.decisions_made.wrapping_add(1);
            let (id, decision) =
                decide_and_record(&self.strategy, &self.decisions, context).await?;
            out.decisions.push((id, symbol, decision.action));
            decided.push((id, decision));
        }

        // Phase B: sells first, then buys, each in universe order.
        for (id, decision) in &decided {
            if decision.intended_qty < Decimal::ZERO {
                self.send(*id, decision, &mut out).await?;
            }
        }
        for (id, decision) in &decided {
            if decision.intended_qty > Decimal::ZERO {
                self.send(*id, decision, &mut out).await?;
            }
        }
        Ok(out)
    }

    /// The features one decision is made from, for `symbol`. The symbol's close, history and
    /// momentum are absent until its window has bars, so the strategy skips it.
    fn features_for(&self, symbol: Symbol, equity: Decimal) -> Features {
        let mut features = Features::new();
        features.insert(
            feature::POSITION_QTY.to_owned(),
            self.state.positions.get(symbol).qty,
        );
        features.insert(feature::EQUITY.to_owned(), equity);
        let n_assets = u64::try_from(self.config.symbols.len()).unwrap_or(u64::MAX);
        features.insert(feature::N_ASSETS.to_owned(), Decimal::from(n_assets));
        if let Some(stats) = self.windows.get(&symbol).and_then(RollingStats::stats) {
            features.insert(feature::CLOSE.to_owned(), stats.close);
            features.insert(
                feature::HISTORY_DAYS.to_owned(),
                Decimal::from(stats.history_days),
            );
            if let Some(momentum) = stats.momentum {
                features.insert(feature::MOMENTUM.to_owned(), momentum);
            }
            if let Some(var_daily) = stats.var_daily {
                features.insert(feature::VAR_DAILY.to_owned(), var_daily);
            }
        }
        features
    }

    /// Sends the order for one non-zero decision and counts the outcome in `report`. A broker
    /// refusal or an unknown outcome is counted and does not stop the rebalance.
    async fn send(
        &mut self,
        decision_id: DecisionId,
        decision: &Decision,
        report: &mut RebalanceReport,
    ) -> Result<(), EngineError> {
        let qty = Quantity::from_decimal(decision.intended_qty.abs()).map_err(|_| {
            EngineError::InvalidConfig("order quantity not representable".to_owned())
        })?;
        let side = if decision.intended_qty < Decimal::ZERO {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let intent = OrderIntent {
            decision_id,
            client_order_id: ClientOrderId::new(format!("nisse-{}-0", decision_id.0)),
            symbol: decision.symbol,
            side,
            order_type: OrderType::Market,
            qty,
            limit_px: None,
            time_in_force: TimeInForce::Day,
            ts: self.clock.now(),
        };
        match submit_and_record(&self.gateway, &mut self.state, &self.sink, &intent).await {
            Ok(_) => report.orders_submitted += 1,
            Err(EngineError::OrderRejected { .. } | EngineError::OrderOutcomeUnknown { .. }) => {
                report.orders_failed += 1;
            }
            Err(err) => return Err(err),
        }
        Ok(())
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
