//! Reading bars back with projection and predicate pushdown.

use std::path::Path;

use nisse_core::nautilus::{Bar, UnixNanos};

use crate::BarsError;

/// A handle on a directory of bar files.
#[derive(Debug)]
pub struct BarReader {
    conn: duckdb::Connection,
    /// `<dir>/*.parquet`, as DuckDB's `read_parquet` takes it.
    glob: String,
}

#[allow(unused_variables, reason = "stub: Phase 3 task B1")]
impl BarReader {
    /// Opens `dir` (it must exist). No file is read yet.
    pub fn open(dir: &Path) -> Result<Self, BarsError> {
        todo!("Phase 3 task B1")
    }

    /// The daily bars of `symbols` with `start <= ts_event <= end`, sorted by time and then
    /// symbol (the order the engine consumes them in). If the same `(symbol, ts_event)` is in
    /// more than one file, the bar from the file whose name sorts last wins: a correction is
    /// a new file, named later. An empty `symbols` slice, or a directory with no Parquet
    /// files, gives an empty vector.
    ///
    /// One query; only the seven columns are read and the symbol and time filters are part of
    /// the scan.
    pub fn read(
        &self,
        symbols: &[&str],
        start: UnixNanos,
        end: UnixNanos,
    ) -> Result<Vec<Bar>, BarsError> {
        todo!("Phase 3 task B1")
    }
}
