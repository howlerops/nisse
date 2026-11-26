//! Writing bars to a Parquet file.

use std::path::Path;

use nisse_core::nautilus::Bar;

use crate::BarsError;

/// Writes `bars` to the Parquet file at `path`, sorted by symbol then time, creating the
/// parent directory if needed.
///
/// The file is written under a temporary name in the same directory and renamed into place,
/// so a reader never sees a half-written file and a failed write leaves nothing behind. An
/// existing file at `path` is an error (`Data`): bar files are written once, and a correction
/// is a new file. An empty slice writes nothing and returns `Ok`.
#[allow(unused_variables, reason = "stub: Phase 3 task B1")]
pub fn write_bars(path: &Path, bars: &[Bar]) -> Result<(), BarsError> {
    todo!("Phase 3 task B1")
}
