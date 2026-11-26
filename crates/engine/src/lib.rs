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

/// Whether this build can stand behind [`GIT_SHA`]: the commit is known and the tree it was
/// built from had no uncommitted changes to tracked files. Decisions made by any other build
/// could not be replayed from that commit, so a process that trades must refuse to start when
/// this is `false`. (The flag is computed when the build script last ran; release builds are
/// made from a fresh checkout.)
#[must_use]
pub fn build_is_replayable() -> bool {
    env!("NISSE_GIT_DIRTY") == "0" && GIT_SHA.bytes().any(|b| b != b'0')
}

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
pub use order_service::{
    OrderBookState, reconcile_and_record, record_stream_update, submit_and_record,
};
pub use order_tracker::{Applied, OrderTracker, TrackedOrder};
pub use positions::{Position, PositionBook};
pub use reconcile::{Finding, reconcile};
pub use strategy::{Strategy, Verdict};
pub use sync::{StateChange, SyncOptions, SyncProblem, SyncReport, sync_with_broker};
