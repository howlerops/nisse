//! Time-series trend following with volatility targeting.
//!
//! Each asset is held only while its own trailing return is positive, and sized so that it
//! contributes an equal share of a portfolio volatility target. The economic rationale and
//! the universe are in `docs/research/trend-v1-rationale.md`, written before any backtest.

use std::str::FromStr;

use nisse_core::{Action, Features};
use rust_decimal::{
    Decimal, RoundingStrategy,
    prelude::{FromPrimitive, ToPrimitive},
};
use serde_json::{Value, json};

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
    /// Signed shares held now, plus the shares still to fill in the engine's own open orders.
    pub const POSITION_QTY: &str = "position_qty";
    /// Account equity in dollars.
    pub const EQUITY: &str = "equity";
    /// Number of assets in the universe (each gets an equal share of the risk budget).
    pub const N_ASSETS: &str = "n_assets";
    /// The risk gate's exposure factor: 1 normally, 0.5 after a 10% drawdown, 0 when flat or while the kill switch flattens. The strategy multiplies its target weight by it.
    pub const RISK_SCALE: &str = "risk_scale";
}

/// Reason codes. Stable strings: they are stored in `decision.reason_code` and compared on
/// replay.
const MISSING_FEATURE: &str = "missing_feature";
const BAD_INPUT: &str = "bad_input";
const INSUFFICIENT_HISTORY: &str = "insufficient_history";
const ZERO_VOL: &str = "zero_vol";
const WITHIN_BAND: &str = "within_band";
const TREND_UP: &str = "trend_up";
const TREND_DOWN: &str = "trend_down";

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

impl TrendVolTarget {
    /// Validates the parameters: `lookback_days > skip_days`, `vol_window >= 2`,
    /// `0 < target_vol <= 1`, `0 < max_weight <= 1`, `min_trade_notional >= 0`,
    /// `sessions_per_year > 0`. Anything else is `InvalidConfig`.
    pub fn new(params: TrendParams) -> Result<Self, EngineError> {
        if params.lookback_days <= params.skip_days {
            return Err(field_error(
                "lookback_days",
                "must be greater than skip_days",
            ));
        }
        if params.vol_window < 2 {
            return Err(field_error("vol_window", "must be at least 2"));
        }
        if params.target_vol <= Decimal::ZERO || params.target_vol > Decimal::ONE {
            return Err(field_error(
                "target_vol",
                "must be greater than 0 and at most 1",
            ));
        }
        if params.max_weight <= Decimal::ZERO || params.max_weight > Decimal::ONE {
            return Err(field_error(
                "max_weight",
                "must be greater than 0 and at most 1",
            ));
        }
        if params.min_trade_notional < Decimal::ZERO {
            return Err(field_error("min_trade_notional", "must not be negative"));
        }
        if params.sessions_per_year == 0 {
            return Err(field_error("sessions_per_year", "must be at least 1"));
        }
        Ok(Self { params })
    }

    /// Parses stored parameters. Integer fields are JSON numbers; decimal fields are JSON
    /// strings. A missing or mistyped field is `InvalidConfig` naming it.
    pub fn from_params(params: &Value) -> Result<Self, EngineError> {
        if params.get("strategy").and_then(Value::as_str) != Some(TREND_VOL_TARGET) {
            return Err(field_error("strategy", "must be \"trend_vol_target\""));
        }
        Self::new(TrendParams {
            lookback_days: uint_field(params, "lookback_days")?,
            skip_days: uint_field(params, "skip_days")?,
            vol_window: uint_field(params, "vol_window")?,
            target_vol: decimal_field(params, "target_vol")?,
            max_weight: decimal_field(params, "max_weight")?,
            min_trade_notional: decimal_field(params, "min_trade_notional")?,
            sessions_per_year: uint_field(params, "sessions_per_year")?,
        })
    }

    /// The parameters as stored JSON, including `"strategy"`. `from_params(&to_params())`
    /// gives back an equal strategy.
    #[must_use]
    pub fn to_params(&self) -> Value {
        let p = &self.params;
        json!({
            "strategy": TREND_VOL_TARGET,
            "lookback_days": p.lookback_days,
            "skip_days": p.skip_days,
            "vol_window": p.vol_window,
            "target_vol": p.target_vol.normalize().to_string(),
            "max_weight": p.max_weight.normalize().to_string(),
            "min_trade_notional": p.min_trade_notional.normalize().to_string(),
            "sessions_per_year": p.sessions_per_year,
        })
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

    /// Rule 3, long branch: the shares that give this asset its share of the volatility
    /// target, scaled by the risk gate's `risk_scale`. `Err` carries the reason code of the
    /// skip.
    fn target_shares(
        &self,
        asset_vol: f64,
        n_assets: Decimal,
        equity: Decimal,
        close: Decimal,
        risk_scale: Decimal,
    ) -> Result<Decimal, &'static str> {
        let (Some(target_vol), Some(max_weight), Some(n)) = (
            self.params.target_vol.to_f64(),
            self.params.max_weight.to_f64(),
            n_assets.to_f64(),
        ) else {
            return Err(BAD_INPUT);
        };
        let weight_f = f64::min(max_weight, (target_vol / asset_vol) / n);
        let weight = Decimal::from_f64(weight_f)
            .ok_or(ZERO_VOL)?
            .round_dp_with_strategy(8, RoundingStrategy::MidpointNearestEven);
        // Applied after the per-asset cap, in the same `Decimal` as the weight.
        let weight = weight.checked_mul(risk_scale).ok_or(BAD_INPUT)?;
        let shares = weight
            .checked_mul(equity)
            .and_then(|x| x.checked_div(close))
            .ok_or(BAD_INPUT)?;
        Ok(shares.round_dp_with_strategy(6, RoundingStrategy::ToZero))
    }
}

impl Strategy for TrendVolTarget {
    fn name(&self) -> &'static str {
        TREND_VOL_TARGET
    }

    /// See `docs/phases/phase-3.md`, "Trend strategy rules".
    fn decide(&self, features: &Features, _seed: u64) -> Verdict {
        // Required features: all present, or `missing_feature`.
        let (Some(close), Some(_), Some(position), Some(equity), Some(n_assets), Some(risk_scale)) = (
            get(features, feature::CLOSE),
            get(features, feature::HISTORY_DAYS),
            get(features, feature::POSITION_QTY),
            get(features, feature::EQUITY),
            get(features, feature::N_ASSETS),
            get(features, feature::RISK_SCALE),
        ) else {
            return skip(MISSING_FEATURE, None);
        };

        // Rule 1.
        if close <= Decimal::ZERO
            || equity <= Decimal::ZERO
            || n_assets < Decimal::ONE
            || !n_assets.fract().is_zero()
            || risk_scale < Decimal::ZERO
            || risk_scale > Decimal::ONE
        {
            return skip(BAD_INPUT, None);
        }

        // Rule 2.
        let (Some(momentum), Some(var_daily)) = (
            get(features, feature::MOMENTUM),
            get(features, feature::VAR_DAILY),
        ) else {
            return skip(INSUFFICIENT_HISTORY, None);
        };

        // Rule 3: annualised volatility, computed on every path past rule 2. `None` when it
        // is zero, negative or not finite.
        let annual_var = var_daily.checked_mul(Decimal::from(self.params.sessions_per_year));
        let asset_vol = annual_var
            .and_then(|v| v.to_f64())
            .map(f64::sqrt)
            .filter(|v| v.is_finite() && *v > 0.0);

        let (target, trend) = if momentum <= Decimal::ZERO {
            (Decimal::ZERO, TREND_DOWN)
        } else {
            let Some(vol) = asset_vol else {
                let reason = if annual_var.is_none() {
                    BAD_INPUT
                } else {
                    ZERO_VOL
                };
                return skip(reason, None);
            };
            match self.target_shares(vol, n_assets, equity, close, risk_scale) {
                Ok(shares) => (shares, TREND_UP),
                Err(reason) => return skip(reason, asset_vol),
            }
        };

        // Rule 4.
        let Some(delta) = target.checked_sub(position) else {
            return skip(BAD_INPUT, asset_vol);
        };
        let Some(notional) = delta.abs().checked_mul(close) else {
            return skip(BAD_INPUT, asset_vol);
        };
        if delta.is_zero() || notional < self.params.min_trade_notional {
            return Verdict {
                action: Action::Hold,
                intended_qty: Decimal::ZERO,
                expected_edge_bps: None,
                expected_vol: asset_vol,
                reason_code: WITHIN_BAND,
            };
        }

        // Rule 5.
        let action = if delta > Decimal::ZERO {
            Action::Enter
        } else {
            Action::Exit
        };
        Verdict {
            action,
            intended_qty: delta,
            expected_edge_bps: None,
            expected_vol: asset_vol,
            reason_code: trend,
        }
    }
}

/// A feature value, if present. Copies the `Decimal`; no allocation.
fn get(features: &Features, name: &str) -> Option<Decimal> {
    features.get(name).copied()
}

/// A skip: no order, no volatility unless it was computed before the skip.
fn skip(reason_code: &'static str, expected_vol: Option<f64>) -> Verdict {
    Verdict {
        action: Action::Skip,
        intended_qty: Decimal::ZERO,
        expected_edge_bps: None,
        expected_vol,
        reason_code,
    }
}

/// An `InvalidConfig` naming the field.
fn field_error(field: &str, problem: &str) -> EngineError {
    EngineError::InvalidConfig(format!("{field} {problem}"))
}

/// A JSON integer field, if present and representable as `T`.
fn uint_field<T: TryFrom<u64>>(params: &Value, field: &str) -> Result<T, EngineError> {
    params
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|v| T::try_from(v).ok())
        .ok_or_else(|| field_error(field, "must be a non-negative integer"))
}

/// A JSON string field holding a plain decimal.
fn decimal_field(params: &Value, field: &str) -> Result<Decimal, EngineError> {
    let text = params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| field_error(field, "must be a decimal string"))?;
    if !is_plain_decimal(text) {
        return Err(field_error(
            field,
            "must be a plain decimal (digits, an optional leading '-' and one '.')",
        ));
    }
    Decimal::from_str(text).map_err(|_| field_error(field, "is not a valid decimal"))
}

/// Digits with an optional leading `-` and at most one `.`; at least one digit; no exponent.
fn is_plain_decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let mut seen_point = false;
    let mut seen_digit = false;
    for byte in digits.bytes() {
        match byte {
            b'0'..=b'9' => seen_digit = true,
            b'.' if !seen_point => seen_point = true,
            _ => return false,
        }
    }
    seen_digit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecisionContext, ReplayOutcome, decide, replay};
    use nisse_core::{
        StrategyVersionId,
        nautilus::{Symbol, UnixNanos},
    };

    fn d(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn params() -> TrendParams {
        TrendParams {
            lookback_days: 252,
            skip_days: 21,
            vol_window: 60,
            target_vol: d("0.09"),
            max_weight: d("0.25"),
            min_trade_notional: d("25"),
            sessions_per_year: 252,
        }
    }

    fn strategy() -> TrendVolTarget {
        TrendVolTarget::new(params()).unwrap()
    }

    fn features(pairs: &[(&str, &str)]) -> Features {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), d(value)))
            .collect()
    }

    /// A complete, valid feature set: the worked example (16% annual volatility, momentum
    /// 0.10, 8 assets, flat position, equity 100,000, close 100).
    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            (feature::CLOSE, "100"),
            (feature::HISTORY_DAYS, "300"),
            (feature::POSITION_QTY, "0"),
            (feature::EQUITY, "100000"),
            (feature::N_ASSETS, "8"),
            (feature::MOMENTUM, "0.10"),
            (feature::VAR_DAILY, "0.0001015873015873015873015873"),
            (feature::RISK_SCALE, "1"),
        ]
    }

    /// The base set with some features replaced (`Some`) or removed (`None`).
    fn with_features(changes: &[(&str, Option<&str>)]) -> Features {
        let mut f = features(&base());
        for (name, value) in changes {
            match value {
                Some(v) => {
                    f.insert((*name).to_owned(), d(v));
                }
                None => {
                    f.remove(*name);
                }
            }
        }
        f
    }

    fn config_error(params: &Value) -> String {
        match TrendVolTarget::from_params(params) {
            Err(EngineError::InvalidConfig(message)) => message,
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    fn with_field(key: &str, value: Value) -> Value {
        let mut v = strategy().to_params();
        v.as_object_mut().unwrap().insert(key.to_owned(), value);
        v
    }

    fn without_field(key: &str) -> Value {
        let mut v = strategy().to_params();
        v.as_object_mut().unwrap().remove(key);
        v
    }

    fn context(features: Features) -> DecisionContext {
        DecisionContext {
            strategy_version_id: StrategyVersionId(1),
            symbol: Symbol::new("SPY"),
            features,
            data_snapshot_id: "snap-test".to_owned(),
            rng_seed: 7,
            ts: UnixNanos::from(1_000_u64),
        }
    }

    #[test]
    fn worked_example_enters_with_the_documented_quantity() {
        let var = (d("0.0256") / d("252")).to_string();
        let f = with_features(&[(feature::VAR_DAILY, Some(var.as_str()))]);

        let v = strategy().decide(&f, 0);

        // weight = 0.09 / 0.16 / 8 = 0.0703125; shares = 0.0703125 * 100000 / 100 = 70.3125.
        assert_eq!(v.action, Action::Enter);
        assert_eq!(v.intended_qty, d("70.3125"));
        assert_eq!(v.reason_code, TREND_UP);
        assert!((v.expected_vol.unwrap() - 0.16).abs() < 1e-9);
        assert_eq!(v.expected_edge_bps, None);
    }

    #[test]
    fn each_required_feature_is_checked() {
        let names = [
            feature::CLOSE,
            feature::HISTORY_DAYS,
            feature::POSITION_QTY,
            feature::EQUITY,
            feature::N_ASSETS,
            feature::RISK_SCALE,
        ];
        for name in names {
            let v = strategy().decide(&with_features(&[(name, None)]), 0);
            assert_eq!(v.action, Action::Skip, "{name}");
            assert_eq!(v.reason_code, MISSING_FEATURE, "{name}");
            assert_eq!(v.intended_qty, Decimal::ZERO, "{name}");
            assert_eq!(v.expected_vol, None, "{name}");
        }
    }

    #[test]
    fn non_positive_inputs_and_bad_asset_count_are_bad_input() {
        let cases: [&[(&str, Option<&str>)]; 7] = [
            &[(feature::CLOSE, Some("0"))],
            &[(feature::CLOSE, Some("-1"))],
            &[(feature::EQUITY, Some("0"))],
            &[(feature::EQUITY, Some("-5"))],
            &[(feature::N_ASSETS, Some("0"))],
            &[(feature::N_ASSETS, Some("1.5"))],
            &[(feature::N_ASSETS, Some("0.5"))],
        ];
        for changes in cases {
            let v = strategy().decide(&with_features(changes), 0);
            assert_eq!(v.action, Action::Skip, "{changes:?}");
            assert_eq!(v.reason_code, BAD_INPUT, "{changes:?}");
        }
    }

    #[test]
    fn bad_input_is_checked_before_history() {
        let v = strategy().decide(
            &with_features(&[(feature::CLOSE, Some("0")), (feature::MOMENTUM, None)]),
            0,
        );
        assert_eq!(v.reason_code, BAD_INPUT);
    }

    #[test]
    fn missing_momentum_or_variance_is_insufficient_history() {
        let cases: [&[(&str, Option<&str>)]; 3] = [
            &[(feature::MOMENTUM, None)],
            &[(feature::VAR_DAILY, None)],
            &[(feature::MOMENTUM, None), (feature::VAR_DAILY, None)],
        ];
        for changes in cases {
            let v = strategy().decide(&with_features(changes), 0);
            assert_eq!(v.action, Action::Skip, "{changes:?}");
            assert_eq!(v.reason_code, INSUFFICIENT_HISTORY, "{changes:?}");
            assert_eq!(v.expected_vol, None, "{changes:?}");
        }
    }

    #[test]
    fn zero_variance_with_positive_momentum_is_zero_vol() {
        let v = strategy().decide(&with_features(&[(feature::VAR_DAILY, Some("0"))]), 0);
        assert_eq!(v.action, Action::Skip);
        assert_eq!(v.reason_code, ZERO_VOL);
        assert_eq!(v.expected_vol, None);
    }

    #[test]
    fn zero_or_negative_momentum_with_flat_position_holds_within_band() {
        for momentum in ["0", "-0.05"] {
            let f = with_features(&[
                (feature::MOMENTUM, Some(momentum)),
                (feature::POSITION_QTY, Some("0")),
            ]);
            let v = strategy().decide(&f, 0);
            assert_eq!(v.action, Action::Hold, "{momentum}");
            assert_eq!(v.intended_qty, Decimal::ZERO, "{momentum}");
            assert_eq!(v.reason_code, WITHIN_BAND, "{momentum}");
            assert!((v.expected_vol.unwrap() - 0.16).abs() < 1e-9, "{momentum}");
        }
    }

    #[test]
    fn zero_momentum_with_a_flat_asset_of_zero_variance_has_no_vol() {
        let f = with_features(&[
            (feature::MOMENTUM, Some("0")),
            (feature::VAR_DAILY, Some("0")),
        ]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Hold);
        assert_eq!(v.reason_code, WITHIN_BAND);
        assert_eq!(v.expected_vol, None);
    }

    #[test]
    fn falling_trend_with_a_position_exits_all_of_it() {
        let f = with_features(&[
            (feature::MOMENTUM, Some("-0.05")),
            (feature::POSITION_QTY, Some("50")),
        ]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Exit);
        assert_eq!(v.intended_qty, d("-50"));
        assert_eq!(v.reason_code, TREND_DOWN);
        assert!((v.expected_vol.unwrap() - 0.16).abs() < 1e-9);
    }

    #[test]
    fn cap_binds_for_a_low_volatility_asset() {
        // 2% annual volatility, two assets: uncapped weight is 0.09 / 0.02 / 2 = 2.25.
        let var = (d("0.02") * d("0.02") / d("252")).to_string();
        let f = with_features(&[
            (feature::VAR_DAILY, Some(var.as_str())),
            (feature::N_ASSETS, Some("2")),
        ]);
        let v = strategy().decide(&f, 0);
        // Capped at 0.25: 0.25 * 100000 / 100 = 250 shares.
        assert_eq!(v.action, Action::Enter);
        assert_eq!(v.intended_qty, d("250"));
        assert_eq!(v.reason_code, TREND_UP);
        assert!((v.expected_vol.unwrap() - 0.02).abs() < 1e-9);
    }

    #[test]
    fn cap_result_is_floored_to_six_places() {
        let var = (d("0.02") * d("0.02") / d("252")).to_string();
        let f = with_features(&[
            (feature::VAR_DAILY, Some(var.as_str())),
            (feature::N_ASSETS, Some("2")),
            (feature::CLOSE, Some("33.33")),
        ]);
        let v = strategy().decide(&f, 0);
        let expected =
            (d("25000") / d("33.33")).round_dp_with_strategy(6, RoundingStrategy::ToZero);
        assert_eq!(v.intended_qty, expected);
    }

    #[test]
    fn position_just_inside_the_minimum_notional_is_within_band() {
        // Target is 70.3125; a holding 0.2 short has a 20-dollar gap at close 100.
        let f = with_features(&[(feature::POSITION_QTY, Some("70.1125"))]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Hold);
        assert_eq!(v.intended_qty, Decimal::ZERO);
        assert_eq!(v.reason_code, WITHIN_BAND);
    }

    #[test]
    fn gap_exactly_at_the_minimum_notional_trades() {
        // A 0.25-share gap at close 100 is exactly 25 dollars: not less than the minimum.
        let f = with_features(&[(feature::POSITION_QTY, Some("70.0625"))]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Enter);
        assert_eq!(v.intended_qty, d("0.25"));
        assert_eq!(v.reason_code, TREND_UP);
    }

    #[test]
    fn reduction_while_the_trend_is_up_is_an_exit() {
        let f = with_features(&[(feature::POSITION_QTY, Some("80"))]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Exit);
        assert_eq!(v.intended_qty, d("-9.6875"));
        assert_eq!(v.reason_code, TREND_UP);
    }

    #[test]
    fn negative_position_is_bought_back_when_momentum_turns() {
        for momentum in ["0", "-0.05"] {
            let f = with_features(&[
                (feature::MOMENTUM, Some(momentum)),
                (feature::POSITION_QTY, Some("-10")),
            ]);
            let v = strategy().decide(&f, 0);
            assert_eq!(v.action, Action::Enter, "{momentum}");
            assert_eq!(v.intended_qty, d("10"), "{momentum}");
            assert_eq!(v.reason_code, TREND_DOWN, "{momentum}");
        }
    }

    #[test]
    fn overflowing_target_is_bad_input_and_keeps_the_vol() {
        let f = with_features(&[
            (feature::EQUITY, Some("79228162514264337593543950335")),
            (feature::CLOSE, Some("0.001")),
        ]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Skip);
        assert_eq!(v.reason_code, BAD_INPUT);
        assert!((v.expected_vol.unwrap() - 0.16).abs() < 1e-9);
    }

    #[test]
    fn risk_scale_outside_zero_to_one_is_bad_input() {
        for value in ["-0.1", "1.1"] {
            let v = strategy().decide(&with_features(&[(feature::RISK_SCALE, Some(value))]), 0);
            assert_eq!(v.action, Action::Skip, "{value}");
            assert_eq!(v.reason_code, BAD_INPUT, "{value}");
            assert_eq!(v.expected_vol, None, "{value}");
        }
    }

    #[test]
    fn risk_scale_of_zero_half_and_one_is_accepted() {
        // Zero and one give a hold and an entry from flat; 0.5 gives an entry of half size.
        for (value, action) in [
            ("0", Action::Hold),
            ("0.5", Action::Enter),
            ("1", Action::Enter),
        ] {
            let v = strategy().decide(&with_features(&[(feature::RISK_SCALE, Some(value))]), 0);
            assert_eq!(v.action, action, "{value}");
            assert_ne!(v.reason_code, BAD_INPUT, "{value}");
            assert_ne!(v.reason_code, MISSING_FEATURE, "{value}");
        }
    }

    #[test]
    fn half_risk_scale_halves_the_uncapped_entry() {
        // Uncapped: weight 0.0703125 at scale 1 gives 0.0703125 * 100000 / 100 = 70.3125 shares.
        // At scale 0.5 the weight is 0.03515625, giving 35.15625 shares, exactly half.
        let full = strategy().decide(&features(&base()), 0);
        let half = strategy().decide(&with_features(&[(feature::RISK_SCALE, Some("0.5"))]), 0);
        assert_eq!(full.intended_qty, d("70.3125"));
        assert_eq!(half.intended_qty, d("35.15625"));
        assert_eq!(half.intended_qty * d("2"), full.intended_qty);
        assert_eq!(half.action, Action::Enter);
        assert_eq!(half.reason_code, TREND_UP);
    }

    #[test]
    fn half_risk_scale_halves_the_capped_entry() {
        // Capped: two assets at 2% volatility give 2.25 uncapped, so the weight is 0.25.
        // At scale 1: 0.25 * 100000 / 100 = 250 shares. At scale 0.5: weight 0.125 gives 125.
        let var = (d("0.02") * d("0.02") / d("252")).to_string();
        let changes = [
            (feature::VAR_DAILY, Some(var.as_str())),
            (feature::N_ASSETS, Some("2")),
        ];
        let full = strategy().decide(&with_features(&changes), 0);
        let half = strategy().decide(
            &with_features(&[
                (feature::VAR_DAILY, Some(var.as_str())),
                (feature::N_ASSETS, Some("2")),
                (feature::RISK_SCALE, Some("0.5")),
            ]),
            0,
        );
        assert_eq!(full.intended_qty, d("250"));
        assert_eq!(half.intended_qty, d("125"));
        assert_eq!(half.intended_qty * d("2"), full.intended_qty);
    }

    #[test]
    fn zero_risk_scale_from_flat_does_not_enter() {
        // Target is 0 shares, delta is 0: the within-band hold, as for a flat zero target.
        let f = with_features(&[(feature::RISK_SCALE, Some("0"))]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Hold);
        assert_eq!(v.intended_qty, Decimal::ZERO);
        assert_eq!(v.reason_code, WITHIN_BAND);
        assert!((v.expected_vol.unwrap() - 0.16).abs() < 1e-9);
    }

    #[test]
    fn zero_risk_scale_with_a_position_exits_all_of_it() {
        // Target 0 with positive momentum: delta is -100, exited through the existing path.
        // The reason code is `trend_up`, as for any reduction while the momentum is positive.
        let f = with_features(&[
            (feature::RISK_SCALE, Some("0")),
            (feature::POSITION_QTY, Some("100")),
        ]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Exit);
        assert_eq!(v.intended_qty, d("-100"));
        assert_eq!(v.reason_code, TREND_UP);
    }

    #[test]
    fn half_risk_scale_sells_half_of_the_full_target() {
        // Holding the scale-1 target 70.3125; at scale 0.5 the target is 35.15625.
        // Delta is -35.15625, notional 3515.625, well above the minimum.
        let f = with_features(&[
            (feature::RISK_SCALE, Some("0.5")),
            (feature::POSITION_QTY, Some("70.3125")),
        ]);
        let v = strategy().decide(&f, 0);
        assert_eq!(v.action, Action::Exit);
        assert_eq!(v.intended_qty, d("-35.15625"));
        assert_eq!(v.reason_code, TREND_UP);
    }

    #[test]
    fn decide_is_deterministic_and_ignores_the_seed() {
        let s = strategy();
        let f = features(&base());
        let first = s.decide(&f, 0);
        for i in 0..1_000_u64 {
            assert_eq!(s.decide(&f, i), first);
        }
        assert_eq!(first.action, Action::Enter);
    }

    #[test]
    fn to_params_round_trips_through_from_params() {
        let s = strategy();
        let v = s.to_params();
        assert_eq!(v["strategy"], "trend_vol_target");
        assert_eq!(v["target_vol"], "0.09");
        assert_eq!(v["max_weight"], "0.25");
        assert_eq!(v["min_trade_notional"], "25");
        assert_eq!(v["lookback_days"], 252);
        assert_eq!(v["sessions_per_year"], 252);
        assert_eq!(TrendVolTarget::from_params(&v).unwrap(), s);
    }

    #[test]
    fn a_missing_field_is_invalid_config_naming_it() {
        for key in [
            "lookback_days",
            "skip_days",
            "vol_window",
            "target_vol",
            "max_weight",
            "min_trade_notional",
            "sessions_per_year",
        ] {
            let message = config_error(&without_field(key));
            assert!(message.contains(key), "{message}");
        }
    }

    #[test]
    fn a_wrong_json_type_is_invalid_config_naming_it() {
        let cases = [
            ("lookback_days", json!("252")),
            ("vol_window", json!(60.0)),
            ("target_vol", json!(0.09)),
            ("max_weight", json!(25)),
            ("min_trade_notional", json!(true)),
            ("sessions_per_year", json!("252")),
        ];
        for (key, value) in cases {
            let message = config_error(&with_field(key, value));
            assert!(message.contains(key), "{message}");
        }
    }

    #[test]
    fn a_wrong_strategy_name_is_invalid_config() {
        assert!(config_error(&with_field("strategy", json!("other"))).contains("strategy"));
        assert!(config_error(&with_field("strategy", json!(1))).contains("strategy"));
    }

    #[test]
    fn decimals_that_are_not_plain_are_invalid_config() {
        for text in ["1e-2", "+0.1", ".", "-", "0.1.2", ""] {
            let message = config_error(&with_field("target_vol", json!(text)));
            assert!(message.contains("target_vol"), "{text}: {message}");
        }
    }

    #[test]
    fn invalid_parameter_values_are_invalid_config() {
        let cases = [
            ("lookback_days", json!(21)),
            ("lookback_days", json!(5)),
            ("vol_window", json!(1)),
            ("target_vol", json!("0")),
            ("target_vol", json!("1.5")),
            ("max_weight", json!("0")),
            ("max_weight", json!("1.5")),
            ("min_trade_notional", json!("-1")),
            ("sessions_per_year", json!(0)),
        ];
        for (key, value) in cases {
            let message = config_error(&with_field(key, value.clone()));
            assert!(message.contains(key), "{key} = {value}: {message}");
        }
    }

    #[test]
    fn new_rejects_the_same_values_as_from_params() {
        let mut bad = params();
        bad.skip_days = 252;
        assert!(matches!(
            TrendVolTarget::new(bad),
            Err(EngineError::InvalidConfig(_))
        ));
    }

    #[test]
    fn build_returns_the_strategy_for_its_name() {
        let Ok(built) = crate::strategies::build(&strategy().to_params()) else {
            panic!("the trend strategy's own parameters must build");
        };
        assert_eq!(built.strategy.name(), TREND_VOL_TARGET);
        assert_eq!(built.window, strategy().window_spec());
        assert_eq!(
            built.sessions_per_year,
            strategy().params().sessions_per_year
        );
    }

    #[test]
    fn build_rejects_unknown_and_missing_names() {
        assert!(matches!(
            crate::strategies::build(&json!({"strategy": "nope"})),
            Err(EngineError::UnknownStrategy(_))
        ));
        assert!(matches!(
            crate::strategies::build(&json!({})),
            Err(EngineError::UnknownStrategy(_))
        ));
    }

    #[test]
    fn replay_matches_for_an_enter_a_hold_and_a_skip() {
        let s = strategy();

        let enter = decide(&s, context(features(&base())));
        assert_eq!(enter.action, Action::Enter);
        assert_eq!(replay(&s, &enter), ReplayOutcome::Match);

        let hold = decide(
            &s,
            context(with_features(&[(feature::POSITION_QTY, Some("70.1125"))])),
        );
        assert_eq!(hold.action, Action::Hold);
        assert_eq!(replay(&s, &hold), ReplayOutcome::Match);

        let skip = decide(&s, context(with_features(&[(feature::CLOSE, None)])));
        assert_eq!(skip.action, Action::Skip);
        assert_eq!(replay(&s, &skip), ReplayOutcome::Match);
    }

    #[test]
    fn replay_matches_a_decision_scaled_by_risk_scale() {
        let s = strategy();
        let scaled = decide(
            &s,
            context(with_features(&[(feature::RISK_SCALE, Some("0.5"))])),
        );
        assert_eq!(scaled.action, Action::Enter);
        assert_eq!(replay(&s, &scaled), ReplayOutcome::Match);
    }

    #[test]
    fn window_spec_matches_the_params() {
        let w = strategy().window_spec();
        assert_eq!((w.lookback_days, w.skip_days, w.vol_window), (252, 21, 60));
    }
}
