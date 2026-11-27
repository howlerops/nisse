//! The engine process: short-lived commands run from cron (`nisse-engine <command>`), each of
//! which rebuilds its state from the store and the bar files, does one thing and exits.
//!
//! This is the only crate that depends on both the engine and the broker adapter. Design and
//! the exact steps of every command are in `docs/phases/phase-6.md`. No LLM is called
//! anywhere here, and the broker credentials are read from the environment by the binary
//! only.

pub mod commands;
pub mod config;
pub mod deployment;
mod error;
pub mod ingest;
pub mod kill_watch;
pub mod market;
pub mod restore;
pub mod session;

pub use error::AppError;
