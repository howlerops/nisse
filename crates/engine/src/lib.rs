//! Deterministic trading engine: strategy, portfolio and order state on one code path for
//! backtest and live.
//!
//! Rules that hold for everything in this crate: no LLM or network call in the order path, and
//! time and randomness only from injected sources, so the same features, config hash, seed and
//! git SHA reproduce the same decision. The broker is reached only through the
//! [`nisse_core::Broker`] trait.

/// The git commit this build was made from: 40 lowercase hex characters (all zeros when the
/// build could not tell; see `build.rs`). Stored in `strategy_version.git_sha`.
pub const GIT_SHA: &str = env!("NISSE_GIT_SHA");

mod decide;
mod error;
mod gateway;
mod order_service;
mod order_tracker;
mod positions;
mod reconcile;
mod strategy;
mod sync;

pub use decide::{DecisionContext, ReplayOutcome, decide, decide_and_record, replay};
pub use error::{BoxError, EngineError};
pub use gateway::{OrderGateway, Submitted};
pub use order_service::{OrderBookState, reconcile_and_record, record_stream_update};
pub use order_tracker::{Applied, OrderTracker, TrackedOrder};
pub use positions::{Position, PositionBook};
pub use reconcile::{Finding, reconcile};
pub use strategy::{Strategy, Verdict};
pub use sync::{StateChange, SyncOptions, SyncProblem, SyncReport, sync_with_broker};
