//! Alpaca market data and execution adapter (bars, quotes, orders, fills, reconciliation).
//!
//! This is a sensitive path: call out every change here in the PR description. Broker
//! credentials live only in the environment of the engine host and never in this repo,
//! including tests, which run against a local mock server with placeholder values.
//!
//! The adapter only speaks Alpaca. Broker-agnostic order logic (tracking, reconciliation,
//! the write-ahead submit path) lives in `nisse-engine` and reaches this crate through the
//! [`nisse_core::Broker`] trait.

mod calendar;
mod config;
mod data;
mod error;
mod http;
mod parse;
mod stream;
mod trading;
mod wire;

pub use calendar::fetch_calendar;
pub use config::{AlpacaConfig, Credentials, ENV_KEY_ID, ENV_SECRET_KEY, Feed};
pub use data::{AlpacaData, VENUE};
pub use error::AdapterError;
pub use http::AlpacaHttp;
pub use stream::{MAX_RECONNECT_DELAY, MIN_RECONNECT_DELAY, StreamEvent, run_order_stream};
pub use trading::AlpacaTrading;
