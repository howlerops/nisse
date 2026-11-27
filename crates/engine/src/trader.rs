//! The trading loop: bars in, decisions and orders out. One code path for backtest and live;
//! only the broker, the clock and the store behind the traits differ.

use std::collections::BTreeMap;

use nisse_core::{
    Action, Broker, Clock, Decision, DecisionId, DecisionLog, Features, IntentLog,
    KillSwitchSource, OrderIntent, OrderState, OrderUpdate, RecordSink, RiskCheck, RiskEvent,
    RiskEventKind, StoreRecord, StrategyVersionId,
    nautilus::{
        Bar, ClientOrderId, OrderSide, OrderType, Quantity, Symbol, TimeInForce, UnixNanos,
    },
};
use nisse_risk::{DrawdownMode, Limits, OrderCheck, RiskGate, RiskState};
use rust_decimal::Decimal;

use crate::{
    DecisionContext, EngineError, Finding, OrderBookState, OrderGateway, Strategy, SyncOptions,
    SyncReport, decide_and_record,
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
    /// The risk limits every order is checked against.
    pub limits: Limits,
    /// The risk state restored from the store (`RiskState::default()` for a fresh account).
    pub risk: RiskState,
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
    /// Orders the risk gate refused. Their decisions stand, with the checks that blocked them.
    pub orders_blocked: u32,
}

/// What the risk gate needs about one symbol's order, from its decision and its window.
#[derive(Debug, Clone, Copy)]
struct GateInputs {
    /// Shares held plus shares on order: the position the decision was made from.
    position_qty: Decimal,
    /// Shares a sell may reduce: filled shares minus the engine's own unfilled sells. Shares
    /// that are only on order are not sellable.
    sellable_qty: Decimal,
    /// The symbol's last close, when it has any bar.
    reference_px: Option<Decimal>,
    /// The symbol's daily variance, when its window has enough returns.
    var_daily: Option<Decimal>,
}

/// The figures every order of one rebalance shares.
#[derive(Debug, Clone, Copy)]
struct RoundFacts {
    now: UnixNanos,
    equity: Decimal,
    trading_blocked: bool,
    /// Nanoseconds since the newest bar in the universe; `None` when there is no bar.
    data_age_nanos: Option<u64>,
}

/// Σ |position| × last close over the decided symbols, or `None` when a symbol with a
/// position has no close, or the sum overflows.
fn gross_exposure(decided: &[(DecisionId, Decision, GateInputs)]) -> Option<Decimal> {
    let mut gross = Decimal::ZERO;
    for (_, _, inputs) in decided {
        let exposure = inputs.position_qty.abs();
        if exposure.is_zero() {
            continue;
        }
        let px = inputs.reference_px?;
        gross = gross.checked_add(exposure.checked_mul(px)?)?;
    }
    Some(gross)
}

/// One symbol's rolling statistics and the time of the last bar that went into them.
#[derive(Debug)]
struct SymbolWindow {
    stats: RollingStats,
    last_bar_ts: Option<UnixNanos>,
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
    windows: BTreeMap<Symbol, SymbolWindow>,
    decisions_made: u64,
    /// The kill switch was engaged by this process. A refresh from the store never lowers
    /// it (the store may not have the row yet); only `release_kill_switch` does.
    kill_engaged_here: bool,
    /// The risk gate: limits, drawdown mode, kill switch, day-trade count.
    gate: RiskGate,
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
        let gate = RiskGate::new(config.limits.clone(), config.risk.clone());
        let mut windows = BTreeMap::new();
        for &symbol in &config.symbols {
            windows.insert(
                symbol,
                SymbolWindow {
                    stats: RollingStats::new(config.window)?,
                    last_bar_ts: None,
                },
            );
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
            kill_engaged_here: false,
            gate,
        })
    }

    /// Feeds one daily bar. This is the per-bar path: one map lookup and one O(1) window
    /// update, no allocation. A bar for a symbol outside the universe is ignored.
    ///
    /// A bar that is not newer than the symbol's last one is `InvalidBar` and changes
    /// nothing: a daily bar delivered twice (a reconnect, a re-fetch after a restart) would
    /// otherwise add a zero return and shift the lookback. The caller drops such a bar.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<(), EngineError> {
        let symbol = bar.bar_type.instrument_id().symbol;
        let Some(window) = self.windows.get_mut(&symbol) else {
            return Ok(());
        };
        if window.last_bar_ts.is_some_and(|last| bar.ts_event <= last) {
            return Err(EngineError::InvalidBar(
                "bar is not newer than the symbol's last bar",
            ));
        }
        window.stats.push(bar.close.as_decimal())?;
        window.last_bar_ts = Some(bar.ts_event);
        Ok(())
    }

    /// Applies one broker order update (live: from the order stream; backtest: from the
    /// simulated broker) and records what it changed. A fill also counts toward the day-trade
    /// limit.
    pub async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<(), EngineError> {
        let id = update.order.client_order_id;
        let filled_before = self.state.tracker.get(id).map(|order| order.cum_filled_qty);
        record_stream_update(&mut self.state, &self.sink, update).await?;
        // Only a fill that moved the order's filled quantity counts: a redelivered message
        // is not a new fill and must not look like one on another day.
        let filled_after = self.state.tracker.get(id).map(|order| order.cum_filled_qty);
        if let (Some(before), Some(after)) = (filled_before, filled_after)
            && after > before
        {
            self.record_day_trade_fill(id, update.ts);
        }
        Ok(())
    }

    /// Feeds the drawdown state with the account's equity. Emits an `equity_peak` event for a
    /// new high-water mark and a `drawdown_mode` event for a mode change, and returns the mode
    /// change when there was one.
    pub async fn observe_equity(
        &mut self,
        equity: Decimal,
    ) -> Result<Option<(DrawdownMode, DrawdownMode)>, EngineError> {
        let limits = self.gate.limits().clone();
        let observation = self.gate.drawdown_mut().observe(equity, &limits);
        let ts = self.clock.now();
        if let Some(peak) = observation.new_peak {
            self.emit(StoreRecord::RiskEvent(RiskEvent {
                kind: RiskEventKind::EquityPeak,
                detail: serde_json::json!({ "peak": peak.to_string() }),
                ts,
            }))
            .await?;
        }
        if let Some((_, to)) = observation.mode_change {
            self.emit(StoreRecord::RiskEvent(RiskEvent {
                kind: RiskEventKind::DrawdownMode,
                detail: serde_json::json!({
                    "mode": to.as_str(),
                    "equity": equity.to_string(),
                    "reset": false,
                }),
                ts,
            }))
            .await?;
        }
        // Durably: a restart must not come back with a lower peak or in a looser mode.
        if observation.new_peak.is_some() || observation.mode_change.is_some() {
            self.flush().await?;
        }
        Ok(observation.mode_change)
    }

    /// Sets the kill switch flag without touching the broker. Used when the flag is read from
    /// the store.
    pub const fn set_kill_switch(&mut self, engaged: bool) {
        // Lowering is ignored while a switch engaged in this process stands: only
        // `release_kill_switch` lowers that one.
        if engaged || !self.kill_engaged_here {
            self.gate.set_kill_switch(engaged);
        }
    }

    /// Reads the kill switch from `source` and sets the flag to what it says. A source that
    /// cannot be read engages the switch. Returns the flag now in force. Does not cancel
    /// orders; see [`Self::engage_kill_switch`].
    pub async fn refresh_kill_switch<Src: KillSwitchSource>(&mut self, source: &Src) -> bool {
        let engaged = match source.kill_switch_engaged().await {
            Ok(engaged) => engaged,
            Err(err) => {
                tracing::error!(error = %err, "kill switch unreadable; engaging it");
                true
            }
        };
        // A switch engaged in this process stays engaged whatever the source says: its row
        // may not have reached the store yet.
        let engaged = engaged || self.kill_engaged_here;
        self.gate.set_kill_switch(engaged);
        engaged
    }

    /// Releases the kill switch: a person's decision, passed in by the caller. Records the
    /// release durably first; if that fails the switch stays engaged.
    pub async fn release_kill_switch(&mut self, by: &str) -> Result<(), EngineError> {
        self.emit(StoreRecord::RiskEvent(RiskEvent {
            kind: RiskEventKind::KillSwitch,
            detail: serde_json::json!({ "engaged": false, "by": by }),
            ts: self.clock.now(),
        }))
        .await?;
        self.flush().await?;
        self.kill_engaged_here = false;
        self.gate.set_kill_switch(false);
        Ok(())
    }

    /// Records which limits this process trades under (`limits_loaded`): call once at
    /// start-up.
    pub async fn record_limits_loaded(&self) -> Result<(), EngineError> {
        let limits = self.gate.limits();
        self.emit(StoreRecord::RiskEvent(RiskEvent {
            kind: RiskEventKind::LimitsLoaded,
            detail: serde_json::json!({
                "hash": limits.hash().to_string(),
                "limits": limits.to_json(),
            }),
            ts: self.clock.now(),
        }))
        .await?;
        self.flush().await
    }

    /// Feeds the day-trade count with a fill from before this process started (the last
    /// seven days of the `fill` table), so a restart does not reset the count.
    pub fn record_past_fill(&mut self, symbol: Symbol, side: OrderSide, ts: UnixNanos) {
        self.gate.day_trades_mut().record_fill(symbol, side, ts);
    }

    /// Whether the risk scale is zero (flat after a drawdown, or a flattening kill switch)
    /// while a position or an open order remains: the caller should rebalance again. Exits
    /// go through ordinary decisions, so a symbol the strategy cannot decide for (no current
    /// bar, for example) stays held until it can; this is how the caller sees that.
    #[must_use]
    pub fn needs_flatten(&self) -> bool {
        self.gate.risk_scale().is_zero()
            && (self
                .state
                .positions
                .quantities()
                .values()
                .any(|qty| !qty.is_zero())
                || self.state.tracker.unresolved_orders().next().is_some())
    }

    /// Engages the kill switch: sets the flag, records the event, and cancels every open
    /// order. The flag is set whatever happens after that. If recording fails, the cancel is
    /// still sent and the store error is returned; if the cancel fails, it is returned as
    /// `Broker`.
    pub async fn engage_kill_switch(&mut self, by: &str) -> Result<(), EngineError> {
        self.gate.set_kill_switch(true);
        self.kill_engaged_here = true;
        let event = StoreRecord::RiskEvent(RiskEvent {
            kind: RiskEventKind::KillSwitch,
            detail: serde_json::json!({ "engaged": true, "by": by }),
            ts: self.clock.now(),
        });
        // Durably: a restart must find the switch engaged.
        let recorded = match self.emit(event).await {
            Ok(()) => self.flush().await,
            Err(err) => Err(err),
        };
        let cancelled = self
            .gateway
            .broker()
            .cancel_all()
            .await
            .map_err(|err| EngineError::Broker(Box::new(err)));
        if let Err(err) = &cancelled {
            tracing::error!(error = %err, "kill switch engaged but open orders were not cancelled");
        }
        recorded?;
        cancelled
    }

    /// Asks the broker to cancel every unresolved buy order it has acknowledged, except one
    /// whose cancel is already pending. A failed cancel is logged and the rest go on: the
    /// order stays tracked and reconciliation will say what became of it.
    async fn cancel_working_buys(&self) {
        let buys: Vec<ClientOrderId> = self
            .state
            .tracker
            .unresolved_orders()
            .filter(|order| {
                order.intent.side == OrderSide::Buy
                    && order.venue_order_id.is_some()
                    && order.state != OrderState::PendingCancel
            })
            .map(|order| order.intent.client_order_id)
            .collect();
        for id in buys {
            if let Err(err) = self.gateway.cancel(&self.state.tracker, id).await {
                tracing::error!(client_order_id = %id, error = %err, "could not cancel a waiting buy at zero risk scale");
            }
        }
    }

    /// Counts a fill of tracked order `id` in the day-trade tracker. An untracked order is
    /// skipped.
    fn record_day_trade_fill(&mut self, id: ClientOrderId, ts: UnixNanos) {
        if let Some(tracked) = self.state.tracker.get(id) {
            self.gate
                .day_trades_mut()
                .record_fill(tracked.intent.symbol, tracked.intent.side, ts);
        }
    }

    /// Sends one record to the sink. A sink failure means the store is gone.
    async fn flush(&self) -> Result<(), EngineError> {
        self.sink
            .flush()
            .await
            .map_err(|err| EngineError::Store(Box::new(err)))
    }

    async fn emit(&self, record: StoreRecord) -> Result<(), EngineError> {
        self.sink
            .emit(record)
            .await
            .map_err(|err| EngineError::Store(Box::new(err)))
    }

    /// Reconciles orders and positions with the broker and records what changed, without
    /// deciding anything. Fills the pass finds count toward the day-trade limit. Errors with
    /// `NotReconciled` when an order's state is still unknown and `ForeignOrders` when the
    /// broker has orders the engine did not send: the caller must not decide after either.
    pub async fn reconcile(&mut self) -> Result<SyncReport, EngineError> {
        let report = reconcile_and_record(
            self.gateway.broker(),
            &mut self.state,
            &self.sink,
            &self.clock,
            self.config.sync,
        )
        .await?;
        // Fills found while reconciling (missed while disconnected) count toward day trades.
        // Recorded before any early return below: the pass has applied them, so no later pass
        // will report them again.
        for fill in &report.fills {
            self.record_day_trade_fill(fill.client_order_id, fill.ts);
        }
        if !report.is_reconciled() {
            return Err(EngineError::NotReconciled(report.problems.len()));
        }
        // Orders the engine did not send change the position under the decisions: something
        // else is trading the account. Nothing is decided until that is cleared up.
        let foreign = report
            .findings
            .iter()
            .filter(|finding| matches!(finding, Finding::UnknownAtBroker { .. }))
            .count()
            .saturating_add(
                usize::try_from(report.unrepresentable_at_broker).unwrap_or(usize::MAX),
            );
        if foreign > 0 {
            return Err(EngineError::ForeignOrders(foreign));
        }
        Ok(report)
    }

    /// Decides for every symbol and sends the resulting orders. Steps are in
    /// `docs/phases/phase-3.md`, "Rebalance steps".
    pub async fn rebalance(&mut self) -> Result<RebalanceReport, EngineError> {
        self.reconcile().await?;
        let (working, working_sells) = self.working_quantities();
        // The newest bar any symbol has. A symbol whose last bar is older than that has a gap
        // in its data: it is decided without its price features, so the strategy skips it
        // instead of sizing an order from a stale close.
        let newest_bar_ts = self.windows.values().filter_map(|w| w.last_bar_ts).max();

        let account = self
            .gateway
            .broker()
            .account()
            .await
            .map_err(|e| EngineError::Broker(Box::new(e)))?;
        let equity = account.equity.as_decimal();
        // Before any decision, so the `risk_scale` in the features reflects the new mode.
        self.observe_equity(equity).await?;
        // At zero scale nothing more may be bought, so buys still waiting are cancelled. They
        // stay counted as working until the broker confirms, so nothing is sold against them.
        if self.gate.risk_scale().is_zero() {
            self.cancel_working_buys().await;
        }

        let now = self.clock.now();
        let facts = RoundFacts {
            now,
            equity,
            trading_blocked: account.trading_blocked,
            // A bar stamped after `now` has no known age: a clock or the feed is wrong.
            data_age_nanos: newest_bar_ts.and_then(|ts| now.as_u64().checked_sub(ts.as_u64())),
        };

        // Phase A: decide for every symbol before any order is sent, all from one equity.
        let universe = self.config.symbols.len();
        let mut decided: Vec<(DecisionId, Decision, GateInputs)> = Vec::with_capacity(universe);
        let mut out = RebalanceReport {
            decisions: Vec::with_capacity(universe),
            ..RebalanceReport::default()
        };
        for &symbol in &self.config.symbols {
            let working_qty = working.get(&symbol).copied().unwrap_or(Decimal::ZERO);
            let features = self.features_for(symbol, equity, working_qty, newest_bar_ts);
            let held = self.state.positions.get(symbol).qty;
            let selling = working_sells.get(&symbol).copied().unwrap_or(Decimal::ZERO);
            let inputs = self.gate_inputs(symbol, held + working_qty, held - selling);
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
            decided.push((id, decision, inputs));
        }

        // Phase B: sells first, then buys, each in universe order. Each order is checked by the
        // risk gate before it is sent. `running_gross` starts from the held book and grows with
        // every buy that may have gone out, so buys in one rebalance cannot together exceed the
        // exposure cap.
        // A position in a symbol outside the universe is exposure this trader cannot price or
        // reduce, so the total is unknown and buys fail the exposure rule.
        let outside_universe = self
            .state
            .positions
            .quantities()
            .iter()
            .any(|(symbol, qty)| !qty.is_zero() && !self.windows.contains_key(symbol));
        let mut running_gross = if outside_universe {
            None
        } else {
            gross_exposure(&decided)
        };
        for (id, decision, inputs) in &decided {
            if decision.intended_qty < Decimal::ZERO {
                self.send_checked(*id, decision, inputs, &facts, &mut running_gross, &mut out)
                    .await?;
            }
        }
        for (id, decision, inputs) in &decided {
            if decision.intended_qty > Decimal::ZERO {
                self.send_checked(*id, decision, inputs, &facts, &mut running_gross, &mut out)
                    .await?;
            }
        }
        Ok(out)
    }

    /// The risk gate's inputs for one symbol: its position and its window's last close and
    /// daily variance (both absent when the window has no bar).
    fn gate_inputs(
        &self,
        symbol: Symbol,
        position_qty: Decimal,
        sellable_qty: Decimal,
    ) -> GateInputs {
        let stats = self.windows.get(&symbol).and_then(|w| w.stats.stats());
        GateInputs {
            position_qty,
            sellable_qty,
            reference_px: stats.map(|s| s.close),
            var_daily: stats.and_then(|s| s.var_daily),
        }
    }

    /// Checks one non-zero decision with the risk gate, records every check, and sends the
    /// order only if the gate approves it. A buy that may have left a position adds its
    /// notional to `running_gross`.
    async fn send_checked(
        &mut self,
        decision_id: DecisionId,
        decision: &Decision,
        inputs: &GateInputs,
        facts: &RoundFacts,
        running_gross: &mut Option<Decimal>,
        report: &mut RebalanceReport,
    ) -> Result<(), EngineError> {
        let side = if decision.intended_qty < Decimal::ZERO {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        // A sell never asks for more than can be sold now. The decision may count shares
        // still on order (it sizes from held plus working); the order sells what is held and
        // the rest follows when it has filled or been cancelled.
        let wanted = decision.intended_qty.abs();
        let qty = if side == OrderSide::Sell && inputs.sellable_qty > Decimal::ZERO {
            wanted.min(inputs.sellable_qty)
        } else {
            wanted
        };
        let days = self.gate.day_trades();
        let order = OrderCheck {
            side,
            qty,
            reference_px: inputs.reference_px,
            var_daily: inputs.var_daily,
            sellable_qty: inputs.sellable_qty,
            equity: facts.equity,
            gross_exposure: *running_gross,
            data_age_nanos: facts.data_age_nanos,
            account_blocked: facts.trading_blocked,
            would_day_trade: days.would_day_trade(decision.symbol, side, facts.now),
            day_trades_in_window: days.count_in_window(facts.now),
        };
        let verdict = self.gate.check(&order);
        for result in &verdict.checks {
            self.emit(StoreRecord::Risk(RiskCheck {
                decision_id,
                rule: result.rule.to_owned(),
                limit_value: result.limit_value,
                actual_value: result.actual_value,
                passed: result.passed,
                ts: facts.now,
            }))
            .await?;
        }
        if !verdict.approved() {
            report.orders_blocked += 1;
            return Ok(());
        }
        // The checks are on record before the order exists anywhere else.
        self.flush().await?;
        let may_hold_exposure = self.send(decision_id, decision, side, qty, report).await?;
        if may_hold_exposure && side == OrderSide::Buy {
            *running_gross = running_gross
                .zip(inputs.reference_px)
                .and_then(|(gross, px)| gross.checked_add(qty.checked_mul(px)?));
        }
        Ok(())
    }

    /// Signed shares still to fill in the engine's own unresolved orders, per symbol: an
    /// order sent earlier and not yet filled (a market order queued for the next open, an
    /// order whose outcome is unknown). Counting them as held is what stops a second
    /// rebalance before the fill from ordering the same shares again.
    ///
    /// The second map is the unfilled remainder of the sell orders alone, unsigned: shares
    /// already promised to a sell, which another sell must not count on.
    fn working_quantities(&self) -> (BTreeMap<Symbol, Decimal>, BTreeMap<Symbol, Decimal>) {
        let mut working: BTreeMap<Symbol, Decimal> = BTreeMap::new();
        let mut sells: BTreeMap<Symbol, Decimal> = BTreeMap::new();
        for order in self.state.tracker.unresolved_orders() {
            let remaining = order.intent.qty.as_decimal() - order.cum_filled_qty.as_decimal();
            let symbol = order.intent.symbol;
            let signed = match order.intent.side {
                OrderSide::Sell => {
                    *sells.entry(symbol).or_insert(Decimal::ZERO) += remaining;
                    -remaining
                }
                OrderSide::Buy => remaining,
            };
            *working.entry(symbol).or_insert(Decimal::ZERO) += signed;
        }
        (working, sells)
    }

    /// The features one decision is made from, for `symbol`. The symbol's close, history and
    /// momentum are absent until its window has bars, so the strategy skips it. The position
    /// is the shares held plus `working_qty`, the shares its unresolved orders will add. The
    /// price features are also left out when the symbol's last bar is older than
    /// `newest_bar_ts`, the newest bar in the universe.
    fn features_for(
        &self,
        symbol: Symbol,
        equity: Decimal,
        working_qty: Decimal,
        newest_bar_ts: Option<UnixNanos>,
    ) -> Features {
        let mut features = Features::new();
        features.insert(
            feature::POSITION_QTY.to_owned(),
            self.state.positions.get(symbol).qty + working_qty,
        );
        features.insert(feature::EQUITY.to_owned(), equity);
        features.insert(feature::RISK_SCALE.to_owned(), self.gate.risk_scale());
        let n_assets = u64::try_from(self.config.symbols.len()).unwrap_or(u64::MAX);
        features.insert(feature::N_ASSETS.to_owned(), Decimal::from(n_assets));
        let current = self
            .windows
            .get(&symbol)
            .filter(|w| w.last_bar_ts == newest_bar_ts);
        if let Some(stats) = current.and_then(|w| w.stats.stats()) {
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
    /// refusal or an unknown outcome is counted and does not stop the rebalance. Returns
    /// whether the order may have left a position: it was accepted, or its outcome is unknown.
    async fn send(
        &mut self,
        decision_id: DecisionId,
        decision: &Decision,
        side: OrderSide,
        qty: Decimal,
        report: &mut RebalanceReport,
    ) -> Result<bool, EngineError> {
        let qty = Quantity::from_decimal(qty).map_err(|_| {
            EngineError::InvalidConfig("order quantity not representable".to_owned())
        })?;
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
            Ok(submitted) => {
                report.orders_submitted += 1;
                // A fill in the broker's answer never comes again as a new fill on the
                // stream, so it is counted toward day trades here.
                if let Some(fill) = submitted.applied.fill {
                    self.record_day_trade_fill(fill.client_order_id, fill.ts);
                }
                Ok(true)
            }
            Err(EngineError::OrderRejected { .. }) => {
                report.orders_failed += 1;
                Ok(false)
            }
            Err(EngineError::OrderOutcomeUnknown { .. }) => {
                report.orders_failed += 1;
                Ok(true)
            }
            Err(err) => Err(err),
        }
    }

    /// The clock the trader reads.
    #[must_use]
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    /// The risk gate, read-only.
    #[must_use]
    pub const fn risk(&self) -> &RiskGate {
        &self.gate
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
