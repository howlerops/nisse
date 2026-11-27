//! The engine config file (`config/engine/paper.json`): what to trade, with which feed and limits.
//!
//! The file is parsed through a private raw struct (strings for symbols, feed and dates) and then
//! checked field by field. Every violation is an [`AppError::InvalidConfig`] whose message names
//! the field. Unknown fields are errors. Only the paper environment is accepted by this build.

use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};
use nisse_alpaca_adapter::Feed;
use nisse_core::nautilus::Symbol;
use serde_json::Value;

use crate::AppError;

/// The only environment this build runs against.
const PAPER: &str = "paper";
/// The longest symbol accepted. Alpaca tickers are short; this bounds the input.
const MAX_SYMBOL_LEN: usize = 12;
/// The earliest first day of bar history. Bars before 2000 are not fetched.
const EARLIEST_HISTORY_YEAR: i32 = 2000;
/// The length of a `YYYY-MM-DD` date.
const DATE_LEN: usize = 10;
/// The heartbeat source name when the file does not give one.
const DEFAULT_SOURCE: &str = "engine";

/// The engine's configuration, parsed and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    /// The strategy parameters exactly as written: stored as `strategy_version.params`.
    pub strategy: serde_json::Value,
    /// The universe, sorted by symbol, no duplicates.
    pub symbols: Vec<Symbol>,
    pub benchmark: Symbol,
    pub feed: Feed,
    pub history_start: NaiveDate,
    pub bars_dir: PathBuf,
    pub limits_path: PathBuf,
    pub seed: u64,
    /// Heartbeat source name.
    pub source: String,
}

/// The file as written: strings for symbols, feed and dates, checked by [`EngineConfig::parse`].
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    environment: String,
    strategy: Value,
    symbols: Vec<String>,
    benchmark: String,
    feed: String,
    history_start: String,
    bars_dir: String,
    limits_path: String,
    /// Read as a JSON value so that a wrong type can be reported with the field name.
    seed: Value,
    #[serde(default = "default_source")]
    source: String,
}

fn default_source() -> String {
    DEFAULT_SOURCE.to_owned()
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::InvalidConfig(message.into())
}

/// Checks one symbol: non-empty, at most [`MAX_SYMBOL_LEN`] characters, upper-case ASCII
/// letters, digits and `.` only. `field` names the setting in the error message.
fn check_symbol(field: &str, text: &str) -> Result<Symbol, AppError> {
    if text.is_empty() {
        return Err(invalid(format!("{field}: symbol must not be empty")));
    }
    if text.len() > MAX_SYMBOL_LEN {
        return Err(invalid(format!(
            "{field}: symbol {text:?} is longer than {MAX_SYMBOL_LEN} characters"
        )));
    }
    let allowed = text
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.');
    if !allowed {
        return Err(invalid(format!(
            "{field}: symbol {text:?} must be upper-case letters, digits or '.' only"
        )));
    }
    Symbol::new_checked(text).map_err(|e| invalid(format!("{field}: {e}")))
}

/// Checks a path setting: it must not be empty.
fn check_path(field: &str, text: &str) -> Result<PathBuf, AppError> {
    if text.trim().is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    Ok(PathBuf::from(text))
}

impl EngineConfig {
    /// Parses and checks the JSON text.
    pub fn parse(text: &str) -> Result<Self, AppError> {
        let raw: RawConfig =
            serde_json::from_str(text).map_err(|e| invalid(format!("invalid JSON: {e}")))?;

        if raw.environment != PAPER {
            return Err(invalid(format!(
                "environment {:?} is refused: this build only runs against {PAPER}",
                raw.environment
            )));
        }

        if !raw.strategy.is_object() {
            return Err(invalid("strategy must be a JSON object"));
        }
        nisse_engine::strategies::build(&raw.strategy)
            .map_err(|e| invalid(format!("strategy: {e}")))?;

        if raw.symbols.is_empty() {
            return Err(invalid("symbols must list at least one symbol"));
        }
        let mut symbols = raw
            .symbols
            .iter()
            .map(|s| check_symbol("symbols", s))
            .collect::<Result<Vec<_>, _>>()?;
        symbols.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        if let Some(pair) = symbols.windows(2).find(|w| w[0] == w[1]) {
            return Err(invalid(format!(
                "symbols: duplicate symbol {}",
                pair[0].as_str()
            )));
        }

        let benchmark = check_symbol("benchmark", &raw.benchmark)?;

        let feed = match raw.feed.as_str() {
            "iex" => Feed::Iex,
            "sip" => Feed::Sip,
            other => {
                return Err(invalid(format!(
                    "feed {other:?} is not known: use \"iex\" or \"sip\""
                )));
            }
        };

        let history_start =
            // chrono also accepts unpadded fields (`2015-1-5`); the file must spell YYYY-MM-DD.
            NaiveDate::parse_from_str(&raw.history_start, "%Y-%m-%d")
                .ok()
                .filter(|_| raw.history_start.len() == DATE_LEN)
                .ok_or_else(|| {
                    invalid(format!(
                        "history_start {:?} must be a date as YYYY-MM-DD",
                        raw.history_start
                    ))
                })?;
        if history_start.year() < EARLIEST_HISTORY_YEAR {
            return Err(invalid(format!(
                "history_start {history_start} is before {EARLIEST_HISTORY_YEAR}-01-01"
            )));
        }

        let bars_dir = check_path("bars_dir", &raw.bars_dir)?;
        let limits_path = check_path("limits_path", &raw.limits_path)?;

        if raw.source.trim().is_empty() {
            return Err(invalid("source must not be empty"));
        }

        Ok(Self {
            strategy: raw.strategy,
            symbols,
            benchmark,
            feed,
            history_start,
            bars_dir,
            limits_path,
            seed: raw
                .seed
                .as_u64()
                .ok_or_else(|| invalid("seed must be a non-negative integer (u64)"))?,
            source: raw.source,
        })
    }

    /// Reads and parses the file. An unreadable file is `InvalidConfig` naming the path.
    pub fn load(path: &Path) -> Result<Self, AppError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| invalid(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text)
    }

    /// Every symbol whose bars are fetched: the universe plus the benchmark, sorted, no duplicate.
    #[must_use]
    pub fn fetch_symbols(&self) -> Vec<Symbol> {
        let mut out = self.symbols.clone();
        if !out.contains(&self.benchmark) {
            out.push(self.benchmark);
        }
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        out
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    const VALID: &str = include_str!("../../../config/engine/paper.json");

    fn valid_value() -> Value {
        serde_json::from_str(VALID).expect("the repository config is valid JSON")
    }

    /// The valid config with one edit applied, serialized back to text.
    fn edited(edit: impl FnOnce(&mut Value)) -> String {
        let mut value = valid_value();
        edit(&mut value);
        value.to_string()
    }

    /// Parses the valid config with one edit applied and returns the error message. Panics if it
    /// is accepted.
    fn refused(edit: impl FnOnce(&mut Value)) -> String {
        match EngineConfig::parse(&edited(edit)) {
            Ok(config) => panic!("expected a refusal, got {config:?}"),
            Err(AppError::InvalidConfig(message)) => message,
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    /// Parses the valid config with one edit applied. Panics if it is refused.
    fn accepted(edit: impl FnOnce(&mut Value)) -> EngineConfig {
        match EngineConfig::parse(&edited(edit)) {
            Ok(config) => config,
            Err(error) => panic!("expected acceptance, got {error}"),
        }
    }

    fn symbol_names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(Symbol::as_str).collect()
    }

    #[test]
    fn the_repository_config_parses() {
        let config = EngineConfig::parse(VALID).expect("repository config parses");
        assert_eq!(
            symbol_names(&config.symbols),
            ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"]
        );
        assert_eq!(config.benchmark.as_str(), "SPY");
        assert_eq!(config.feed, Feed::Iex);
        assert_eq!(
            config.history_start,
            NaiveDate::from_ymd_opt(2015, 1, 1).expect("valid date")
        );
        assert_eq!(config.bars_dir, PathBuf::from("data/bars"));
        assert_eq!(
            config.limits_path,
            PathBuf::from("config/limits/limits.json")
        );
        assert_eq!(config.seed, 0);
        assert_eq!(config.source, "engine");
        assert_eq!(config.strategy["strategy"], "trend_vol_target");
        assert_eq!(config.fetch_symbols().len(), 8);
    }

    #[test]
    fn load_reads_the_repository_config() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/engine/paper.json"
        );
        let config = EngineConfig::load(Path::new(path)).expect("repository config loads");
        assert_eq!(config.symbols.len(), 8);
    }

    #[test]
    fn unknown_field_is_refused() {
        let message = refused(|v| v["extra"] = json!(1));
        assert!(message.contains("extra"), "{message}");
    }

    #[test]
    fn missing_required_fields_are_refused() {
        let required = [
            "environment",
            "strategy",
            "symbols",
            "benchmark",
            "feed",
            "history_start",
            "bars_dir",
            "limits_path",
            "seed",
        ];
        for key in required {
            let message = refused(|v| {
                v.as_object_mut().expect("object").remove(key);
            });
            assert!(message.contains(key), "{key}: {message}");
        }
    }

    #[test]
    fn source_may_be_omitted_and_defaults_to_engine() {
        let config = accepted(|v| {
            v.as_object_mut().expect("object").remove("source");
        });
        assert_eq!(config.source, "engine");
    }

    #[test]
    fn empty_source_is_refused() {
        let message = refused(|v| v["source"] = json!("  "));
        assert!(message.contains("source"), "{message}");
    }

    #[test]
    fn live_environment_is_refused() {
        let message = refused(|v| v["environment"] = json!("live"));
        assert!(message.contains("paper"), "{message}");
        assert!(message.contains("environment"), "{message}");
    }

    #[test]
    fn symbols_are_sorted_whatever_the_input_order() {
        let config = accepted(|v| {
            v["symbols"] = json!(["VNQ", "DBC", "SPY", "EEM", "EFA", "GLD", "IEF", "TLT"]);
        });
        assert_eq!(
            symbol_names(&config.symbols),
            ["DBC", "EEM", "EFA", "GLD", "IEF", "SPY", "TLT", "VNQ"]
        );
    }

    #[test]
    fn duplicate_symbol_is_refused() {
        let message = refused(|v| v["symbols"] = json!(["SPY", "DBC", "SPY"]));
        assert!(message.contains("duplicate"), "{message}");
        assert!(message.contains("SPY"), "{message}");
    }

    #[test]
    fn lower_case_symbol_is_refused() {
        let message = refused(|v| v["symbols"] = json!(["spy"]));
        assert!(message.contains("spy"), "{message}");
    }

    #[test]
    fn malformed_symbols_are_refused() {
        for bad in ["", "BRK/B", "A B", "TOOLONGSYMBOLX"] {
            let message = refused(|v| v["symbols"] = json!([bad]));
            assert!(message.contains("symbol"), "{bad:?}: {message}");
        }
    }

    #[test]
    fn dot_and_digits_are_allowed_in_symbols() {
        let config = accepted(|v| v["symbols"] = json!(["BRK.B", "SPY1"]));
        assert_eq!(symbol_names(&config.symbols), ["BRK.B", "SPY1"]);
    }

    #[test]
    fn empty_symbol_list_is_refused() {
        let message = refused(|v| v["symbols"] = json!([]));
        assert!(message.contains("symbols"), "{message}");
    }

    #[test]
    fn benchmark_outside_universe_is_fetched_once_and_sorted() {
        let config = accepted(|v| v["benchmark"] = json!("QQQ"));
        assert_eq!(config.benchmark.as_str(), "QQQ");
        let fetched = config.fetch_symbols();
        assert_eq!(
            symbol_names(&fetched),
            [
                "DBC", "EEM", "EFA", "GLD", "IEF", "QQQ", "SPY", "TLT", "VNQ"
            ]
        );
    }

    #[test]
    fn benchmark_inside_universe_is_not_repeated() {
        let config = EngineConfig::parse(VALID).expect("repository config parses");
        let fetched = config.fetch_symbols();
        assert_eq!(fetched.iter().filter(|s| s.as_str() == "SPY").count(), 1);
    }

    #[test]
    fn malformed_benchmark_is_refused() {
        let message = refused(|v| v["benchmark"] = json!("spy"));
        assert!(message.contains("benchmark"), "{message}");
    }

    #[test]
    fn bad_feed_is_refused() {
        let message = refused(|v| v["feed"] = json!("nasdaq"));
        assert!(message.contains("feed"), "{message}");
        assert!(message.contains("nasdaq"), "{message}");
    }

    #[test]
    fn sip_feed_is_accepted() {
        let config = accepted(|v| v["feed"] = json!("sip"));
        assert_eq!(config.feed, Feed::Sip);
    }

    #[test]
    fn bad_history_start_is_refused() {
        for bad in ["2015-13-01", "01/02/2015", "2015-1-5", ""] {
            let message = refused(|v| v["history_start"] = json!(bad));
            assert!(message.contains("history_start"), "{bad:?}: {message}");
        }
    }

    #[test]
    fn history_start_before_2000_is_refused() {
        let message = refused(|v| v["history_start"] = json!("1999-12-31"));
        assert!(message.contains("history_start"), "{message}");
    }

    #[test]
    fn history_start_on_2000_01_01_is_accepted() {
        let config = accepted(|v| v["history_start"] = json!("2000-01-01"));
        assert_eq!(
            config.history_start,
            NaiveDate::from_ymd_opt(2000, 1, 1).expect("valid date")
        );
    }

    #[test]
    fn empty_bars_dir_is_refused() {
        let message = refused(|v| v["bars_dir"] = json!(""));
        assert!(message.contains("bars_dir"), "{message}");
    }

    #[test]
    fn empty_limits_path_is_refused() {
        let message = refused(|v| v["limits_path"] = json!(""));
        assert!(message.contains("limits_path"), "{message}");
    }

    #[test]
    fn negative_or_string_seed_is_refused() {
        let message = refused(|v| v["seed"] = json!(-1));
        assert!(message.contains("seed"), "{message}");
        let message = refused(|v| v["seed"] = json!("0"));
        assert!(message.contains("seed"), "{message}");
    }

    #[test]
    fn unknown_strategy_name_is_refused() {
        let message = refused(|v| v["strategy"]["strategy"] = json!("no_such_strategy"));
        assert!(message.contains("strategy"), "{message}");
        assert!(message.contains("no_such_strategy"), "{message}");
    }

    #[test]
    fn non_object_strategy_is_refused() {
        let message = refused(|v| v["strategy"] = json!("trend_vol_target"));
        assert!(message.contains("strategy"), "{message}");
        let message = refused(|v| v["strategy"] = json!(["trend_vol_target"]));
        assert!(message.contains("strategy"), "{message}");
    }

    #[test]
    fn load_of_missing_path_names_the_path() {
        let path = Path::new("/nonexistent/nisse-test/engine.json");
        match EngineConfig::load(path) {
            Ok(config) => panic!("expected an error, got {config:?}"),
            Err(AppError::InvalidConfig(message)) => {
                assert!(
                    message.contains("/nonexistent/nisse-test/engine.json"),
                    "{message}"
                );
            }
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
        }
    }
}
