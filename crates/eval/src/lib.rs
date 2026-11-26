//! Nightly evaluation jobs: the replay test now; outcome windows, rollups and the promotion
//! gates (deflated Sharpe, PBO) in Phase 5.
//!
//! Every evaluation run registers a trial in `trial_registry`.

pub mod backtest_job;
pub mod deadman;
mod error;
pub mod gates;
pub mod replay;
pub mod stats;

pub use error::EvalError;
