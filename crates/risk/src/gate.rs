//! The gate every order passes before it is sent. Rules in `docs/phases/phase-4.md`,
//! "Gate rules".

use nisse_core::nautilus::OrderSide;
use rust_decimal::Decimal;

use crate::{DayTradeTracker, DrawdownState, Limits};

/// The rule names stored in `risk_check.rule`, in evaluation order.
pub mod rule {
    pub const KILL_SWITCH: &str = "kill_switch";
    pub const ACCOUNT_BLOCKED: &str = "account_blocked";
    pub const DRAWDOWN_FLAT: &str = "drawdown_flat";
    pub const STALE_DATA: &str = "stale_data";
    pub const NO_SHORT: &str = "no_short";
    pub const RISK_PER_TRADE: &str = "risk_per_trade";
    pub const KELLY_EXPOSURE: &str = "kelly_exposure";
    pub const DAY_TRADE_LIMIT: &str = "day_trade_limit";

    /// Every rule, in the order the gate evaluates and reports them.
    pub const ALL: [&str; 8] = [
        KILL_SWITCH,
        ACCOUNT_BLOCKED,
        DRAWDOWN_FLAT,
        STALE_DATA,
        NO_SHORT,
        RISK_PER_TRADE,
        KELLY_EXPOSURE,
        DAY_TRADE_LIMIT,
    ];
}

/// The risk state that outlives a process: restored from `risk_event` rows at start-up.
/// `Default` is a fresh account: no peak, normal mode, kill switch off.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RiskState {
    pub drawdown: DrawdownState,
    pub kill_switch_engaged: bool,
}

/// What the gate is told about one order. Everything is as of just before the order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCheck {
    pub side: OrderSide,
    /// Shares, positive.
    pub qty: Decimal,
    /// The symbol's last close. `None` when it has no bar.
    pub reference_px: Option<Decimal>,
    /// Variance of the symbol's daily returns. `None` when there is not enough history.
    pub var_daily: Option<Decimal>,
    /// Signed shares held plus the unfilled remainder of the engine's own open orders.
    pub position_qty: Decimal,
    pub equity: Decimal,
    /// `Σ |held + working| × last close` over the universe. `None` when a held symbol has no
    /// close, so the figure is unknown.
    pub gross_exposure: Option<Decimal>,
    /// Age of the newest bar in the universe. `None` when there is no bar at all.
    pub data_age_nanos: Option<u64>,
    /// The broker reports the account blocked from trading.
    pub account_blocked: bool,
    /// From [`DayTradeTracker::would_day_trade`].
    pub would_day_trade: bool,
    /// From [`DayTradeTracker::count_in_window`].
    pub day_trades_in_window: u32,
}

/// One rule's outcome: one `risk_check` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleResult {
    pub rule: &'static str,
    pub limit_value: Decimal,
    pub actual_value: Decimal,
    pub passed: bool,
}

/// Every rule's outcome for one order, in [`rule::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    pub checks: Vec<RuleResult>,
}

impl GateVerdict {
    /// The order may be sent: every rule passed (and there is at least one result, so an
    /// empty verdict never approves anything).
    #[must_use]
    pub fn approved(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.passed)
    }
}

/// The limits and the state they are applied to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskGate {
    limits: Limits,
    state: RiskState,
    day_trades: DayTradeTracker,
}

impl RiskGate {
    #[must_use]
    pub fn new(limits: Limits, state: RiskState) -> Self {
        Self {
            limits,
            state,
            day_trades: DayTradeTracker::new(),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    #[must_use]
    pub const fn state(&self) -> &RiskState {
        &self.state
    }

    /// The drawdown state, for `observe` and `reset`.
    pub const fn drawdown_mut(&mut self) -> &mut DrawdownState {
        &mut self.state.drawdown
    }

    #[must_use]
    pub const fn day_trades(&self) -> &DayTradeTracker {
        &self.day_trades
    }

    pub const fn day_trades_mut(&mut self) -> &mut DayTradeTracker {
        &mut self.day_trades
    }

    /// Sets the kill switch. Engaging is always allowed; the caller decides who may release.
    pub const fn set_kill_switch(&mut self, engaged: bool) {
        self.state.kill_switch_engaged = engaged;
    }

    /// The factor the strategy multiplies its target weights by, and the gate its exposure
    /// cap: the drawdown mode's scale, or zero while the kill switch is engaged with
    /// `flatten_on_kill`.
    #[must_use]
    pub fn risk_scale(&self) -> Decimal {
        if self.state.kill_switch_engaged && self.limits.flatten_on_kill {
            return Decimal::ZERO;
        }
        self.state.drawdown.mode.scale()
    }

    /// Evaluates every rule for one order. Never short-circuits: the verdict has one result
    /// per rule in [`rule::ALL`] order, whatever failed. Anything that cannot be computed
    /// (a missing price, an overflow, a non-positive equity) fails its rule with
    /// `actual_value` -1. Amounts are rounded half-up to 6 decimal places.
    #[allow(unused_variables, reason = "stub: Phase 4 task R4")]
    #[must_use]
    pub fn check(&self, order: &OrderCheck) -> GateVerdict {
        todo!("Phase 4 task R4")
    }
}
