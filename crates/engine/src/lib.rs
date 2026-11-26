//! Deterministic trading engine: strategy, portfolio and order state on one code path for
//! backtest and live.
//!
//! Rules that hold for everything in this crate: no LLM or network call in the order path, and
//! time and randomness only from injected sources, so the same features, config hash, seed and
//! git SHA reproduce the same decision. The broker is reached only through the
//! [`nisse_core::Broker`] trait.

mod error;
mod gateway;
mod order_tracker;
mod reconcile;
mod sync;

pub use error::{BoxError, EngineError};
pub use gateway::{OrderGateway, Submitted};
pub use order_tracker::{Applied, OrderTracker, TrackedOrder};
pub use reconcile::{Finding, reconcile};
pub use sync::{SyncReport, sync_with_broker};
