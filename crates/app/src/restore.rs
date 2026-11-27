//! What every command does first: rebuild the trader's state from the store and the bar files.
//! The engine process: short-lived commands run from cron (`nisse-engine <command>`), each of
//! which rebuilds its state from the store and the bar files, does one thing and exits.
//!
//! This is the only crate that depends on both the engine and the broker adapter. Design and
//! the exact steps of every command are in `docs/phases/phase-6.md`. No LLM is called
//! anywhere here, and the broker credentials are read from the environment by the binary
//! only.

use nisse_bars::BarReader;
use nisse_core::nautilus::{Symbol, UnixNanos};
use nisse_core::{
    Broker, Clock, StrategyVersion, StrategyVersionId, bars_snapshot_id, config_hash,
};
use nisse_engine::{
    GIT_SHA, OrderBookState, OrderGateway, Strategy, SyncOptions, Trader, TraderConfig, strategies,
};
use nisse_risk::{Limits, RiskState};
use nisse_store::{
    Store, StoreWriter,
    tables::scoring::{self, ENGINE_LOCK},
};

use crate::{AppError, config::EngineConfig};

/// The trader type the engine process runs: the strategy named in the config, one store
/// connection as both decision log and intent log, the writer queue as record sink.
pub type EngineTrader<'a, B, C> =
    Trader<Box<dyn Strategy>, B, &'a Store, &'a Store, StoreWriter, C>;

/// How far back fills are read for the day-trade count.
pub const DAY_TRADE_LOOKBACK_NANOS: u64 = 7 * 24 * 3_600 * 1_000_000_000;

/// A trader with its state put back from the store and the bar files.
pub struct Restored<'a, B, C> {
    pub trader: EngineTrader<'a, B, C>,
    pub strategy_version_id: StrategyVersionId,
    /// Bars fed to the trader (0 when there is no bar file yet).
    pub bars_fed: usize,
    /// The time of the newest bar fed.
    pub newest_bar_ts: Option<UnixNanos>,
}

/// Takes the engine lock on the store's connection. `AppError::Busy` when another engine
/// process holds it. The lock is released when the connection closes.
pub async fn take_engine_lock(store: &Store) -> Result<(), AppError> {
    if scoring::try_advisory_lock(store.client(), ENGINE_LOCK).await? {
        Ok(())
    } else {
        Err(AppError::Busy)
    }
}

/// Rebuilds the trader. Steps 2 to 9 of docs/phases/phase-6.md "restore" (the lock is the
/// caller's). Nothing here talks to the broker.
///
/// The bar read is a blocking DuckDB query. That is accepted: the engine is a short-lived
/// process and this runs once per invocation, before any decision is made.
pub async fn restore<'a, B: Broker, C: Clock>(
    config: &EngineConfig,
    store: &'a Store,
    sink: StoreWriter,
    broker: B,
    clock: C,
) -> Result<Restored<'a, B, C>, AppError> {
    let now = clock.now();

    // 1. Limits. The hash is taken now, before the limits move into the trader.
    let limits = Limits::load(&config.limits_path)?;
    let limits_hash = limits.hash().to_string();

    // 2. Strategy and its version.
    let built = strategies::build(&config.strategy)?;
    let strategy_version_id = store
        .ensure_strategy_version(&StrategyVersion {
            git_sha: GIT_SHA.to_owned(),
            config_hash: config_hash(&config.strategy),
            params: config.strategy.clone(),
            parent_id: None,
            ts: now,
        })
        .await?;

    // 3. Risk state. An unreadable event is returned: the engine does not trade on it.
    let risk = RiskState::restore(&store.load_risk_events().await?)?;

    // 4. Orders that may still be live.
    let sync = SyncOptions::default();
    let recheck_since = UnixNanos::from(
        now.as_u64()
            .saturating_sub(sync.never_arrived_recheck_nanos),
    );
    let orders = store.load_live_orders(Some(recheck_since)).await?;
    let mut state = OrderBookState::default();
    state.tracker.restore_all(&orders)?;

    // 5. Positions: the newest row per symbol.
    for row in store.load_positions().await? {
        state
            .positions
            .restore(row.symbol, row.qty_after, row.avg_cost);
    }

    // 6. Bars: the newest file, the universe only (not the benchmark), up to now.
    let bars = if config.bars_dir.is_dir() {
        let reader = BarReader::open(&config.bars_dir)?;
        let symbols: Vec<&str> = config.symbols.iter().map(Symbol::as_str).collect();
        reader.read(&symbols, UnixNanos::from(0_u64), now)?
    } else {
        Vec::new()
    };
    let data_snapshot_id = bars_snapshot_id(&bars);
    let bars_fed = bars.len();
    let newest_bar_ts = bars.iter().map(|bar| bar.ts_event).max();

    // 7. The trader.
    let mut trader = Trader::new(
        built.strategy,
        OrderGateway::new(broker, store),
        store,
        sink,
        clock,
        TraderConfig {
            strategy_version_id,
            symbols: config.symbols.clone(),
            window: built.window,
            data_snapshot_id,
            seed: config.seed,
            sync,
            limits,
            risk,
        },
        state,
    )?;

    // 8. Day trades: the fills of the last seven days, so a restart does not reset the count.
    let since = UnixNanos::from(now.as_u64().saturating_sub(DAY_TRADE_LOOKBACK_NANOS));
    for fill in store.recent_fill_sides(since).await? {
        trader.record_past_fill(fill.symbol, fill.side, fill.ts);
    }

    // 9. Feed every bar, in the order the reader returned them (time, then symbol).
    for bar in &bars {
        trader.on_bar(bar)?;
    }

    // 10. Kill switch. An unreadable switch engages it; the trader handles that.
    trader.refresh_kill_switch(store).await;

    // 11. Record which limits this run trades under, unless the newest record already says so.
    if store.latest_limits_hash().await?.as_deref() != Some(limits_hash.as_str()) {
        trader.record_limits_loaded().await?;
    }

    Ok(Restored {
        trader,
        strategy_version_id,
        bars_fed,
        newest_bar_ts,
    })
}
