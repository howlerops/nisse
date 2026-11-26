//! Writing bars to a Parquet file.

use std::path::{Path, PathBuf};

use duckdb::{Connection, params};
use nisse_core::nautilus::Bar;
use rust_decimal::Decimal;

use crate::BarsError;

/// Decimal places kept in every stored price and volume (`DECIMAL(28, 9)`).
const STORED_SCALE: u32 = 9;

/// Writes `bars` to the Parquet file at `path`, sorted by symbol then time, creating the
/// parent directory if needed.
///
/// The file is written under a temporary name in the same directory and renamed into place,
/// so a reader never sees a half-written file and a failed write leaves nothing behind. An
/// existing file at `path` is an error (`Data`): bar files are written once, and a correction
/// is a new file. An empty slice writes nothing and returns `Ok`.
pub fn write_bars(path: &Path, bars: &[Bar]) -> Result<(), BarsError> {
    if bars.is_empty() {
        return Ok(());
    }
    sql_path(path)?;
    if path.exists() {
        return Err(BarsError::Data(format!(
            "{} already exists",
            path.display()
        )));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BarsError::Data(format!("{} has no file name", path.display())))?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let tmp: PathBuf = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    let tmp_text = sql_path(&tmp)?;

    let result = write_temporary(tmp_text, bars)
        .and_then(|()| std::fs::rename(&tmp, path).map_err(BarsError::from));
    if result.is_err() {
        // Best effort: the temporary file may not exist yet.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Loads `bars` into an in-memory table and copies it to `tmp_text` as Parquet.
fn write_temporary(tmp_text: &str, bars: &[Bar]) -> Result<(), BarsError> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE bars (symbol VARCHAR, ts_event BIGINT, open DECIMAL(28,9), \
         high DECIMAL(28,9), low DECIMAL(28,9), close DECIMAL(28,9), volume DECIMAL(28,9))",
    )?;
    conn.execute_batch("BEGIN TRANSACTION")?;
    {
        let mut appender = conn.appender("bars")?;
        for bar in bars {
            let symbol = bar.bar_type.instrument_id().symbol;
            let ts_event = i64::try_from(bar.ts_event.as_u64()).map_err(|_| {
                BarsError::Data(format!("ts_event {} out of range", bar.ts_event.as_u64()))
            })?;
            appender.append_row(params![
                symbol.as_str(),
                ts_event,
                stored(bar.open.as_decimal())?,
                stored(bar.high.as_decimal())?,
                stored(bar.low.as_decimal())?,
                stored(bar.close.as_decimal())?,
                stored(bar.volume.as_decimal())?,
            ])?;
        }
        appender.flush()?;
    }
    conn.execute_batch("COMMIT")?;
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM bars ORDER BY symbol, ts_event) TO '{tmp_text}' (FORMAT PARQUET)"
    ))?;
    Ok(())
}

/// Rejects a value that DuckDB could not store exactly in `DECIMAL(28, 9)`.
fn stored(value: Decimal) -> Result<Decimal, BarsError> {
    if value.scale() > STORED_SCALE {
        return Err(BarsError::Data(format!(
            "{value} has more than {STORED_SCALE} decimal places"
        )));
    }
    Ok(value)
}

/// `path` as the text of a SQL string literal: valid UTF-8 and free of single quotes
/// and glob characters.
pub(crate) fn sql_path(path: &Path) -> Result<&str, BarsError> {
    let text = path
        .to_str()
        .ok_or_else(|| BarsError::Data(format!("{} is not valid UTF-8", path.display())))?;
    // A quote would end the SQL literal; the others are glob characters, which would change
    // which files `read_parquet` matches.
    if text.contains(['\'', '*', '?', '[', ']', '{', '}']) {
        return Err(BarsError::Data(format!(
            "{text} contains a quote or a glob character"
        )));
    }
    Ok(text)
}
