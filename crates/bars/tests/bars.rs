#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::path::{Path, PathBuf};
use std::str::FromStr;

use duckdb::params;
use nisse_bars::{BarReader, BarsError, write_bars};
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
    PriceType, Quantity, Symbol, UnixNanos, Venue,
};
use rust_decimal::Decimal;

/// 2022-01-01 21:00:00 UTC in nanoseconds: the session close of day 1.
const DAY_ONE_CLOSE: u64 = 1_641_070_800_000_000_000;
const DAY_NANOS: u64 = 86_400_000_000_000;
/// The whole range of `ts_event` values a `BIGINT` can hold.
const ALL_FROM: u64 = 0;
const ALL_TO: u64 = i64::MAX as u64;

/// A scratch directory under the system temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("nisse-bars-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("decimal literal")
}

/// A bar of `symbol` on January `day` 2022, at 21:00 UTC, with prices from text.
fn bar(symbol: &str, day: u32, o: &str, h: &str, l: &str, c: &str, v: &str) -> Bar {
    bar_at(symbol, day_ts(day), o, h, l, c, v)
}

/// A bar of `symbol` at `ts` nanoseconds. The four prices share the largest number of
/// decimal places any of them is written with.
fn bar_at(symbol: &str, ts: u64, o: &str, h: &str, l: &str, c: &str, v: &str) -> Bar {
    let spec = BarSpecification::new_checked(1, BarAggregation::Day, PriceType::Last).unwrap();
    let venue = Venue::new_checked("ALPACA").unwrap();
    let instrument = InstrumentId::new(Symbol::new_checked(symbol).unwrap(), venue);
    let bar_type = BarType::new(instrument, spec, AggregationSource::External);
    let prices = [dec(o), dec(h), dec(l), dec(c)];
    let digits = prices.iter().map(|d| d.scale()).max().unwrap_or(0);
    let digits = u8::try_from(digits).unwrap();
    let volume = dec(v);
    let price = |d: Decimal| Price::from_decimal_dp(d, digits).unwrap();
    Bar::new_checked(
        bar_type,
        price(prices[0]),
        price(prices[1]),
        price(prices[2]),
        price(prices[3]),
        Quantity::from_decimal_dp(volume, u8::try_from(volume.scale()).unwrap()).unwrap(),
        UnixNanos::from(ts),
        UnixNanos::from(ts),
    )
    .unwrap()
}

/// A bar whose four prices are all `close` and whose volume is 1.
fn flat(symbol: &str, day: u32, close: &str) -> Bar {
    bar(symbol, day, close, close, close, close, "1")
}

fn all_range() -> (UnixNanos, UnixNanos) {
    (UnixNanos::from(ALL_FROM), UnixNanos::from(ALL_TO))
}

fn day_ts(day: u32) -> u64 {
    DAY_ONE_CLOSE + u64::from(day - 1) * DAY_NANOS
}

/// Asserts that `got` has the same instrument, time, prices and volume as `want`.
fn assert_same(got: &Bar, want: &Bar) {
    assert_eq!(got.bar_type, want.bar_type, "bar_type");
    assert_eq!(got.ts_event, want.ts_event, "ts_event");
    assert_eq!(got.ts_init, got.ts_event, "ts_init must equal ts_event");
    assert_eq!(got.open.as_decimal(), want.open.as_decimal(), "open");
    assert_eq!(got.high.as_decimal(), want.high.as_decimal(), "high");
    assert_eq!(got.low.as_decimal(), want.low.as_decimal(), "low");
    assert_eq!(got.close.as_decimal(), want.close.as_decimal(), "close");
    assert_eq!(got.volume.as_decimal(), want.volume.as_decimal(), "volume");
}

fn symbol_of(bar: &Bar) -> String {
    bar.bar_type.instrument_id().symbol.to_string()
}

#[test]
fn base_constant_is_2022_01_01_21h_utc() {
    let expected = chrono::NaiveDate::from_ymd_opt(2022, 1, 1)
        .unwrap()
        .and_hms_opt(21, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_nanos_opt()
        .unwrap();
    assert_eq!(i64::try_from(DAY_ONE_CLOSE).unwrap(), expected);
}

#[test]
fn round_trip_returns_scrambled_bars_sorted_by_time_then_symbol() {
    let dir = Scratch::new("round_trip");
    let file = dir.path().join("bars.parquet");
    let input = vec![
        bar("CCC", 3, "5.5", "6.0", "5.0", "5.75", "700"),
        bar(
            "AAA",
            2,
            "178.235733",
            "179.5",
            "177.1",
            "178.235733",
            "12345.678",
        ),
        bar("BBB", 1, "9.123456789", "9.2", "9.1", "9.123456789", "1"),
        bar("AAA", 1, "0.1", "0.1", "0.1", "0.1", "1000"),
        bar("CCC", 1, "5.0", "5.5", "4.9", "5.25", "42"),
        bar("BBB", 3, "9.5", "9.6", "9.4", "9.55", "3"),
        bar("AAA", 3, "180", "181", "179", "180.5", "0"),
        bar("CCC", 2, "5.1", "5.2", "5.0", "5.15", "8"),
        bar("BBB", 2, "9.3", "9.4", "9.2", "9.35", "2"),
    ];
    write_bars(&file, &input).unwrap();

    let (from, to) = all_range();
    let got = BarReader::open(dir.path())
        .unwrap()
        .read(&["AAA", "BBB", "CCC"], from, to)
        .unwrap();

    let want = vec![
        bar("AAA", 1, "0.1", "0.1", "0.1", "0.1", "1000"),
        bar("BBB", 1, "9.123456789", "9.2", "9.1", "9.123456789", "1"),
        bar("CCC", 1, "5.0", "5.5", "4.9", "5.25", "42"),
        bar(
            "AAA",
            2,
            "178.235733",
            "179.5",
            "177.1",
            "178.235733",
            "12345.678",
        ),
        bar("BBB", 2, "9.3", "9.4", "9.2", "9.35", "2"),
        bar("CCC", 2, "5.1", "5.2", "5.0", "5.15", "8"),
        bar("AAA", 3, "180", "181", "179", "180.5", "0"),
        bar("BBB", 3, "9.5", "9.6", "9.4", "9.55", "3"),
        bar("CCC", 3, "5.5", "6.0", "5.0", "5.75", "700"),
    ];
    assert_eq!(got.len(), want.len());
    for (got, want) in got.iter().zip(&want) {
        assert_same(got, want);
    }
    assert_eq!(
        got[0].bar_type.to_string(),
        "AAA.ALPACA-1-DAY-LAST-EXTERNAL"
    );
}

#[test]
fn exact_values_survive_the_round_trip_without_floats() {
    let dir = Scratch::new("exact_values");
    let file = dir.path().join("bars.parquet");
    write_bars(&file, &[bar("AAA", 1, "0.1", "0.1", "0.1", "0.1", "0.1")]).unwrap();
    let (from, to) = all_range();
    let got = BarReader::open(dir.path())
        .unwrap()
        .read(&["AAA"], from, to)
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].close.as_decimal(), dec("0.1"));
    assert_eq!(got[0].volume.as_decimal(), dec("0.1"));
    assert_eq!(got[0].close.as_decimal().to_string(), "0.1");
}

#[test]
fn symbol_filter_returns_only_requested_symbols() {
    let dir = Scratch::new("symbol_filter");
    write_bars(
        &dir.path().join("bars.parquet"),
        &[
            flat("AAA", 1, "1.0"),
            flat("BBB", 1, "2.0"),
            flat("CCC", 1, "3.0"),
            flat("BBB", 2, "2.5"),
        ],
    )
    .unwrap();
    let (from, to) = all_range();
    let reader = BarReader::open(dir.path()).unwrap();

    let got = reader.read(&["BBB"], from, to).unwrap();
    assert_eq!(got.len(), 2);
    assert_same(&got[0], &flat("BBB", 1, "2.0"));
    assert_same(&got[1], &flat("BBB", 2, "2.5"));

    let got = reader.read(&["CCC", "AAA"], from, to).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(symbol_of(&got[0]), "AAA");
    assert_eq!(symbol_of(&got[1]), "CCC");

    assert!(reader.read(&["ZZZ"], from, to).unwrap().is_empty());
}

#[test]
fn time_bounds_are_inclusive_at_both_ends() {
    let dir = Scratch::new("time_bounds");
    write_bars(
        &dir.path().join("bars.parquet"),
        &[
            flat("AAA", 1, "1.0"),
            flat("AAA", 2, "2.0"),
            flat("AAA", 3, "3.0"),
        ],
    )
    .unwrap();
    let reader = BarReader::open(dir.path()).unwrap();
    let ts = |day| UnixNanos::from(day_ts(day));

    let got = reader.read(&["AAA"], ts(2), ts(3)).unwrap();
    assert_eq!(got.len(), 2, "bars exactly at start and end are returned");
    assert_same(&got[0], &flat("AAA", 2, "2.0"));
    assert_same(&got[1], &flat("AAA", 3, "3.0"));

    let just_after_day_two = UnixNanos::from(day_ts(2) + 1);
    let got = reader.read(&["AAA"], just_after_day_two, ts(3)).unwrap();
    assert_eq!(
        got.len(),
        1,
        "one nanosecond after start excludes the start bar"
    );
    assert_same(&got[0], &flat("AAA", 3, "3.0"));

    let just_before_day_three = UnixNanos::from(day_ts(3) - 1);
    let got = reader.read(&["AAA"], ts(1), just_before_day_three).unwrap();
    assert_eq!(
        got.len(),
        2,
        "one nanosecond before end excludes the end bar"
    );
    assert_same(&got[1], &flat("AAA", 2, "2.0"));

    assert!(reader.read(&["AAA"], ts(3), ts(2)).unwrap().is_empty());
}

#[test]
fn empty_symbols_gives_empty_result() {
    let dir = Scratch::new("empty_symbols");
    write_bars(&dir.path().join("bars.parquet"), &[flat("AAA", 1, "1.0")]).unwrap();
    let (from, to) = all_range();
    let reader = BarReader::open(dir.path()).unwrap();
    assert!(reader.read(&[], from, to).unwrap().is_empty());
}

#[test]
fn empty_directory_gives_empty_result() {
    let dir = Scratch::new("empty_directory");
    let reader = BarReader::open(dir.path()).unwrap();
    let (from, to) = all_range();
    assert!(reader.read(&["AAA"], from, to).unwrap().is_empty());
}

#[test]
fn directory_with_no_parquet_file_gives_empty_result() {
    let dir = Scratch::new("no_parquet");
    std::fs::write(dir.path().join("notes.txt"), "not a bar file").unwrap();
    let reader = BarReader::open(dir.path()).unwrap();
    let (from, to) = all_range();
    assert!(reader.read(&["AAA"], from, to).unwrap().is_empty());
}

#[test]
fn open_on_missing_directory_is_an_error() {
    let dir = Scratch::new("missing_parent");
    let missing = dir.path().join("does-not-exist");
    assert!(matches!(BarReader::open(&missing), Err(BarsError::Data(_))));
}

#[test]
fn open_on_a_file_is_an_error() {
    let dir = Scratch::new("open_on_file");
    let file = dir.path().join("bars.parquet");
    write_bars(&file, &[flat("AAA", 1, "1.0")]).unwrap();
    assert!(matches!(BarReader::open(&file), Err(BarsError::Data(_))));
}

#[test]
fn start_out_of_range_is_an_error() {
    let dir = Scratch::new("start_out_of_range");
    write_bars(&dir.path().join("bars.parquet"), &[flat("AAA", 1, "1.0")]).unwrap();
    let reader = BarReader::open(dir.path()).unwrap();
    let too_big = UnixNanos::from(u64::MAX);
    assert!(matches!(
        reader.read(&["AAA"], too_big, too_big),
        Err(BarsError::Data(_))
    ));
}

#[test]
fn only_the_file_whose_name_sorts_last_is_read() {
    let dir = Scratch::new("snapshots");
    // Written first, but "b" sorts after "a": it is the snapshot that is read, whole. Rows
    // from "a" are never spliced in, even for a day "b" does not have.
    write_bars(
        &dir.path().join("b.parquet"),
        &[flat("AAA", 1, "20.0"), flat("BBB", 1, "7.0")],
    )
    .unwrap();
    write_bars(
        &dir.path().join("a.parquet"),
        &[flat("AAA", 1, "10.0"), flat("AAA", 2, "11.0")],
    )
    .unwrap();

    let (from, to) = all_range();
    let got = BarReader::open(dir.path())
        .unwrap()
        .read(&["AAA", "BBB"], from, to)
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_same(&got[0], &flat("AAA", 1, "20.0"));
    assert_same(&got[1], &flat("BBB", 1, "7.0"));
}

#[test]
fn writing_to_an_existing_path_is_an_error_and_leaves_the_file_unchanged() {
    let dir = Scratch::new("existing");
    let file = dir.path().join("bars.parquet");
    write_bars(&file, &[flat("AAA", 1, "1.0")]).unwrap();
    let before = std::fs::read(&file).unwrap();

    let err = write_bars(&file, &[flat("AAA", 1, "99.0"), flat("AAA", 2, "99.0")]).unwrap_err();
    assert!(matches!(err, BarsError::Data(_)), "got {err:?}");

    assert_eq!(std::fs::read(&file).unwrap(), before);
    let (from, to) = all_range();
    let got = BarReader::open(dir.path())
        .unwrap()
        .read(&["AAA"], from, to)
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_same(&got[0], &flat("AAA", 1, "1.0"));
}

#[test]
fn no_temporary_file_is_left_after_a_successful_write() {
    let dir = Scratch::new("no_tmp");
    write_bars(&dir.path().join("bars.parquet"), &[flat("AAA", 1, "1.0")]).unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["bars.parquet".to_string()]);
}

#[test]
fn failed_write_leaves_no_temporary_file() {
    let dir = Scratch::new("failed_write");
    // Longer than the file system allows once ".tmp-<pid>" is appended, so DuckDB's COPY to
    // the temporary name fails.
    let name = format!("{}.parquet", "a".repeat(250));
    let file = dir.path().join(name);
    assert!(write_bars(&file, &[flat("AAA", 1, "1.0")]).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_path_with_a_single_quote_is_rejected_without_writing() {
    let dir = Scratch::new("quote");
    let file = dir.path().join("it's.parquet");
    let err = write_bars(&file, &[flat("AAA", 1, "1.0")]).unwrap_err();
    assert!(matches!(err, BarsError::Data(_)), "got {err:?}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_directory_with_a_glob_character_cannot_be_opened() {
    let dir = Scratch::new("glob");
    for name in ["a*b", "a?b", "a[b]", "a{b}"] {
        let sub = dir.path().join(name);
        std::fs::create_dir(&sub).unwrap();
        let err = BarReader::open(&sub).unwrap_err();
        assert!(matches!(err, BarsError::Data(_)), "{name}: got {err:?}");
    }
}

#[test]
fn a_path_that_does_not_end_in_parquet_is_rejected_without_writing() {
    let dir = Scratch::new("suffix");
    for name in ["bars", "bars.csv", "bars.parquet.bak"] {
        let err = write_bars(&dir.path().join(name), &[flat("AAA", 1, "1.0")]).unwrap_err();
        assert!(matches!(err, BarsError::Data(_)), "{name}: got {err:?}");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn empty_slice_writes_no_file() {
    let dir = Scratch::new("empty_slice");
    let file = dir.path().join("nested").join("bars.parquet");
    write_bars(&file, &[]).unwrap();
    assert!(!file.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn write_creates_the_parent_directory() {
    let dir = Scratch::new("parent");
    let file = dir.path().join("nested").join("bars.parquet");
    write_bars(&file, &[flat("AAA", 1, "1.0")]).unwrap();
    assert!(file.is_file());
}

#[test]
fn written_file_has_decimal_prices_and_bigint_timestamps() {
    let dir = Scratch::new("describe");
    let file = dir.path().join("bars.parquet");
    write_bars(&file, &[flat("AAA", 1, "1.0")]).unwrap();

    let conn = duckdb::Connection::open_in_memory().unwrap();
    let sql = format!(
        "DESCRIBE SELECT * FROM read_parquet('{}')",
        file.to_str().unwrap()
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let columns: Vec<(String, String)> = stmt
        .query_map(params![], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let type_of = |name: &str| {
        columns
            .iter()
            .find(|(column, _)| column == name)
            .map(|(_, ty)| ty.as_str())
    };
    assert_eq!(type_of("close"), Some("DECIMAL(28,9)"));
    assert_eq!(type_of("open"), Some("DECIMAL(28,9)"));
    assert_eq!(type_of("volume"), Some("DECIMAL(28,9)"));
    assert_eq!(type_of("ts_event"), Some("BIGINT"));
    assert_eq!(type_of("symbol"), Some("VARCHAR"));
}

/// Deterministic prices for the large round trip: `(open, high, low, close)` of symbol `s`
/// on day `d`, each with four decimal places and a valid OHLC shape.
fn synthetic_prices(s: usize, d: u32) -> [String; 4] {
    let base = 100 + s;
    let frac = (u64::from(d) * 37 + s as u64 * 11) % 10_000;
    [
        format!("{base}.{frac:04}"),
        format!("{}.{frac:04}", base + 1),
        format!("{}.{frac:04}", base - 1),
        format!("{base}.{:04}", (frac + 1) % 10_000),
    ]
}

#[test]
fn twenty_thousand_bars_round_trip() {
    const SYMBOLS: usize = 20;
    const DAYS: u32 = 1000;

    let mut bars = Vec::with_capacity(SYMBOLS * DAYS as usize);
    for d in 1..=DAYS {
        for s in 0..SYMBOLS {
            let symbol = format!("S{s:02}");
            let [o, h, l, c] = synthetic_prices(s, d);
            bars.push(bar_at(&symbol, day_ts(d), &o, &h, &l, &c, &d.to_string()));
        }
    }

    let dir = Scratch::new("twenty_thousand");
    write_bars(&dir.path().join("bars.parquet"), &bars).unwrap();
    drop(bars);

    let symbols: Vec<String> = (0..SYMBOLS).map(|s| format!("S{s:02}")).collect();
    let refs: Vec<&str> = symbols.iter().map(String::as_str).collect();
    let (from, to) = all_range();
    let got = BarReader::open(dir.path())
        .unwrap()
        .read(&refs, from, to)
        .unwrap();

    assert_eq!(got.len(), SYMBOLS * DAYS as usize);

    let [o, h, l, c] = synthetic_prices(0, 1);
    assert_same(&got[0], &bar_at("S00", day_ts(1), &o, &h, &l, &c, "1"));
    let [o, h, l, c] = synthetic_prices(19, DAYS);
    let last = &got[got.len() - 1];
    assert_same(
        last,
        &bar_at("S19", day_ts(DAYS), &o, &h, &l, &c, &DAYS.to_string()),
    );
}
