//! The risk limits, read from a reviewed JSON file.

use std::path::Path;
use std::str::FromStr;

use nisse_core::{Hash32, config_hash};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};

use crate::RiskError;

/// Every field a limits file must hold. Anything else in the file is an error.
const FIELDS: [&str; 12] = [
    "max_risk_per_trade",
    "risk_sigmas",
    "kelly_fraction",
    "haircut_sharpe",
    "portfolio_vol",
    "drawdown_halve",
    "drawdown_flat",
    "drawdown_resume",
    "max_bar_age_hours",
    "max_day_trades",
    "flatten_on_kill",
    "heartbeat_max_age_seconds",
];

/// The risk limits. Built only by [`Limits::from_json`] / [`Limits::load`], which refuse
/// values looser than the ceilings fixed in this file (the struct is `non_exhaustive`, so no
/// other crate can write one out by hand).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Largest share of equity one order may put at risk.
    pub max_risk_per_trade: Decimal,
    /// "At risk" is the order's notional times this many daily standard deviations.
    pub risk_sigmas: Decimal,
    /// Share of the Kelly leverage that may be used.
    pub kelly_fraction: Decimal,
    /// The strategy's Sharpe ratio after the multiple-testing haircut.
    pub haircut_sharpe: Decimal,
    /// The annual portfolio volatility the Kelly leverage is computed with.
    pub portfolio_vol: Decimal,
    /// Drawdown at which exposure is halved.
    pub drawdown_halve: Decimal,
    /// Drawdown at which the account goes flat, until a person resets it.
    pub drawdown_flat: Decimal,
    /// Drawdown below which halved exposure returns to normal.
    pub drawdown_resume: Decimal,
    /// Oldest the newest daily bar may be for a new entry.
    pub max_bar_age_hours: u32,
    /// Day trades allowed in any 7 calendar days.
    pub max_day_trades: u32,
    /// Whether an engaged kill switch also exits every position.
    pub flatten_on_kill: bool,
    /// Oldest the newest heartbeat may be before the dead-man check fails.
    pub heartbeat_max_age_seconds: u32,
}

impl Limits {
    /// Reads and validates the limits file at `path`.
    pub fn load(path: &Path) -> Result<Self, RiskError> {
        let text = std::fs::read_to_string(path).map_err(|source| RiskError::LimitsFile {
            path: path.display().to_string(),
            source,
        })?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|err| RiskError::InvalidLimits(format!("not valid JSON: {err}")))?;
        Self::from_json(&value)
    }

    /// Parses and validates limits. Decimals are JSON strings; a JSON number where a decimal
    /// is expected, a missing field, an unknown field, or a value outside its accepted range
    /// (`docs/phases/phase-4.md`, "Limits") is `InvalidLimits` naming the field.
    pub fn from_json(value: &Value) -> Result<Self, RiskError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("limits", "must be a JSON object"))?;
        if let Some(unknown) = object.keys().find(|key| !FIELDS.contains(&key.as_str())) {
            return Err(invalid(unknown, "is an unknown field"));
        }
        let limits = Self {
            max_risk_per_trade: decimal(object, "max_risk_per_trade")?,
            risk_sigmas: decimal(object, "risk_sigmas")?,
            kelly_fraction: decimal(object, "kelly_fraction")?,
            haircut_sharpe: decimal(object, "haircut_sharpe")?,
            portfolio_vol: decimal(object, "portfolio_vol")?,
            drawdown_halve: decimal(object, "drawdown_halve")?,
            drawdown_flat: decimal(object, "drawdown_flat")?,
            drawdown_resume: decimal(object, "drawdown_resume")?,
            max_bar_age_hours: integer(object, "max_bar_age_hours")?,
            max_day_trades: integer(object, "max_day_trades")?,
            flatten_on_kill: field(object, "flatten_on_kill")?
                .as_bool()
                .ok_or_else(|| invalid("flatten_on_kill", "must be true or false"))?,
            heartbeat_max_age_seconds: integer(object, "heartbeat_max_age_seconds")?,
        };
        limits.validate()?;
        Ok(limits)
    }

    /// The ceilings. A limits file can tighten the rules, never loosen them past these.
    fn validate(&self) -> Result<(), RiskError> {
        let zero = Decimal::ZERO;
        let d = |units: i64, scale: u32| Decimal::new(units, scale);
        check(
            "max_risk_per_trade",
            self.max_risk_per_trade > zero && self.max_risk_per_trade <= d(5, 3),
            "must be above 0 and at most 0.005",
        )?;
        check(
            "risk_sigmas",
            self.risk_sigmas >= d(1, 0) && self.risk_sigmas <= d(10, 0),
            "must be between 1 and 10",
        )?;
        check(
            "kelly_fraction",
            self.kelly_fraction > zero && self.kelly_fraction <= d(25, 2),
            "must be above 0 and at most 0.25",
        )?;
        check(
            "haircut_sharpe",
            self.haircut_sharpe > zero && self.haircut_sharpe <= d(2, 0),
            "must be above 0 and at most 2",
        )?;
        check(
            "portfolio_vol",
            self.portfolio_vol >= d(5, 2) && self.portfolio_vol <= d(5, 1),
            "must be between 0.05 and 0.5",
        )?;
        check(
            "drawdown_resume",
            self.drawdown_resume > zero,
            "must be above 0",
        )?;
        check(
            "drawdown_halve",
            self.drawdown_halve > self.drawdown_resume && self.drawdown_halve <= d(10, 2),
            "must be above drawdown_resume and at most 0.10",
        )?;
        check(
            "drawdown_flat",
            self.drawdown_flat > self.drawdown_halve && self.drawdown_flat <= d(20, 2),
            "must be above drawdown_halve and at most 0.20",
        )?;
        check(
            "max_bar_age_hours",
            (1..=240).contains(&self.max_bar_age_hours),
            "must be between 1 and 240",
        )?;
        check(
            "max_day_trades",
            self.max_day_trades <= 3,
            "must be at most 3",
        )?;
        check(
            "heartbeat_max_age_seconds",
            (30..=3600).contains(&self.heartbeat_max_age_seconds),
            "must be between 30 and 3600",
        )
    }

    /// The limits as JSON, decimals as normalised strings. `from_json(&to_json())` gives the
    /// same limits back.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let text = |value: Decimal| value.normalize().to_string();
        json!({
            "max_risk_per_trade": text(self.max_risk_per_trade),
            "risk_sigmas": text(self.risk_sigmas),
            "kelly_fraction": text(self.kelly_fraction),
            "haircut_sharpe": text(self.haircut_sharpe),
            "portfolio_vol": text(self.portfolio_vol),
            "drawdown_halve": text(self.drawdown_halve),
            "drawdown_flat": text(self.drawdown_flat),
            "drawdown_resume": text(self.drawdown_resume),
            "max_bar_age_hours": self.max_bar_age_hours,
            "max_day_trades": self.max_day_trades,
            "flatten_on_kill": self.flatten_on_kill,
            "heartbeat_max_age_seconds": self.heartbeat_max_age_seconds,
        })
    }

    /// SHA-256 of the canonical JSON of [`Self::to_json`]: two files that differ only in
    /// layout or in trailing zeros have the same hash.
    #[must_use]
    pub fn hash(&self) -> Hash32 {
        config_hash(&self.to_json())
    }
}

fn invalid(field: &str, reason: &str) -> RiskError {
    RiskError::InvalidLimits(format!("{field}: {reason}"))
}

fn check(field: &str, ok: bool, reason: &str) -> Result<(), RiskError> {
    if ok {
        Ok(())
    } else {
        Err(invalid(field, reason))
    }
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, RiskError> {
    object.get(name).ok_or_else(|| invalid(name, "is missing"))
}

/// A decimal given as a JSON string. A JSON number is refused: it would arrive as a float.
fn decimal(object: &Map<String, Value>, name: &str) -> Result<Decimal, RiskError> {
    let text = field(object, name)?
        .as_str()
        .ok_or_else(|| invalid(name, "must be a decimal given as a JSON string"))?;
    Decimal::from_str(text).map_err(|_| invalid(name, "is not a valid decimal"))
}

fn integer(object: &Map<String, Value>, name: &str) -> Result<u32, RiskError> {
    field(object, name)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid(name, "must be a non-negative whole number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = include_str!("../../../config/limits/limits.json");

    fn default_json() -> Value {
        serde_json::from_str(DEFAULT).unwrap()
    }

    fn with(name: &str, value: Value) -> Value {
        let mut json = default_json();
        json[name] = value;
        json
    }

    fn rejects(json: &Value, field: &str) {
        match Limits::from_json(json) {
            Err(RiskError::InvalidLimits(message)) => {
                assert!(
                    message.starts_with(field),
                    "{message} does not name {field}"
                );
            }
            other => panic!("expected InvalidLimits for {field}, got {other:?}"),
        }
    }

    #[test]
    fn the_repository_file_parses_to_the_documented_defaults() {
        let limits = Limits::from_json(&default_json()).unwrap();
        assert_eq!(limits.max_risk_per_trade, Decimal::new(5, 3));
        assert_eq!(limits.risk_sigmas, Decimal::new(2, 0));
        assert_eq!(limits.kelly_fraction, Decimal::new(25, 2));
        assert_eq!(limits.haircut_sharpe, Decimal::new(4, 1));
        assert_eq!(limits.portfolio_vol, Decimal::new(9, 2));
        assert_eq!(limits.drawdown_halve, Decimal::new(10, 2));
        assert_eq!(limits.drawdown_flat, Decimal::new(15, 2));
        assert_eq!(limits.drawdown_resume, Decimal::new(5, 2));
        assert_eq!(limits.max_bar_age_hours, 96);
        assert_eq!(limits.max_day_trades, 3);
        assert!(!limits.flatten_on_kill);
        assert_eq!(limits.heartbeat_max_age_seconds, 300);
    }

    #[test]
    fn every_field_is_required() {
        for name in FIELDS {
            let mut json = default_json();
            json.as_object_mut().unwrap().remove(name);
            rejects(&json, name);
        }
    }

    #[test]
    fn an_unknown_field_is_refused() {
        rejects(&with("max_leverage", json!("10")), "max_leverage");
    }

    #[test]
    fn a_float_is_refused_for_a_decimal() {
        rejects(
            &with("max_risk_per_trade", json!(0.005)),
            "max_risk_per_trade",
        );
    }

    #[test]
    fn something_that_is_not_an_object_is_refused() {
        rejects(&json!([1, 2]), "limits");
    }

    #[test]
    fn values_looser_than_the_ceilings_are_refused() {
        let cases = [
            ("max_risk_per_trade", json!("0.0051")),
            ("max_risk_per_trade", json!("0")),
            ("max_risk_per_trade", json!("-0.001")),
            ("risk_sigmas", json!("0.99")),
            ("risk_sigmas", json!("11")),
            ("kelly_fraction", json!("0.26")),
            ("kelly_fraction", json!("0")),
            ("haircut_sharpe", json!("2.01")),
            ("haircut_sharpe", json!("0")),
            ("portfolio_vol", json!("0.049")),
            ("portfolio_vol", json!("0.51")),
            ("drawdown_resume", json!("0")),
            ("drawdown_halve", json!("0.11")),
            ("drawdown_halve", json!("0.05")),
            ("drawdown_flat", json!("0.21")),
            ("drawdown_flat", json!("0.10")),
            ("max_bar_age_hours", json!(0)),
            ("max_bar_age_hours", json!(241)),
            ("max_bar_age_hours", json!(-1)),
            ("max_day_trades", json!(4)),
            ("flatten_on_kill", json!("yes")),
            ("heartbeat_max_age_seconds", json!(29)),
            ("heartbeat_max_age_seconds", json!(3601)),
        ];
        for (name, value) in cases {
            rejects(&with(name, value), name);
        }
    }

    #[test]
    fn tighter_values_are_accepted() {
        let mut json = default_json();
        json["max_risk_per_trade"] = json!("0.001");
        json["kelly_fraction"] = json!("0.1");
        json["drawdown_halve"] = json!("0.06");
        json["drawdown_flat"] = json!("0.08");
        json["max_day_trades"] = json!(0);
        json["flatten_on_kill"] = json!(true);
        assert!(Limits::from_json(&json).is_ok());
    }

    #[test]
    fn to_json_round_trips() {
        let limits = Limits::from_json(&default_json()).unwrap();
        assert_eq!(Limits::from_json(&limits.to_json()).unwrap(), limits);
    }

    #[test]
    fn the_hash_ignores_layout_and_trailing_zeros_but_not_values() {
        let limits = Limits::from_json(&default_json()).unwrap();
        let same = Limits::from_json(&with("drawdown_halve", json!("0.1"))).unwrap();
        let tighter = Limits::from_json(&with("drawdown_halve", json!("0.09"))).unwrap();
        assert_eq!(limits.hash(), same.hash());
        assert_ne!(limits.hash(), tighter.hash());
    }

    #[test]
    fn load_reads_the_repository_file_and_reports_a_missing_one() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/limits/limits.json"
        ));
        assert_eq!(
            Limits::load(path).unwrap(),
            Limits::from_json(&default_json()).unwrap()
        );
        assert!(matches!(
            Limits::load(Path::new("/nonexistent/limits.json")),
            Err(RiskError::LimitsFile { .. })
        ));
    }
}
