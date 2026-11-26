//! Nightly evaluation jobs: outcome windows, implementation shortfall, strategy rollups and
//! the promotion gates (deflated Sharpe, PBO).
//!
//! Every evaluation run registers a trial in `trial_registry`. Phase 0 is scaffold only; the
//! jobs are built in Phase 5.

mod error;

pub use error::EvalError;
