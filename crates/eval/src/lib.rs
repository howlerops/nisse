//! Nightly evaluation jobs: the replay test now; outcome windows, rollups and the promotion
//! gates (deflated Sharpe, PBO) in Phase 5.
//!
//! Every evaluation run registers a trial in `trial_registry`.

pub mod backtest_job;
mod error;
pub mod replay;

pub use error::EvalError;
