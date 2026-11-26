//! Running a strategy over historical bars through the same [`Trader`] that trades live.

use std::collections::BTreeSet;

use nisse_core::{
    Broker, Decision, Fill, ManualClock, MarketCalendar, StoreRecord, StrategyVersionId,
    nautilus::{Bar, Symbol, UnixNanos},
};
use rust_decimal::Decimal;

use crate::{
    CostModel, EngineError, MemoryStore, OrderBookState, OrderGateway, SimBroker, Strategy,
    SyncOptions, Trader, TraderConfig, features::WindowSpec, metrics::EquityPoint,
};

/// What a backtest is given besides the strategy and the bars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacktestConfig {
    pub strategy_version_id: StrategyVersionId,
    pub window: WindowSpec,
    pub starting_cash: Decimal,
    pub costs: CostModel,
    pub data_snapshot_id: String,
    pub seed: u64,
}

/// Everything a backtest produced.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestRun {
    /// Equity at each session's close, in time order.
    pub equity_curve: Vec<EquityPoint>,
    /// Every decision, in the order it was made.
    pub decisions: Vec<Decision>,
    /// Every fill, in the order it happened.
    pub fills: Vec<Fill>,
    pub fees_paid: Decimal,
}

/// Runs `strategy` over `bars` (sorted by time, then symbol) with month-end rebalancing.
/// Steps are in `docs/phases/phase-3.md`, "Backtest loop". The universe is the set of symbols
/// in `bars`. Deterministic: the same inputs give the same `BacktestRun`.
pub async fn run_backtest<S: Strategy>(
    strategy: S,
    bars: &[Bar],
    calendar: &MarketCalendar,
    config: BacktestConfig,
) -> Result<BacktestRun, EngineError> {
    let first = bars
        .first()
        .ok_or_else(|| EngineError::InvalidConfig("no bars".to_owned()))?;
    if !is_strictly_ordered(bars) {
        return Err(EngineError::InvalidBar(
            "bars must be sorted by time then symbol",
        ));
    }
    let symbols: Vec<Symbol> = bars
        .iter()
        .map(symbol_of)
        .collect::<BTreeSet<Symbol>>()
        .into_iter()
        .collect();

    let store = MemoryStore::new();
    let clock = ManualClock::new(first.ts_event);
    let broker = SimBroker::new(config.starting_cash, config.costs);
    let gateway = OrderGateway::new(broker, &store);
    let mut trader = Trader::new(
        strategy,
        gateway,
        &store,
        &store,
        &clock,
        TraderConfig {
            strategy_version_id: config.strategy_version_id,
            symbols,
            window: config.window,
            data_snapshot_id: config.data_snapshot_id,
            seed: config.seed,
            sync: SyncOptions {
                never_arrived_grace_nanos: 0,
                never_arrived_recheck_nanos: 0,
            },
        },
        OrderBookState::default(),
    )?;

    let session_count = bars.chunk_by(same_session).count();
    let mut equity_curve: Vec<EquityPoint> = Vec::with_capacity(session_count);
    for session in bars.chunk_by(same_session) {
        let Some(session_ts) = session.first().map(|bar| bar.ts_event) else {
            continue;
        };
        clock.set(session_ts);
        trader.broker().on_session(session)?;
        for update in trader.broker().drain_updates() {
            trader.on_order_update(&update).await?;
        }
        for bar in session {
            trader.on_bar(bar)?;
        }
        let equity = trader
            .broker()
            .account()
            .await
            .map_err(|e| EngineError::Broker(Box::new(e)))?
            .equity
            .as_decimal();
        equity_curve.push((session_ts, equity));

        // A bar's ts is the session close; the session that contains it is the one that
        // closed it, which is the one whose last instant is just before that close.
        let inside = session_ts
            .as_u64()
            .checked_sub(1)
            .and_then(|ts| calendar.session_at(UnixNanos::from(ts)));
        let Some(closed_session) = inside else {
            return Err(EngineError::InvalidBar("bar outside the calendar"));
        };
        if calendar.is_last_session_of_month(closed_session.date) == Some(true) {
            trader.rebalance().await?;
        }
    }

    let fills: Vec<Fill> = store
        .records()
        .into_iter()
        .filter_map(|record| match record {
            StoreRecord::Order { fill, .. } => fill,
            _ => None,
        })
        .collect();

    Ok(BacktestRun {
        equity_curve,
        decisions: store.decisions(),
        fills,
        fees_paid: trader.broker().fees_paid(),
    })
}

fn symbol_of(bar: &Bar) -> Symbol {
    bar.bar_type.instrument_id().symbol
}

/// Whether two bars belong to the same session (they share a close time).
fn same_session(a: &Bar, b: &Bar) -> bool {
    a.ts_event == b.ts_event
}

/// Whether every adjacent pair is strictly increasing by `(ts_event, symbol)`. Strictly
/// increasing means both sorted and free of duplicates.
fn is_strictly_ordered(bars: &[Bar]) -> bool {
    bars.windows(2)
        .all(|pair| matches!(pair, [a, b] if in_order(a, b)))
}

fn in_order(a: &Bar, b: &Bar) -> bool {
    let (sa, sb) = (symbol_of(a), symbol_of(b));
    (a.ts_event.as_u64(), sa.as_str()) < (b.ts_event.as_u64(), sb.as_str())
}
