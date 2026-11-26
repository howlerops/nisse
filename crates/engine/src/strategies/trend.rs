//! Time-series trend following with volatility targeting.
//!
//! Each asset is held only while its own trailing return is positive, and sized so that it
//! contributes an equal share of a portfolio volatility target. The economic rationale and
//! the universe are in `docs/research/trend-v1-rationale.md`, written before any backtest.

use nisse_core::Features;
use rust_decimal::Decimal;
use serde_json::Value;

use crate::{EngineError, Strategy, Verdict, features::WindowSpec};

/// The value of `params["strategy"]` for this strategy.
pub const TREND_VOL_TARGET: &str = "trend_vol_target";

/// Feature names this strategy reads. The engine fills them in; replay reads them back.
pub mod feature {
    /// Latest close.
    pub const CLOSE: &str = "close";
    /// Sessions of history for the symbol.
    pub const HISTORY_DAYS: &str = "history_days";
    /// Trailing return over the lookback, skipping the most recent sessions.
    pub const MOMENTUM: &str = "momentum";
    /// Variance of daily returns over the volatility window.
    pub const VAR_DAILY: &str = "var_daily";
    /// Signed shares held now.
    pub const POSITION_QTY: &str = "position_qty";
    /// Account equity in dollars.
    pub const EQUITY: &str = "equity";
    /// Number of assets in the universe (each gets an equal share of the risk budget).
    pub const N_ASSETS: &str = "n_assets";
}

/// Parameters, as stored in `strategy_version.params` (with `"strategy": "trend_vol_target"`).
/// Decimals are JSON strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrendParams {
    pub lookback_days: usize,
    pub skip_days: usize,
    pub vol_window: usize,
    /// Annualised portfolio volatility target, e.g. `0.09`.
    pub target_vol: Decimal,
    /// Cap on one asset's weight, e.g. `0.25`. The weights are never scaled up to add to one,
    /// so the strategy is unlevered by construction when `max_weight * n_assets <= 1`.
    pub max_weight: Decimal,
    /// A change smaller than this many dollars is not traded.
    pub min_trade_notional: Decimal,
    /// Trading sessions per year used to annualise volatility (252).
    pub sessions_per_year: u32,
}

/// The strategy. Holds its parameters and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrendVolTarget {
    params: TrendParams,
}

#[allow(unused_variables, reason = "stub: Phase 3 task ST1")]
impl TrendVolTarget {
    /// Validates the parameters: `lookback_days > skip_days`, `vol_window >= 2`,
    /// `0 < target_vol <= 1`, `0 < max_weight <= 1`, `min_trade_notional >= 0`,
    /// `sessions_per_year > 0`. Anything else is `InvalidConfig`.
    pub fn new(params: TrendParams) -> Result<Self, EngineError> {
        todo!("Phase 3 task ST1")
    }

    /// Parses stored parameters. Integer fields are JSON numbers; decimal fields are JSON
    /// strings. A missing or mistyped field is `InvalidConfig` naming it.
    pub fn from_params(params: &Value) -> Result<Self, EngineError> {
        todo!("Phase 3 task ST1")
    }

    /// The parameters as stored JSON, including `"strategy"`. `from_params(&to_params())`
    /// gives back an equal strategy.
    #[must_use]
    pub fn to_params(&self) -> Value {
        todo!("Phase 3 task ST1")
    }

    #[must_use]
    pub const fn params(&self) -> &TrendParams {
        &self.params
    }

    /// The rolling-window sizes the engine must maintain for this strategy.
    #[must_use]
    pub const fn window_spec(&self) -> WindowSpec {
        WindowSpec {
            lookback_days: self.params.lookback_days,
            skip_days: self.params.skip_days,
            vol_window: self.params.vol_window,
        }
    }
}

#[allow(unused_variables, reason = "stub: Phase 3 task ST1")]
impl Strategy for TrendVolTarget {
    fn name(&self) -> &'static str {
        TREND_VOL_TARGET
    }

    /// See `docs/phases/phase-3.md`, "Trend strategy rules".
    fn decide(&self, features: &Features, seed: u64) -> Verdict {
        todo!("Phase 3 task ST1")
    }
}
