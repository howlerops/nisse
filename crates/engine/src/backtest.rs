//! Running a strategy over historical bars through the same [`Trader`] that trades live.

use nisse_core::{Decision, Fill, MarketCalendar, StrategyVersionId, nautilus::Bar};
use rust_decimal::Decimal;

use crate::{CostModel, EngineError, Strategy, features::WindowSpec, metrics::EquityPoint};

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
#[allow(unused_variables, reason = "stub: Phase 3 task BT1")]
pub async fn run_backtest<S: Strategy>(
    strategy: S,
    bars: &[Bar],
    calendar: &MarketCalendar,
    config: BacktestConfig,
) -> Result<BacktestRun, EngineError> {
    todo!("Phase 3 task BT1")
}
