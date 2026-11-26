//! The strategies this build knows, and the registry that rebuilds one from the parameters
//! stored in `strategy_version.params` (used by the engine at start-up and by replay).

mod trend;

pub use trend::{TREND_VOL_TARGET, TrendParams, TrendVolTarget, feature};

use serde_json::Value;

use crate::{EngineError, Strategy, features::WindowSpec};

/// A strategy rebuilt from stored parameters, with what the engine must set up around it.
pub struct BuiltStrategy {
    pub strategy: Box<dyn Strategy>,
    /// The rolling-window sizes the trader must maintain for it.
    pub window: WindowSpec,
    /// Sessions per year the strategy annualises with; reports use the same figure.
    pub sessions_per_year: u32,
}

/// Builds the strategy named by `params["strategy"]`. Errors with `UnknownStrategy` for a
/// missing or unrecognised name and `InvalidConfig` for parameters that name a known
/// strategy but do not parse.
pub fn build(params: &Value) -> Result<BuiltStrategy, EngineError> {
    match params.get("strategy").and_then(Value::as_str) {
        Some(TREND_VOL_TARGET) => {
            let strategy = TrendVolTarget::from_params(params)?;
            Ok(BuiltStrategy {
                window: strategy.window_spec(),
                sessions_per_year: strategy.params().sessions_per_year,
                strategy: Box::new(strategy),
            })
        }
        Some(other) => Err(EngineError::UnknownStrategy(other.to_owned())),
        None => Err(EngineError::UnknownStrategy("<missing>".to_owned())),
    }
}
