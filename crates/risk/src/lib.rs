//! In-process risk gate: limits, drawdown step-down, day-trade count and kill switch.
//!
//! This crate is a sensitive path (see CLAUDE.md): every change is called out in its PR.
//! It is pure: no I/O beyond reading the limits file, no clock, no async. The engine feeds it
//! facts and it answers. The gate fails closed: anything it cannot evaluate is a failed
//! check, and a failed check means no order. Rules are in `docs/phases/phase-4.md`.

mod day_trades;
mod drawdown;
mod error;
mod gate;
mod limits;

pub use day_trades::{DAY_NANOS, DayTradeTracker, WINDOW_DAYS};
pub use drawdown::{DrawdownMode, DrawdownState, Observation};
pub use error::RiskError;
pub use gate::{GateVerdict, OrderCheck, RiskGate, RiskState, RuleResult, rule};
pub use limits::Limits;
