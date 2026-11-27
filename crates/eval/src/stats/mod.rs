//! The statistics behind the promotion gates: pure functions on daily simple returns.
//!
//! Definitions and reference values are in `docs/phases/phase-5.md`, "Statistics". Every
//! function returns `None` (or an error) when its input is too short or its result is not
//! finite; nothing here panics and nothing returns NaN. Statistics are `f64`; money never
//! comes through here.

pub mod bootstrap;
pub mod cv;
pub mod dsr;
pub mod moments;
pub mod normal;
pub mod pbo;
pub mod rng;
pub mod tracking;
