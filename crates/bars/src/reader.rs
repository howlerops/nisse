//! Reading bars back with projection and predicate pushdown.

use std::path::{Path, PathBuf};

use duckdb::ToSql;
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
    PriceType, Quantity, Symbol, UnixNanos, Venue,
};
use rust_decimal::Decimal;

use crate::writer::sql_path;
use crate::{BarsError, VENUE};

/// A handle on a directory of bar files.
#[derive(Debug)]
pub struct BarReader {
    conn: duckdb::Connection,
    /// The directory of bar files; looked at afresh on each read.
    dir: PathBuf,
}

impl BarReader {
    /// Opens `dir` (it must exist). No file is read yet.
    pub fn open(dir: &Path) -> Result<Self, BarsError> {
        if !dir.is_dir() {
            return Err(BarsError::Data(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        sql_path(dir)?;
        let conn = duckdb::Connection::open_in_memory()?;
        Ok(Self {
            conn,
            dir: dir.to_path_buf(),
        })
    }

    /// The daily bars of `symbols` with `start <= ts_event <= end`, sorted by time and then
    /// symbol (the order the engine consumes them in), from **one** file: the `*.parquet`
    /// file whose name sorts last. Files are never merged. Each file is a complete snapshot
    /// fetched at one time from one feed with prices adjusted as of that time, so splicing
    /// rows from two fetches would put a false jump (a dividend, a split) at the join. A
    /// correction or an extension is a new, complete file with a later name. An empty
    /// `symbols` slice, or a directory with no Parquet files, gives an empty vector.
    ///
    /// One query; only the seven columns are read and the symbol and time filters are part of
    /// the scan.
    pub fn read(
        &self,
        symbols: &[&str],
        start: UnixNanos,
        end: UnixNanos,
    ) -> Result<Vec<Bar>, BarsError> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let Some(file) = self.latest_file()? else {
            return Ok(Vec::new());
        };
        let file_text = sql_path(&file)?;
        let lo = i64::try_from(start.as_u64())
            .map_err(|_| BarsError::Data(format!("start {} out of range", start.as_u64())))?;
        let hi = i64::try_from(end.as_u64())
            .map_err(|_| BarsError::Data(format!("end {} out of range", end.as_u64())))?;

        let mut sql = format!(
            "SELECT symbol, ts_event, open, high, low, close, volume \
             FROM read_parquet('{file_text}') WHERE symbol IN ("
        );
        for index in 0..symbols.len() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push_str(") AND ts_event BETWEEN ? AND ? ORDER BY ts_event, symbol");

        let mut params: Vec<&dyn ToSql> = Vec::with_capacity(symbols.len() + 2);
        for symbol in symbols {
            params.push(symbol);
        }
        params.push(&lo);
        params.push(&hi);

        let spec = BarSpecification::new_checked(1, BarAggregation::Day, PriceType::Last)
            .map_err(|e| BarsError::Data(e.to_string()))?;
        let venue = Venue::new_checked(VENUE).map_err(|e| BarsError::Data(e.to_string()))?;

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params.as_slice())?;
        let mut bars = Vec::new();
        while let Some(row) = rows.next()? {
            let symbol: String = row.get(0)?;
            let ts_event: i64 = row.get(1)?;
            let open: Decimal = row.get(2)?;
            let high: Decimal = row.get(3)?;
            let low: Decimal = row.get(4)?;
            let close: Decimal = row.get(5)?;
            let volume: Decimal = row.get(6)?;

            let symbol =
                Symbol::new_checked(&symbol).map_err(|e| BarsError::Data(e.to_string()))?;
            let instrument_id = InstrumentId::new(symbol, venue);
            let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
            let ts = u64::try_from(ts_event)
                .map(UnixNanos::from)
                .map_err(|_| BarsError::Data(format!("stored ts_event {ts_event} is negative")))?;
            bars.push(build_bar(bar_type, [open, high, low, close], volume, ts)?);
        }
        Ok(bars)
    }

    /// The name of the file a `read` uses now (see [`Self::read`]), for recording which
    /// snapshot a run was made from. `None` when the directory has no Parquet file.
    pub fn snapshot_file_name(&self) -> Result<Option<String>, BarsError> {
        Ok(self
            .latest_file()?
            .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned())))
    }

    /// The `*.parquet` file in the directory whose name sorts last, if there is one.
    fn latest_file(&self) -> Result<Option<PathBuf>, BarsError> {
        let mut latest: Option<PathBuf> = None;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            let is_parquet = path.extension().is_some_and(|ext| ext == "parquet");
            if !is_parquet || !entry.file_type()?.is_file() {
                continue;
            }
            if latest
                .as_ref()
                .is_none_or(|best| path.file_name() > best.file_name())
            {
                latest = Some(path);
            }
        }
        Ok(latest)
    }
}

/// A bar from its stored decimals. The four prices share one precision: the largest number of
/// fractional digits any of them needs once trailing zeros are dropped. No value goes through
/// a float.
fn build_bar(
    bar_type: BarType,
    [open, high, low, close]: [Decimal; 4],
    volume: Decimal,
    ts: UnixNanos,
) -> Result<Bar, BarsError> {
    let [open, high, low, close] = [open, high, low, close].map(|d| d.normalize());
    let volume = volume.normalize();
    let digits = [open, high, low, close]
        .iter()
        .map(|d| d.scale())
        .fold(0, u32::max);
    let digits = u8::try_from(digits).map_err(data_error)?;
    let volume_digits = u8::try_from(volume.scale()).map_err(data_error)?;
    Bar::new_checked(
        bar_type,
        price_at(open, digits)?,
        price_at(high, digits)?,
        price_at(low, digits)?,
        price_at(close, digits)?,
        Quantity::from_decimal_dp(volume, volume_digits).map_err(data_error)?,
        ts,
        ts,
    )
    .map_err(data_error)
}

/// `value` as a price with `digits` fractional digits.
fn price_at(value: Decimal, digits: u8) -> Result<Price, BarsError> {
    Price::from_decimal_dp(value, digits).map_err(data_error)
}

/// A `Data` error carrying the message of a Nautilus or conversion error.
fn data_error(e: impl std::fmt::Display) -> BarsError {
    BarsError::Data(e.to_string())
}
