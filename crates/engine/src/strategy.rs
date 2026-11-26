//! The strategy interface.
//!
//! A strategy's decision is a pure function of the features it is shown, its own parameters
//! (fixed when it is built) and a seed. Nothing else: no clock, no network, no global state,
//! no map with random iteration order. That is what lets a decision be replayed later, from
//! the stored features, the stored parameters and the stored seed, and checked against what
//! was recorded.

use nisse_core::{Action, Features};
use rust_decimal::Decimal;

/// What a strategy decided for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub action: Action,
    /// Signed change in shares: positive buys, negative sells. Zero for `Hold` and `Skip`.
    pub intended_qty: Decimal,
    pub expected_edge_bps: Option<f64>,
    pub expected_vol: Option<f64>,
    /// A short, stable, machine-readable reason (`trend_up`, `below_min_history`).
    pub reason_code: &'static str,
}

/// A trading strategy. Implementations hold their parameters and nothing mutable.
pub trait Strategy: Send + Sync {
    /// The name stored in `strategy_version.params["strategy"]`, which the registry uses to
    /// rebuild the strategy for replay.
    fn name(&self) -> &'static str;

    /// Decides for one symbol. Everything the decision depends on, including the current
    /// position, must be in `features`. Must be deterministic.
    fn decide(&self, features: &Features, seed: u64) -> Verdict;
}

impl<S: Strategy + ?Sized> Strategy for Box<S> {
    fn name(&self) -> &'static str {
        (**self).name()
    }

    fn decide(&self, features: &Features, seed: u64) -> Verdict {
        (**self).decide(features, seed)
    }
}
