//! The strategies this build knows, and the registry that rebuilds one from the parameters
//! stored in `strategy_version.params` (used by the engine at start-up and by replay).

mod trend;

pub use trend::{TREND_VOL_TARGET, TrendParams, TrendVolTarget};

use serde_json::Value;

use crate::{EngineError, Strategy};

/// Builds the strategy named by `params["strategy"]`. Errors with `UnknownStrategy` for a
/// missing or unrecognised name and `InvalidConfig` for parameters that name a known
/// strategy but do not parse.
pub fn build(params: &Value) -> Result<Box<dyn Strategy>, EngineError> {
    match params.get("strategy").and_then(Value::as_str) {
        Some(TREND_VOL_TARGET) => Ok(Box::new(TrendVolTarget::from_params(params)?)),
        Some(other) => Err(EngineError::UnknownStrategy(other.to_owned())),
        None => Err(EngineError::UnknownStrategy("<missing>".to_owned())),
    }
}
