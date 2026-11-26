//! The gate every order passes before it is sent. Rules in `docs/phases/phase-4.md`,
//! "Gate rules".

use nisse_core::nautilus::OrderSide;
use rust_decimal::{Decimal, MathematicalOps, RoundingStrategy};

use nisse_core::{RiskEvent, RiskEventKind};

use crate::{DayTradeTracker, DrawdownMode, DrawdownState, Limits, RiskError};

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

impl RiskState {
    /// The state after `events` (every `risk_event` row, in id order): the drawdown state
    /// from [`DrawdownState::restore`], and the kill switch from the newest `kill_switch`
    /// event. An event that cannot be read is `InvalidEvent`: the state is unknown and the
    /// caller must not trade.
    pub fn restore(events: &[RiskEvent]) -> Result<Self, RiskError> {
        let mut kill_switch_engaged = false;
        for event in events {
            if event.kind == RiskEventKind::KillSwitch {
                kill_switch_engaged = event
                    .detail
                    .get("engaged")
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| {
                        RiskError::InvalidEvent("kill_switch: engaged must be a boolean".to_owned())
                    })?;
            }
        }
        Ok(Self {
            drawdown: DrawdownState::restore(events)?,
            kill_switch_engaged,
        })
    }
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
    /// Shares a sell may reduce: the filled, broker-confirmed position minus the unfilled
    /// remainder of the engine's own open sell orders. Shares that are only on order (an
    /// unfilled buy) are NOT counted: selling against them could leave the account short.
    pub sellable_qty: Decimal,
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
    #[must_use]
    pub fn check(&self, order: &OrderCheck) -> GateVerdict {
        let limits = &self.limits;
        let state = &self.state;
        let held = order.sellable_qty.max(Decimal::ZERO);
        let mut checks = Vec::with_capacity(rule::ALL.len());
        checks.push(kill_switch(state, limits, order, held));
        checks.push(account_blocked(order));
        checks.push(drawdown_flat(state, limits, order));
        checks.push(stale_data(limits, order));
        checks.push(no_short(order, held));
        checks.push(risk_per_trade(limits, order));
        checks.push(kelly_exposure(state, limits, order));
        checks.push(day_trade_limit(limits, order));
        GateVerdict { checks }
    }
}

/// `actual_value` for a quantity that could not be computed.
const UNKNOWN: Decimal = Decimal::NEGATIVE_ONE;
const SECONDS_PER_HOUR: u64 = 3_600;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

fn is_sell(order: &OrderCheck) -> bool {
    order.side == OrderSide::Sell
}

/// A sell of more than nothing and no more than the shares held.
fn is_reducing_sell(order: &OrderCheck, held: Decimal) -> bool {
    is_sell(order) && order.qty > Decimal::ZERO && order.qty <= held
}

fn round6(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero)
}

fn flag(value: bool) -> Decimal {
    if value { Decimal::ONE } else { Decimal::ZERO }
}

fn result(
    rule: &'static str,
    limit_value: Decimal,
    actual_value: Decimal,
    passed: bool,
) -> RuleResult {
    RuleResult {
        rule,
        limit_value,
        actual_value,
        passed,
    }
}

fn kill_switch(
    state: &RiskState,
    limits: &Limits,
    order: &OrderCheck,
    held: Decimal,
) -> RuleResult {
    let engaged = state.kill_switch_engaged;
    let passed = !engaged || (limits.flatten_on_kill && is_reducing_sell(order, held));
    result(rule::KILL_SWITCH, Decimal::ZERO, flag(engaged), passed)
}

fn account_blocked(order: &OrderCheck) -> RuleResult {
    result(
        rule::ACCOUNT_BLOCKED,
        Decimal::ZERO,
        flag(order.account_blocked),
        !order.account_blocked,
    )
}

fn drawdown_flat(state: &RiskState, limits: &Limits, order: &OrderCheck) -> RuleResult {
    let drawdown = round6(state.drawdown.drawdown(order.equity));
    let passed = is_sell(order) || state.drawdown.mode != DrawdownMode::Flat;
    result(rule::DRAWDOWN_FLAT, limits.drawdown_flat, drawdown, passed)
}

fn stale_data(limits: &Limits, order: &OrderCheck) -> RuleResult {
    let limit_secs = u64::from(limits.max_bar_age_hours).checked_mul(SECONDS_PER_HOUR);
    let limit_nanos = limit_secs.and_then(|secs| secs.checked_mul(NANOS_PER_SECOND));
    let limit = limit_secs.map_or(Decimal::ZERO, Decimal::from);
    let age = order.data_age_nanos;
    let actual = age.map_or(UNKNOWN, |nanos| Decimal::from(nanos / NANOS_PER_SECOND));
    let fresh = matches!((age, limit_nanos), (Some(age), Some(max)) if age <= max);
    result(rule::STALE_DATA, limit, actual, is_sell(order) || fresh)
}

fn no_short(order: &OrderCheck, held: Decimal) -> RuleResult {
    let qty_ok = order.qty > Decimal::ZERO;
    let passed = is_reducing_sell(order, held) || (!is_sell(order) && qty_ok);
    // A malformed quantity, or any sell, shows the quantity itself; a valid buy shows zero.
    let actual = if is_sell(order) || !qty_ok {
        round6(order.qty)
    } else {
        Decimal::ZERO
    };
    result(rule::NO_SHORT, round6(held), actual, passed)
}

/// `max_risk_per_trade × equity`, rounded; zero when the equity is not positive or the
/// product overflows.
fn risk_limit(limits: &Limits, equity: Decimal) -> Decimal {
    if equity <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    limits
        .max_risk_per_trade
        .checked_mul(equity)
        .map_or(Decimal::ZERO, round6)
}

/// `qty × px × risk_sigmas × sqrt(var_daily)` for a buy, or `None` when any input is
/// missing, invalid or overflows.
fn buy_risk(limits: &Limits, order: &OrderCheck) -> Option<Decimal> {
    let px = order.reference_px.filter(|px| *px > Decimal::ZERO)?;
    let var = order.var_daily.filter(|var| *var >= Decimal::ZERO)?;
    if order.equity <= Decimal::ZERO || order.qty <= Decimal::ZERO {
        return None;
    }
    let sigma = var.sqrt()?;
    order
        .qty
        .checked_mul(px)?
        .checked_mul(limits.risk_sigmas)?
        .checked_mul(sigma)
}

fn risk_per_trade(limits: &Limits, order: &OrderCheck) -> RuleResult {
    let limit = risk_limit(limits, order.equity);
    if is_sell(order) {
        return result(rule::RISK_PER_TRADE, limit, Decimal::ZERO, true);
    }
    match buy_risk(limits, order) {
        Some(risk) => {
            let risk = round6(risk);
            result(rule::RISK_PER_TRADE, limit, risk, risk <= limit)
        }
        None => result(rule::RISK_PER_TRADE, limit, UNKNOWN, false),
    }
}

/// `kelly_fraction × haircut_sharpe / portfolio_vol × scale`, or `None` on overflow.
fn kelly_cap(limits: &Limits, scale: Decimal) -> Option<Decimal> {
    limits
        .kelly_fraction
        .checked_mul(limits.haircut_sharpe)?
        .checked_div(limits.portfolio_vol)?
        .checked_mul(scale)
}

/// The gross exposure, when it is known, non-negative and the equity is positive.
fn known_gross(order: &OrderCheck) -> Option<Decimal> {
    let gross = order
        .gross_exposure
        .filter(|gross| *gross >= Decimal::ZERO)?;
    (order.equity > Decimal::ZERO).then_some(gross)
}

/// Exposure after a buy, as a multiple of equity; `None` unless every input is valid.
fn buy_exposure_after(order: &OrderCheck) -> Option<Decimal> {
    let gross = known_gross(order)?;
    // The exposure before the order must be computable too.
    gross.checked_div(order.equity)?;
    let px = order.reference_px.filter(|px| *px > Decimal::ZERO)?;
    if order.qty <= Decimal::ZERO {
        return None;
    }
    gross
        .checked_add(order.qty.checked_mul(px)?)?
        .checked_div(order.equity)
}

fn kelly_exposure(state: &RiskState, limits: &Limits, order: &OrderCheck) -> RuleResult {
    let cap = kelly_cap(limits, state.drawdown.mode.scale());
    let limit = cap.map_or(Decimal::ZERO, round6);
    if is_sell(order) {
        let before = known_gross(order).and_then(|gross| gross.checked_div(order.equity));
        let actual = before.map_or(UNKNOWN, round6);
        return result(rule::KELLY_EXPOSURE, limit, actual, true);
    }
    let after = cap.and(buy_exposure_after(order));
    let actual = after.map_or(UNKNOWN, round6);
    let passed = matches!((after, cap), (Some(after), Some(cap)) if round6(after) <= round6(cap));
    result(rule::KELLY_EXPOSURE, limit, actual, passed)
}

fn day_trade_limit(limits: &Limits, order: &OrderCheck) -> RuleResult {
    let max = limits.max_day_trades;
    let limit = Decimal::from(max);
    if order.would_day_trade {
        let count = order.day_trades_in_window.saturating_add(1);
        result(
            rule::DAY_TRADE_LIMIT,
            limit,
            Decimal::from(count),
            count <= max,
        )
    } else {
        let count = order.day_trades_in_window;
        result(rule::DAY_TRADE_LIMIT, limit, Decimal::from(count), true)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::{Value, json};

    use super::*;

    const DEFAULT: &str = include_str!("../../../config/limits/limits.json");

    fn d(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn limits() -> Limits {
        limits_with(|_| {})
    }

    fn limits_with(edit: impl FnOnce(&mut Value)) -> Limits {
        let mut value: Value = serde_json::from_str(DEFAULT).unwrap();
        edit(&mut value);
        Limits::from_json(&value).unwrap()
    }

    fn gate() -> RiskGate {
        RiskGate::new(limits(), RiskState::default())
    }

    fn gate_in(mode: DrawdownMode) -> RiskGate {
        let mut gate = gate();
        gate.drawdown_mut().mode = mode;
        gate
    }

    fn killed(limits: Limits) -> RiskGate {
        let mut gate = RiskGate::new(limits, RiskState::default());
        gate.set_kill_switch(true);
        gate
    }

    /// A buy that passes every rule with the default limits: 100 shares at 100 on 1_000_000
    /// equity, daily variance 0.0001 (daily vol 0.01), flat, no exposure, fresh data.
    fn buy() -> OrderCheck {
        OrderCheck {
            side: OrderSide::Buy,
            qty: d("100"),
            reference_px: Some(d("100")),
            var_daily: Some(d("0.0001")),
            sellable_qty: Decimal::ZERO,
            equity: d("1000000"),
            gross_exposure: Some(Decimal::ZERO),
            data_age_nanos: Some(0),
            account_blocked: false,
            would_day_trade: false,
            day_trades_in_window: 0,
        }
    }

    /// A sell of `qty` shares from a position of `position`, otherwise as [`buy`].
    fn sell(qty: &str, position: &str) -> OrderCheck {
        OrderCheck {
            side: OrderSide::Sell,
            qty: d(qty),
            sellable_qty: d(position),
            ..buy()
        }
    }

    fn names(verdict: &GateVerdict) -> Vec<&'static str> {
        verdict.checks.iter().map(|check| check.rule).collect()
    }

    fn failed(verdict: &GateVerdict) -> Vec<&'static str> {
        verdict
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.rule)
            .collect()
    }

    fn result_of(verdict: &GateVerdict, name: &str) -> RuleResult {
        *verdict
            .checks
            .iter()
            .find(|check| check.rule == name)
            .unwrap()
    }

    fn assert_result(
        verdict: &GateVerdict,
        name: &str,
        limit: Decimal,
        actual: Decimal,
        passed: bool,
    ) {
        let check = result_of(verdict, name);
        assert_eq!(check.limit_value, limit, "{name} limit");
        assert_eq!(check.actual_value, actual, "{name} actual");
        assert_eq!(check.passed, passed, "{name} passed");
    }

    /// Exactly `name` failed, with these values; every other rule passed.
    fn only_fails(verdict: &GateVerdict, name: &str, limit: Decimal, actual: Decimal) {
        assert!(!verdict.approved());
        assert_eq!(failed(verdict), vec![name]);
        assert_result(verdict, name, limit, actual, false);
    }

    /// A missing or unusable price fails the risk rule and, for a buy, the exposure rule too.
    fn both_sized_rules_fail(verdict: &GateVerdict) {
        assert!(!verdict.approved());
        assert_eq!(
            failed(verdict),
            vec![rule::RISK_PER_TRADE, rule::KELLY_EXPOSURE]
        );
        assert_result(verdict, rule::RISK_PER_TRADE, d("5000"), d("-1"), false);
        assert_result(verdict, rule::KELLY_EXPOSURE, d("1.111111"), d("-1"), false);
    }

    fn check(gate: &RiskGate, order: &OrderCheck) -> GateVerdict {
        gate.check(order)
    }

    #[test]
    fn the_baseline_buy_passes_every_rule() {
        let verdict = check(&gate(), &buy());
        assert!(verdict.approved());
        assert_eq!(names(&verdict), rule::ALL.to_vec());
        assert_eq!(verdict.checks.len(), 8);
        assert_result(&verdict, rule::KILL_SWITCH, d("0"), d("0"), true);
        assert_result(&verdict, rule::ACCOUNT_BLOCKED, d("0"), d("0"), true);
        assert_result(&verdict, rule::DRAWDOWN_FLAT, d("0.15"), d("0"), true);
        assert_result(&verdict, rule::STALE_DATA, d("345600"), d("0"), true);
        assert_result(&verdict, rule::NO_SHORT, d("0"), d("0"), true);
        // risk = 100 × 100 × 2 × 0.01 = 200; limit = 0.005 × 1_000_000
        assert_result(&verdict, rule::RISK_PER_TRADE, d("5000"), d("200"), true);
        // cap = 0.25 × 0.4 / 0.09 = 1.1111111…, rounded; exposure = 10_000 / 1_000_000
        assert_result(
            &verdict,
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("0.01"),
            true,
        );
        assert_result(&verdict, rule::DAY_TRADE_LIMIT, d("3"), d("0"), true);
    }

    // Rule 1: kill switch.

    #[test]
    fn a_kill_switch_fails_a_buy() {
        let verdict = check(&killed(limits()), &buy());
        only_fails(&verdict, rule::KILL_SWITCH, d("0"), d("1"));
    }

    #[test]
    fn a_reducing_sell_fails_the_kill_switch_when_flatten_is_off() {
        let verdict = check(&killed(limits()), &sell("100", "100"));
        only_fails(&verdict, rule::KILL_SWITCH, d("0"), d("1"));
    }

    #[test]
    fn with_flatten_a_reducing_sell_passes_but_a_buy_still_fails() {
        let flatten = || limits_with(|value| value["flatten_on_kill"] = json!(true));

        let sale = check(&killed(flatten()), &sell("100", "100"));
        assert!(sale.approved());
        assert_result(&sale, rule::KILL_SWITCH, d("0"), d("1"), true);

        let purchase = check(&killed(flatten()), &buy());
        only_fails(&purchase, rule::KILL_SWITCH, d("0"), d("1"));
    }

    #[test]
    fn with_flatten_a_sell_larger_than_the_position_fails_rules_1_and_5() {
        let verdict = check(
            &killed(limits_with(|value| value["flatten_on_kill"] = json!(true))),
            &sell("150", "100"),
        );
        assert!(!verdict.approved());
        assert_eq!(failed(&verdict), vec![rule::KILL_SWITCH, rule::NO_SHORT]);
        assert_result(&verdict, rule::KILL_SWITCH, d("0"), d("1"), false);
        assert_result(&verdict, rule::NO_SHORT, d("100"), d("150"), false);
    }

    // Rule 2: account blocked.

    #[test]
    fn a_blocked_account_fails_a_buy() {
        let mut order = buy();
        order.account_blocked = true;
        only_fails(
            &check(&gate(), &order),
            rule::ACCOUNT_BLOCKED,
            d("0"),
            d("1"),
        );
    }

    #[test]
    fn a_blocked_account_fails_a_sell() {
        let mut order = sell("100", "100");
        order.account_blocked = true;
        only_fails(
            &check(&gate(), &order),
            rule::ACCOUNT_BLOCKED,
            d("0"),
            d("1"),
        );
    }

    // Rule 3: drawdown flat.

    #[test]
    fn flat_mode_fails_a_buy_with_the_drawdown_from_the_peak() {
        let mut gate = gate_in(DrawdownMode::Flat);
        gate.drawdown_mut().peak = Some(d("1000000"));
        let mut order = buy();
        order.equity = d("900000");

        let verdict = check(&gate, &order);
        assert!(!verdict.approved());
        // Flat also sets the exposure cap to zero, so the buy fails rule 7 too.
        assert_eq!(
            failed(&verdict),
            vec![rule::DRAWDOWN_FLAT, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::DRAWDOWN_FLAT, d("0.15"), d("0.1"), false);
        // 10_000 / 900_000 against a zero cap.
        assert_result(&verdict, rule::KELLY_EXPOSURE, d("0"), d("0.011111"), false);
    }

    #[test]
    fn flat_mode_passes_a_sell_with_the_drawdown_from_the_peak() {
        let mut gate = gate_in(DrawdownMode::Flat);
        gate.drawdown_mut().peak = Some(d("1000000"));
        let mut order = sell("100", "100");
        order.equity = d("900000");

        let verdict = check(&gate, &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::DRAWDOWN_FLAT, d("0.15"), d("0.1"), true);
    }

    #[test]
    fn flat_mode_latched_fails_a_buy_even_when_equity_has_recovered() {
        let mut gate = gate_in(DrawdownMode::Flat);
        gate.drawdown_mut().peak = Some(d("1000000"));

        let verdict = check(&gate, &buy());
        assert!(!verdict.approved());
        assert_eq!(
            failed(&verdict),
            vec![rule::DRAWDOWN_FLAT, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::DRAWDOWN_FLAT, d("0.15"), d("0"), false);
    }

    // Rule 4: stale data.

    /// 96 hours in nanoseconds: 345_600 seconds.
    const LIMIT_NANOS: u64 = 345_600_000_000_000;

    #[test]
    fn data_exactly_at_the_age_limit_passes() {
        let mut order = buy();
        order.data_age_nanos = Some(LIMIT_NANOS);
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::STALE_DATA, d("345600"), d("345600"), true);
    }

    #[test]
    fn data_one_nanosecond_past_the_age_limit_fails() {
        let mut order = buy();
        order.data_age_nanos = Some(LIMIT_NANOS + 1);
        only_fails(
            &check(&gate(), &order),
            rule::STALE_DATA,
            d("345600"),
            d("345600"),
        );
    }

    #[test]
    fn unknown_data_age_fails_a_buy() {
        let mut order = buy();
        order.data_age_nanos = None;
        only_fails(
            &check(&gate(), &order),
            rule::STALE_DATA,
            d("345600"),
            d("-1"),
        );
    }

    #[test]
    fn unknown_data_age_passes_a_sell() {
        let mut order = sell("100", "100");
        order.data_age_nanos = None;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::STALE_DATA, d("345600"), d("-1"), true);
    }

    // Rule 5: no short.

    #[test]
    fn a_sell_of_exactly_the_position_passes() {
        let verdict = check(&gate(), &sell("100", "100"));
        assert!(verdict.approved());
        assert_result(&verdict, rule::NO_SHORT, d("100"), d("100"), true);
    }

    #[test]
    fn a_sell_one_millionth_past_the_position_fails() {
        only_fails(
            &check(&gate(), &sell("100.000001", "100")),
            rule::NO_SHORT,
            d("100"),
            d("100.000001"),
        );
    }

    #[test]
    fn a_sell_from_a_flat_position_fails() {
        only_fails(
            &check(&gate(), &sell("100", "0")),
            rule::NO_SHORT,
            d("0"),
            d("100"),
        );
    }

    #[test]
    fn a_sell_from_a_short_position_fails() {
        only_fails(
            &check(&gate(), &sell("100", "-50")),
            rule::NO_SHORT,
            d("0"),
            d("100"),
        );
    }

    #[test]
    fn a_zero_quantity_buy_fails_no_short_and_both_sized_rules() {
        let mut order = buy();
        order.qty = Decimal::ZERO;
        let verdict = check(&gate(), &order);
        assert!(!verdict.approved());
        assert_eq!(
            failed(&verdict),
            vec![rule::NO_SHORT, rule::RISK_PER_TRADE, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::NO_SHORT, d("0"), d("0"), false);
        assert_result(&verdict, rule::RISK_PER_TRADE, d("5000"), d("-1"), false);
        assert_result(
            &verdict,
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("-1"),
            false,
        );
    }

    #[test]
    fn a_negative_quantity_buy_fails_no_short_and_both_sized_rules() {
        let mut order = buy();
        order.qty = d("-5");
        let verdict = check(&gate(), &order);
        assert_eq!(
            failed(&verdict),
            vec![rule::NO_SHORT, rule::RISK_PER_TRADE, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::NO_SHORT, d("0"), d("-5"), false);
    }

    #[test]
    fn a_zero_quantity_sell_fails_no_short() {
        only_fails(
            &check(&gate(), &sell("0", "100")),
            rule::NO_SHORT,
            d("100"),
            d("0"),
        );
    }

    #[test]
    fn a_negative_quantity_sell_fails_no_short() {
        only_fails(
            &check(&gate(), &sell("-5", "100")),
            rule::NO_SHORT,
            d("100"),
            d("-5"),
        );
    }

    // Rule 6: risk per trade.

    #[test]
    fn risk_exactly_at_the_limit_passes() {
        // 2500 × 100 × 2 × 0.01 = 5000 = 0.005 × 1_000_000
        let mut order = buy();
        order.qty = d("2500");
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::RISK_PER_TRADE, d("5000"), d("5000"), true);
    }

    #[test]
    fn one_share_more_than_the_risk_limit_fails() {
        // 2501 × 100 × 2 × 0.01 = 5002; the exposure 250_100 / 1_000_000 is still under the cap.
        let mut order = buy();
        order.qty = d("2501");
        only_fails(
            &check(&gate(), &order),
            rule::RISK_PER_TRADE,
            d("5000"),
            d("5002"),
        );
    }

    #[test]
    fn a_missing_price_fails_the_risk_rule() {
        let mut order = buy();
        order.reference_px = None;
        both_sized_rules_fail(&check(&gate(), &order));
    }

    #[test]
    fn a_zero_price_fails_the_risk_rule() {
        let mut order = buy();
        order.reference_px = Some(Decimal::ZERO);
        both_sized_rules_fail(&check(&gate(), &order));
    }

    #[test]
    fn a_negative_price_fails_the_risk_rule() {
        let mut order = buy();
        order.reference_px = Some(d("-100"));
        both_sized_rules_fail(&check(&gate(), &order));
    }

    #[test]
    fn a_missing_variance_fails_the_risk_rule() {
        let mut order = buy();
        order.var_daily = None;
        only_fails(
            &check(&gate(), &order),
            rule::RISK_PER_TRADE,
            d("5000"),
            d("-1"),
        );
    }

    #[test]
    fn a_negative_variance_fails_the_risk_rule() {
        let mut order = buy();
        order.var_daily = Some(d("-0.0001"));
        only_fails(
            &check(&gate(), &order),
            rule::RISK_PER_TRADE,
            d("5000"),
            d("-1"),
        );
    }

    #[test]
    fn zero_variance_gives_zero_risk_and_passes() {
        let mut order = buy();
        order.var_daily = Some(Decimal::ZERO);
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::RISK_PER_TRADE, d("5000"), d("0"), true);
    }

    #[test]
    fn zero_equity_fails_the_risk_rule_with_a_zero_limit() {
        let mut order = buy();
        order.equity = Decimal::ZERO;
        let verdict = check(&gate(), &order);
        assert!(!verdict.approved());
        assert_result(&verdict, rule::RISK_PER_TRADE, d("0"), d("-1"), false);
    }

    #[test]
    fn negative_equity_fails_the_risk_rule_with_a_zero_limit() {
        let mut order = buy();
        order.equity = d("-1000");
        let verdict = check(&gate(), &order);
        assert!(!verdict.approved());
        assert_result(&verdict, rule::RISK_PER_TRADE, d("0"), d("-1"), false);
    }

    #[test]
    fn a_sell_passes_the_risk_rule_whatever_is_missing() {
        let mut order = sell("100", "100");
        order.reference_px = None;
        order.var_daily = None;
        order.equity = Decimal::ZERO;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::RISK_PER_TRADE, d("0"), d("0"), true);
    }

    // Rule 7: Kelly exposure.

    // With equity 1_000_000 and gross 1_000_000, the cap is 1.111111 (rounded). A buy of
    // notional N moves the exposure to (1_000_000 + N) / 1_000_000. N = 111_111 gives
    // 1.111111, which is at the cap; N = 111_112 gives 1.111112, which is over it. The buy
    // is 111_111 shares at 1, so its risk is 111_111 × 1 × 2 × 0.01 = 2_222.22, under 5_000.
    #[test]
    fn exposure_exactly_at_the_cap_passes() {
        let mut order = buy();
        order.qty = d("111111");
        order.reference_px = Some(d("1"));
        order.gross_exposure = Some(d("1000000"));
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(
            &verdict,
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("1.111111"),
            true,
        );
    }

    #[test]
    fn exposure_just_above_the_cap_fails() {
        // Risk 111_112 × 1 × 2 × 0.01 = 2_222.24, under 5_000.
        let mut order = buy();
        order.qty = d("111112");
        order.reference_px = Some(d("1"));
        order.gross_exposure = Some(d("1000000"));
        only_fails(
            &check(&gate(), &order),
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("1.111112"),
        );
    }

    #[test]
    fn halved_mode_halves_the_cap() {
        let verdict = check(&gate_in(DrawdownMode::Halved), &buy());
        assert!(verdict.approved());
        assert_result(
            &verdict,
            rule::KELLY_EXPOSURE,
            d("0.555556"),
            d("0.01"),
            true,
        );
    }

    #[test]
    fn a_buy_over_the_halved_cap_fails_only_in_halved_mode() {
        // (600_000 + 10_000) / 1_000_000 = 0.61: over 0.555556, under 1.111111.
        let mut order = buy();
        order.gross_exposure = Some(d("600000"));
        only_fails(
            &check(&gate_in(DrawdownMode::Halved), &order),
            rule::KELLY_EXPOSURE,
            d("0.555556"),
            d("0.61"),
        );
        let normal = check(&gate(), &order);
        assert!(normal.approved());
    }

    #[test]
    fn flat_mode_sets_the_cap_to_zero() {
        let verdict = check(&gate_in(DrawdownMode::Flat), &buy());
        assert_eq!(
            failed(&verdict),
            vec![rule::DRAWDOWN_FLAT, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::KELLY_EXPOSURE, d("0"), d("0.01"), false);
    }

    #[test]
    fn unknown_gross_exposure_fails_a_buy() {
        let mut order = buy();
        order.gross_exposure = None;
        only_fails(
            &check(&gate(), &order),
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("-1"),
        );
    }

    #[test]
    fn unknown_gross_exposure_passes_a_sell_with_an_unknown_actual() {
        let mut order = sell("100", "100");
        order.gross_exposure = None;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::KELLY_EXPOSURE, d("1.111111"), d("-1"), true);
    }

    #[test]
    fn a_sell_passes_even_when_exposure_is_above_the_cap() {
        let mut order = sell("100", "100");
        order.gross_exposure = Some(d("2000000"));
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::KELLY_EXPOSURE, d("1.111111"), d("2"), true);
    }

    // Rule 8: day trades.

    #[test]
    fn a_day_trade_that_keeps_the_count_within_the_limit_passes() {
        let mut order = buy();
        order.would_day_trade = true;
        order.day_trades_in_window = 2;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::DAY_TRADE_LIMIT, d("3"), d("3"), true);
    }

    #[test]
    fn a_day_trade_over_the_limit_fails() {
        let mut order = buy();
        order.would_day_trade = true;
        order.day_trades_in_window = 3;
        only_fails(
            &check(&gate(), &order),
            rule::DAY_TRADE_LIMIT,
            d("3"),
            d("4"),
        );
    }

    #[test]
    fn an_order_that_is_not_a_day_trade_passes_whatever_the_count() {
        let mut order = buy();
        order.day_trades_in_window = 5;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_result(&verdict, rule::DAY_TRADE_LIMIT, d("3"), d("5"), true);
    }

    #[test]
    fn with_no_day_trades_allowed_the_first_day_trade_fails() {
        let gate = RiskGate::new(
            limits_with(|value| value["max_day_trades"] = json!(0)),
            RiskState::default(),
        );
        let mut order = buy();
        order.would_day_trade = true;
        only_fails(&check(&gate, &order), rule::DAY_TRADE_LIMIT, d("0"), d("1"));
    }

    #[test]
    fn the_day_trade_count_saturates_instead_of_wrapping() {
        let mut order = buy();
        order.would_day_trade = true;
        order.day_trades_in_window = u32::MAX;
        only_fails(
            &check(&gate(), &order),
            rule::DAY_TRADE_LIMIT,
            d("3"),
            d("4294967295"),
        );
    }

    // Whole-verdict behaviour.

    #[test]
    fn several_failing_rules_are_all_reported() {
        let gate = killed(limits());
        let mut order = buy();
        order.data_age_nanos = None;
        order.account_blocked = true;
        let verdict = check(&gate, &order);
        assert!(!verdict.approved());
        assert_eq!(verdict.checks.len(), 8);
        assert_eq!(
            failed(&verdict),
            vec![rule::KILL_SWITCH, rule::ACCOUNT_BLOCKED, rule::STALE_DATA]
        );
    }

    #[test]
    fn the_verdict_has_every_rule_in_order_when_it_passes_and_when_it_fails() {
        let passing = check(&gate(), &buy());
        assert_eq!(names(&passing), rule::ALL.to_vec());

        let mut order = buy();
        order.account_blocked = true;
        order.qty = Decimal::ZERO;
        let failing = check(&killed(limits()), &order);
        assert_eq!(names(&failing), rule::ALL.to_vec());
        assert_eq!(failing.checks.len(), 8);
    }

    #[test]
    fn overflowing_quantity_fails_the_sized_rules_without_panicking() {
        let mut order = buy();
        order.qty = Decimal::MAX;
        let verdict = check(&gate(), &order);
        assert!(!verdict.approved());
        assert_eq!(
            failed(&verdict),
            vec![rule::RISK_PER_TRADE, rule::KELLY_EXPOSURE]
        );
        assert_result(&verdict, rule::RISK_PER_TRADE, d("5000"), d("-1"), false);
        assert_result(
            &verdict,
            rule::KELLY_EXPOSURE,
            d("1.111111"),
            d("-1"),
            false,
        );
    }

    #[test]
    fn an_overflowing_quantity_sell_is_still_evaluated_without_panicking() {
        let mut order = sell("0", "0");
        order.qty = Decimal::MAX;
        order.sellable_qty = Decimal::MAX;
        let verdict = check(&gate(), &order);
        assert!(verdict.approved());
        assert_eq!(verdict.checks.len(), 8);
    }

    #[test]
    fn an_enormous_equity_computes_the_limit_without_panicking() {
        // 0.005 × Decimal::MAX fits, so the limit is the rounded product and a 200 risk passes.
        let mut order = buy();
        order.equity = Decimal::MAX;
        let verdict = check(&gate(), &order);
        let limit = round6(d("0.005").checked_mul(Decimal::MAX).unwrap());
        assert_result(&verdict, rule::RISK_PER_TRADE, limit, d("200"), true);
    }

    #[test]
    fn an_empty_verdict_is_never_approved() {
        assert!(!GateVerdict { checks: vec![] }.approved());
    }

    // risk_scale.

    #[test]
    fn risk_scale_follows_the_drawdown_mode() {
        assert_eq!(gate_in(DrawdownMode::Normal).risk_scale(), d("1"));
        assert_eq!(gate_in(DrawdownMode::Halved).risk_scale(), d("0.5"));
        assert_eq!(gate_in(DrawdownMode::Flat).risk_scale(), d("0"));
    }

    #[test]
    fn risk_scale_is_zero_when_killed_with_flatten() {
        let flatten = limits_with(|value| value["flatten_on_kill"] = json!(true));
        for mode in [
            DrawdownMode::Normal,
            DrawdownMode::Halved,
            DrawdownMode::Flat,
        ] {
            let mut gate = RiskGate::new(flatten.clone(), RiskState::default());
            gate.drawdown_mut().mode = mode;
            gate.set_kill_switch(true);
            assert_eq!(gate.risk_scale(), d("0"));
        }
    }

    #[test]
    fn risk_scale_is_the_mode_scale_when_killed_without_flatten() {
        let mut normal = gate();
        normal.set_kill_switch(true);
        assert_eq!(normal.risk_scale(), d("1"));

        let mut halved = gate_in(DrawdownMode::Halved);
        halved.set_kill_switch(true);
        assert_eq!(halved.risk_scale(), d("0.5"));
    }

    fn event(kind: RiskEventKind, detail: serde_json::Value) -> RiskEvent {
        RiskEvent {
            kind,
            detail,
            ts: nisse_core::nautilus::UnixNanos::from(1_u64),
        }
    }

    #[test]
    fn risk_state_restores_the_newest_kill_switch_and_the_drawdown() {
        use serde_json::json;
        assert_eq!(RiskState::restore(&[]).unwrap(), RiskState::default());
        let events = [
            event(RiskEventKind::EquityPeak, json!({"peak": "100"})),
            event(
                RiskEventKind::KillSwitch,
                json!({"engaged": true, "by": "a"}),
            ),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "halved", "equity": "90"}),
            ),
            event(RiskEventKind::LimitsLoaded, json!({"hash": "00"})),
        ];
        let state = RiskState::restore(&events).unwrap();
        assert!(state.kill_switch_engaged);
        assert_eq!(state.drawdown.mode, DrawdownMode::Halved);
        assert_eq!(state.drawdown.peak, Some(Decimal::from(100)));

        let mut released = events.to_vec();
        released.push(event(RiskEventKind::KillSwitch, json!({"engaged": false})));
        assert!(!RiskState::restore(&released).unwrap().kill_switch_engaged);
    }

    #[test]
    fn risk_state_restore_refuses_an_unreadable_event() {
        use serde_json::json;
        for detail in [json!({}), json!({"engaged": "yes"})] {
            let events = [event(RiskEventKind::KillSwitch, detail)];
            assert!(matches!(
                RiskState::restore(&events),
                Err(RiskError::InvalidEvent(_))
            ));
        }
        let bad_peak = [event(RiskEventKind::EquityPeak, json!({"peak": 1}))];
        assert!(matches!(
            RiskState::restore(&bad_peak),
            Err(RiskError::InvalidEvent(_))
        ));
    }
}
