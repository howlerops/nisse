//! Deterministic trading engine: strategy, portfolio and order state on one code path for
//! backtest and live.
//!
//! Rules that hold for everything in this crate: no LLM or network call in the order path, and
//! time and randomness only from injected sources, so the same features, config hash, seed and
//! git SHA reproduce the same decision. Phase 0 is scaffold only; the engine actor arrives in
//! Phase 2.

mod error;

pub use error::EngineError;
