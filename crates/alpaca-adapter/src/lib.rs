//! Alpaca market data and execution adapter (bars, quotes, orders, fills, reconciliation).
//!
//! Changes here need CODEOWNERS review. Broker credentials live only on the engine host and
//! never in this repo, including tests. Phase 0 is scaffold only; the clients are built in
//! Phase 1.

mod error;

pub use error::AdapterError;
