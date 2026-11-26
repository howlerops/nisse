//! Nightly evaluation jobs: the replay test now; outcome windows, rollups and the promotion
//! gates (deflated Sharpe, PBO) in Phase 5.
//!
//! Every evaluation run registers a trial in `trial_registry`.

pub mod backtest_job;
pub mod deadman;
mod error;
pub mod execution;
pub mod execution_job;
pub mod gates;
pub mod outcome;
pub mod replay;
pub mod score_job;
pub mod stats;

pub use error::EvalError;
