//! Append-only Postgres decision store.
//!
//! The schema is in `/migrations`. Every table has `event_ts` and `ingest_ts`, and rows are
//! only ever inserted: a correction is a new row. Phase 0 ships the schema only; the writer is
//! built in Phase 2.

mod error;

pub use error::StoreError;
