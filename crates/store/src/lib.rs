//! Append-only Postgres decision store.
//!
//! The schema is in `/migrations`. Every table has `event_ts` and `ingest_ts`, and rows are
//! only ever inserted: a correction is a new row. This crate never issues `UPDATE`, `DELETE`
//! or `TRUNCATE`, and in production it connects as a role that could not if it tried.
//!
//! Two write paths:
//! - [`Store`]: awaited writes the engine cannot proceed without (a decision, whose id orders
//!   refer to; the write-ahead order intent).
//! - [`StoreWriter`]: everything else, through a bounded queue to a writer task that batches
//!   rows into transactions, so the engine never waits on the database for them.

#![allow(
    dead_code,
    reason = "Phase 2 in progress: stubs call nothing yet. LEAD: remove before the PR."
)]

mod db;
mod error;
mod store;
pub mod tables;
#[cfg(feature = "test-support")]
pub mod testing;
mod writer;

pub use db::connect;
pub use error::StoreError;
pub use store::Store;
pub use writer::{StoreWriter, WriterConfig};
