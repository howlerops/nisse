//! Canonical text forms and their hashes.
//!
//! A decision is replayed from its features, its strategy's parameters and a seed, and the
//! store keeps a hash of each so tampering or drift is detectable. A hash is only stable if
//! the bytes it covers are: same key order, same number formatting, no whitespace. This
//! module defines those bytes once.

use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{CoreError, Hash32};

/// The inputs a strategy decides from: named exact decimals. A `BTreeMap`, so iteration (and
/// therefore hashing and serialisation) is in name order.
pub type Features = BTreeMap<String, Decimal>;

/// JSON with object keys sorted (byte order) at every level and no insignificant whitespace.
/// Strings are escaped as `serde_json` escapes them; numbers are written as `serde_json`
/// writes them. Arrays keep their order.
#[allow(unused_variables, reason = "stub: Phase 2 task C1")]
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    todo!("Phase 2 task C1")
}

/// SHA-256 of [`canonical_json`] of a strategy's parameters (`strategy_version.config_hash`).
#[allow(unused_variables, reason = "stub: Phase 2 task C1")]
#[must_use]
pub fn config_hash(params: &Value) -> Hash32 {
    todo!("Phase 2 task C1")
}

/// Features as a JSON object whose values are decimal **strings** (`{"mom_12m":"0.1"}`), so
/// no value passes through a float on the way into or out of the store. Each decimal is
/// written normalised (no trailing zeros; `-0` is `0`).
#[allow(unused_variables, reason = "stub: Phase 2 task C1")]
#[must_use]
pub fn features_to_json(features: &Features) -> Value {
    todo!("Phase 2 task C1")
}

/// The inverse of [`features_to_json`]. Errors with [`CoreError::InvalidFeatures`] unless the
/// value is an object whose every value is a string holding a plain decimal.
#[allow(unused_variables, reason = "stub: Phase 2 task C1")]
pub fn features_from_json(value: &Value) -> Result<Features, CoreError> {
    todo!("Phase 2 task C1")
}

/// SHA-256 of `canonical_json(features_to_json(features))` (`decision.feature_hash`). Two
/// feature sets with equal values hash equally even if the decimals were written differently
/// (`1.50` and `1.5`).
#[allow(unused_variables, reason = "stub: Phase 2 task C1")]
#[must_use]
pub fn feature_hash(features: &Features) -> Hash32 {
    todo!("Phase 2 task C1")
}
