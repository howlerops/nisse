//! The promotion gate thresholds, read from a reviewed JSON file.

use std::path::Path;

use chrono::NaiveDate;
use serde_json::{Map, Value};

use crate::EvalError;

const FIELDS: [&str; 14] = [
    "deflated_sharpe_min",
    "pbo_max",
    "pbo_min_configs",
    "trial_budget",
    "sharpe_haircut",
    "cv_folds",
    "cv_embargo_sessions",
    "holdout_start",
    "holdout_max_looks",
    "tracking_error_max",
    "tracking_gap_max",
    "benchmark",
    "periods_per_year",
    "bootstrap_seed",
];

/// The gate thresholds. Built only by [`Gates::from_json`] / [`Gates::load`], which refuse
/// thresholds looser than PLAN.md's (`docs/phases/phase-5.md`, "Gates").
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Gates {
    pub deflated_sharpe_min: f64,
    pub pbo_max: f64,
    /// Fewest configurations over the same data for the overfitting probability to mean
    /// anything; with fewer the gate is not evaluated.
    pub pbo_min_configs: u32,
    pub trial_budget: u32,
    /// The share of the backtest Sharpe removed before it may be used for sizing.
    pub sharpe_haircut: f64,
    pub cv_folds: u32,
    pub cv_embargo_sessions: u32,
    /// Bars on or after this date are the holdout.
    pub holdout_start: NaiveDate,
    pub holdout_max_looks: u32,
    pub tracking_error_max: f64,
    pub tracking_gap_max: f64,
    pub benchmark: String,
    pub periods_per_year: u32,
    pub bootstrap_seed: u64,
}

impl Gates {
    /// Reads and validates the gates file at `path`.
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| invalid("gates file", &format!("cannot be read: {err}")))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|err| invalid("gates file", &format!("is not valid JSON: {err}")))?;
        Self::from_json(&value)
    }

    /// Parses and validates gates. A missing field, an unknown field or a value outside its
    /// accepted range is `InvalidGates` naming the field.
    pub fn from_json(value: &Value) -> Result<Self, EvalError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("gates", "must be a JSON object"))?;
        if let Some(unknown) = object.keys().find(|key| !FIELDS.contains(&key.as_str())) {
            return Err(invalid(unknown, "is an unknown field"));
        }
        let holdout_text = field(object, "holdout_start")?
            .as_str()
            .ok_or_else(|| invalid("holdout_start", "must be a date as a string, YYYY-MM-DD"))?;
        let gates = Self {
            deflated_sharpe_min: number(object, "deflated_sharpe_min")?,
            pbo_max: number(object, "pbo_max")?,
            pbo_min_configs: integer(object, "pbo_min_configs")?,
            trial_budget: integer(object, "trial_budget")?,
            sharpe_haircut: number(object, "sharpe_haircut")?,
            cv_folds: integer(object, "cv_folds")?,
            cv_embargo_sessions: integer(object, "cv_embargo_sessions")?,
            holdout_start: NaiveDate::parse_from_str(holdout_text, "%Y-%m-%d")
                .map_err(|_| invalid("holdout_start", "must be a date as YYYY-MM-DD"))?,
            holdout_max_looks: integer(object, "holdout_max_looks")?,
            tracking_error_max: number(object, "tracking_error_max")?,
            tracking_gap_max: number(object, "tracking_gap_max")?,
            benchmark: field(object, "benchmark")?
                .as_str()
                .filter(|symbol| !symbol.is_empty())
                .ok_or_else(|| invalid("benchmark", "must be a non-empty string"))?
                .to_owned(),
            periods_per_year: integer(object, "periods_per_year")?,
            bootstrap_seed: field(object, "bootstrap_seed")?
                .as_u64()
                .ok_or_else(|| invalid("bootstrap_seed", "must be a non-negative whole number"))?,
        };
        gates.validate()?;
        Ok(gates)
    }

    /// A gates file can tighten the thresholds, never loosen them past PLAN.md.
    fn validate(&self) -> Result<(), EvalError> {
        check(
            "deflated_sharpe_min",
            self.deflated_sharpe_min >= 0.95 && self.deflated_sharpe_min < 1.0,
            "must be at least 0.95 and below 1",
        )?;
        check(
            "pbo_max",
            self.pbo_max > 0.0 && self.pbo_max <= 0.2,
            "must be above 0 and at most 0.2",
        )?;
        check(
            "pbo_min_configs",
            self.pbo_min_configs >= 5,
            "must be at least 5",
        )?;
        check(
            "holdout_start",
            self.holdout_start <= latest_holdout_start(),
            "must not be later than 2023-01-01 (a later date shrinks the holdout)",
        )?;
        check(
            "trial_budget",
            (1..=100).contains(&self.trial_budget),
            "must be between 1 and 100",
        )?;
        check(
            "sharpe_haircut",
            self.sharpe_haircut >= 0.5 && self.sharpe_haircut < 1.0,
            "must be at least 0.5 and below 1",
        )?;
        check(
            "cv_folds",
            (2..=20).contains(&self.cv_folds),
            "must be between 2 and 20",
        )?;
        check(
            "cv_embargo_sessions",
            self.cv_embargo_sessions >= 21,
            "must be at least 21",
        )?;
        check(
            "holdout_max_looks",
            (1..=10).contains(&self.holdout_max_looks),
            "must be between 1 and 10",
        )?;
        check(
            "tracking_error_max",
            self.tracking_error_max > 0.0 && self.tracking_error_max <= 0.10,
            "must be above 0 and at most 0.10",
        )?;
        check(
            "tracking_gap_max",
            self.tracking_gap_max > 0.0 && self.tracking_gap_max <= 0.10,
            "must be above 0 and at most 0.10",
        )?;
        check(
            "periods_per_year",
            (200..=366).contains(&self.periods_per_year),
            "must be between 200 and 366",
        )
    }
}

/// The latest date the holdout may start. The holdout is the reserved end of the bar history;
/// a gates file may reserve more of it (an earlier date), never less.
#[must_use]
pub fn latest_holdout_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, 1).unwrap_or(NaiveDate::MAX)
}

fn invalid(field: &str, reason: &str) -> EvalError {
    EvalError::InvalidGates(format!("{field}: {reason}"))
}

fn check(field: &str, ok: bool, reason: &str) -> Result<(), EvalError> {
    if ok {
        Ok(())
    } else {
        Err(invalid(field, reason))
    }
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, EvalError> {
    object.get(name).ok_or_else(|| invalid(name, "is missing"))
}

/// A finite JSON number. (Thresholds are statistics, not money, so a float is right here.)
fn number(object: &Map<String, Value>, name: &str) -> Result<f64, EvalError> {
    field(object, name)?
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid(name, "must be a number"))
}

fn integer(object: &Map<String, Value>, name: &str) -> Result<u32, EvalError> {
    field(object, name)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid(name, "must be a non-negative whole number"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DEFAULT: &str = include_str!("../../../config/eval/gates.json");

    fn default_json() -> Value {
        serde_json::from_str(DEFAULT).unwrap()
    }

    fn with(name: &str, value: Value) -> Value {
        let mut json = default_json();
        json[name] = value;
        json
    }

    fn rejects(json: &Value, field: &str) {
        match Gates::from_json(json) {
            Err(EvalError::InvalidGates(message)) => {
                assert!(
                    message.starts_with(field),
                    "{message} does not name {field}"
                );
            }
            other => panic!("expected InvalidGates for {field}, got {other:?}"),
        }
    }

    #[test]
    fn the_repository_file_parses_to_the_documented_defaults() {
        let gates = Gates::from_json(&default_json()).unwrap();
        assert!((gates.deflated_sharpe_min - 0.95).abs() < f64::EPSILON);
        assert!((gates.pbo_max - 0.2).abs() < f64::EPSILON);
        assert_eq!(gates.pbo_min_configs, 10);
        assert_eq!(gates.trial_budget, 100);
        assert!((gates.sharpe_haircut - 0.5).abs() < f64::EPSILON);
        assert_eq!(gates.cv_folds, 8);
        assert_eq!(gates.cv_embargo_sessions, 21);
        assert_eq!(
            gates.holdout_start,
            NaiveDate::from_ymd_opt(2023, 1, 1).unwrap()
        );
        assert_eq!(gates.holdout_max_looks, 10);
        assert_eq!(gates.benchmark, "SPY");
        assert_eq!(gates.periods_per_year, 252);
        assert_eq!(gates.bootstrap_seed, 20_261_126);
    }

    #[test]
    fn every_field_is_required_and_unknown_fields_are_refused() {
        for name in FIELDS {
            let mut json = default_json();
            json.as_object_mut().unwrap().remove(name);
            rejects(&json, name);
        }
        rejects(&with("extra", json!(1)), "extra");
        rejects(&json!([]), "gates");
    }

    #[test]
    fn thresholds_looser_than_the_plan_are_refused() {
        let cases = [
            ("deflated_sharpe_min", json!(0.94)),
            ("deflated_sharpe_min", json!(1.0)),
            ("deflated_sharpe_min", json!("0.95")),
            ("pbo_max", json!(0.21)),
            ("pbo_max", json!(0.0)),
            ("pbo_min_configs", json!(4)),
            ("holdout_start", json!("2023-01-02")),
            ("holdout_start", json!("2099-01-01")),
            ("trial_budget", json!(101)),
            ("trial_budget", json!(0)),
            ("sharpe_haircut", json!(0.49)),
            ("sharpe_haircut", json!(1.0)),
            ("cv_folds", json!(1)),
            ("cv_folds", json!(21)),
            ("cv_embargo_sessions", json!(20)),
            ("holdout_start", json!("2023-13-01")),
            ("holdout_start", json!(20230101)),
            ("holdout_max_looks", json!(11)),
            ("holdout_max_looks", json!(0)),
            ("tracking_error_max", json!(0.11)),
            ("tracking_gap_max", json!(0.0)),
            ("benchmark", json!("")),
            ("periods_per_year", json!(199)),
            ("bootstrap_seed", json!(-1)),
        ];
        for (name, value) in cases {
            rejects(&with(name, value), name);
        }
    }

    #[test]
    fn tighter_thresholds_are_accepted() {
        let mut json = default_json();
        json["deflated_sharpe_min"] = json!(0.99);
        json["pbo_max"] = json!(0.1);
        json["trial_budget"] = json!(50);
        json["sharpe_haircut"] = json!(0.75);
        json["holdout_max_looks"] = json!(3);
        json["holdout_start"] = json!("2020-01-01");
        json["pbo_min_configs"] = json!(20);
        assert!(Gates::from_json(&json).is_ok());
    }

    #[test]
    fn load_reads_the_repository_file_and_reports_a_missing_one() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/eval/gates.json"
        ));
        assert_eq!(
            Gates::load(path).unwrap(),
            Gates::from_json(&default_json()).unwrap()
        );
        assert!(matches!(
            Gates::load(Path::new("/nonexistent/gates.json")),
            Err(EvalError::InvalidGates(_))
        ));
    }
}
