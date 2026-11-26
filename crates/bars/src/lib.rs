//! Daily bar storage: Parquet files, written and read through DuckDB.
//!
//! Bars are never loaded by reading whole files into memory. A read is one DuckDB query over
//! `read_parquet`, selecting only the columns and rows asked for, so DuckDB skips the row
//! groups and columns the query does not need (projection and predicate pushdown).
//!
//! File layout: one directory of `*.parquet` files, each written once and never modified.
//! Columns: `symbol VARCHAR`, `ts_event BIGINT` (UTC nanoseconds, the session close),
//! `open`, `high`, `low`, `close` and `volume` as `DECIMAL(28, 9)`. Prices are exact decimals
//! from the broker's text all the way into the file; nothing passes through a float.

mod error;
mod reader;
mod writer;

pub use error::BarsError;
pub use reader::BarReader;
pub use writer::write_bars;

/// Venue code in the instrument id of every bar this crate returns (`SPY.ALPACA`), matching
/// the adapter that produced them.
pub const VENUE: &str = "ALPACA";
