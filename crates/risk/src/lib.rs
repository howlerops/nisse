//! In-process risk gate: limits, drawdown step-down, stale-data watchdog and kill switch.
//!
//! Changes here need CODEOWNERS review. The gate fails closed. Phase 0 is scaffold only; the
//! gate is built in Phase 4.

mod error;

pub use error::RiskError;
