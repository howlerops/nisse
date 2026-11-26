//! Fetching daily bars from Alpaca and writing them as one Parquet file.
//!
//! The network path is [`ingest`]; argument parsing is [`IngestArgs::parse`], which touches no
//! network and is what the tests cover.

use std::path::PathBuf;

use chrono::NaiveDate;
use nisse_alpaca_adapter::{AlpacaConfig, AlpacaData, AlpacaHttp, Feed, fetch_calendar};
use nisse_core::nautilus::UnixNanos;

use crate::write_bars;

/// Date format of `--start` and `--end`.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Parsed `nisse-ingest` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestArgs {
    pub symbols: Vec<String>,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub feed: Feed,
    pub out: PathBuf,
}

impl IngestArgs {
    /// Parses the arguments after the program name. Every flag is required and takes one value;
    /// flags may come in any order; a repeated flag, an unknown flag, a missing value, an empty
    /// symbol list, an empty symbol, a duplicate symbol, a bad date, `start > end`, or an unknown
    /// feed is `Err` with a message naming the flag. Symbols are upper-cased.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut symbols = None;
        let mut start = None;
        let mut end = None;
        let mut feed = None;
        let mut out = None;

        let mut iter = args.into_iter();
        while let Some(flag) = iter.next() {
            if !matches!(
                flag.as_str(),
                "--symbols" | "--start" | "--end" | "--feed" | "--out"
            ) {
                return Err(if flag.starts_with("--") {
                    format!("unknown flag {flag}")
                } else {
                    format!("unexpected argument {flag}")
                });
            }
            let value = match iter.next() {
                Some(value) if !value.starts_with("--") => value,
                _ => return Err(format!("{flag} needs a value")),
            };
            match flag.as_str() {
                "--symbols" => set_once(&mut symbols, &flag, parse_symbols(&value)?)?,
                "--start" => set_once(&mut start, &flag, parse_date(&flag, &value)?)?,
                "--end" => set_once(&mut end, &flag, parse_date(&flag, &value)?)?,
                "--feed" => set_once(&mut feed, &flag, parse_feed(&value)?)?,
                _ => set_once(&mut out, &flag, parse_out(&value)?)?,
            }
        }

        let start: NaiveDate = start.ok_or("missing --start")?;
        let end: NaiveDate = end.ok_or("missing --end")?;
        if start > end {
            return Err(format!("--start {start} is after --end {end}"));
        }
        Ok(Self {
            symbols: symbols.ok_or("missing --symbols")?,
            start,
            end,
            feed: feed.ok_or("missing --feed")?,
            out: out.ok_or("missing --out")?,
        })
    }
}

/// Stores `value` in `slot`, or fails if the flag was already given.
fn set_once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{flag} given more than once"));
    }
    *slot = Some(value);
    Ok(())
}

/// Splits a comma-separated list, trimming spaces and upper-casing each symbol.
fn parse_symbols(text: &str) -> Result<Vec<String>, String> {
    if text.trim().is_empty() {
        return Err("--symbols needs at least one symbol".to_owned());
    }
    let mut symbols: Vec<String> = Vec::new();
    for part in text.split(',') {
        let symbol = part.trim().to_ascii_uppercase();
        if symbol.is_empty() {
            return Err("--symbols contains an empty symbol".to_owned());
        }
        if symbols.contains(&symbol) {
            return Err(format!("--symbols lists {symbol} more than once"));
        }
        symbols.push(symbol);
    }
    Ok(symbols)
}

fn parse_date(flag: &str, value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, DATE_FORMAT)
        .map_err(|_| format!("{flag} is not a date (expected YYYY-MM-DD): {value}"))
}

fn parse_feed(value: &str) -> Result<Feed, String> {
    match value {
        "iex" => Ok(Feed::Iex),
        "sip" => Ok(Feed::Sip),
        other => Err(format!("--feed must be iex or sip, not {other}")),
    }
}

fn parse_out(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("--out needs a file path".to_owned());
    }
    Ok(PathBuf::from(value))
}

/// Fetches the calendar for `args.start..=args.end`, then the daily bars for `args.symbols`
/// (with `now` as `as_of`), and writes them to `args.out`. Returns the number of bars written.
/// Zero bars is an error and nothing is written.
pub async fn ingest(
    http: &AlpacaHttp,
    config: &AlpacaConfig,
    data: &AlpacaData,
    args: &IngestArgs,
    now: UnixNanos,
) -> anyhow::Result<usize> {
    let calendar = fetch_calendar(http, config, args.start, args.end).await?;
    let symbols: Vec<&str> = args.symbols.iter().map(String::as_str).collect();
    let bars = data
        .daily_bars(&symbols, args.start, args.end, &calendar, now)
        .await?;
    if bars.is_empty() {
        anyhow::bail!(
            "no bars for {} between {} and {}; nothing written",
            args.symbols.join(","),
            args.start,
            args.end
        );
    }
    write_bars(&args.out, &bars)?;
    Ok(bars.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    fn parse(items: &[&str]) -> Result<IngestArgs, String> {
        IngestArgs::parse(items.iter().map(|item| (*item).to_owned()))
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// The documented example, with every flag present.
    const EXAMPLE: [&str; 10] = [
        "--symbols",
        "SPY,EFA",
        "--start",
        "2015-01-01",
        "--end",
        "2024-12-31",
        "--feed",
        "iex",
        "--out",
        "data/bars/2024-12-31.parquet",
    ];

    /// Asserts that `result` is an error naming `flag`.
    fn assert_error_names(result: Result<IngestArgs, String>, flag: &str) {
        let message = match result {
            Ok(args) => panic!("expected an error naming {flag}, got {args:?}"),
            Err(message) => message,
        };
        assert!(
            message.contains(flag),
            "message {message:?} does not name {flag}"
        );
    }

    #[test]
    fn documented_example_parses_to_expected_struct() {
        let args = parse(&EXAMPLE).unwrap();
        assert_eq!(
            args,
            IngestArgs {
                symbols: vec!["SPY".to_owned(), "EFA".to_owned()],
                start: date(2015, 1, 1),
                end: date(2024, 12, 31),
                feed: Feed::Iex,
                out: PathBuf::from("data/bars/2024-12-31.parquet"),
            }
        );
    }

    #[test]
    fn flags_in_another_order_parse_the_same() {
        let shuffled = [
            "--out",
            "data/bars/2024-12-31.parquet",
            "--feed",
            "iex",
            "--end",
            "2024-12-31",
            "--start",
            "2015-01-01",
            "--symbols",
            "SPY,EFA",
        ];
        assert_eq!(parse(&shuffled).unwrap(), parse(&EXAMPLE).unwrap());
    }

    #[test]
    fn lower_case_symbols_are_upper_cased() {
        let args = parse(&[
            "--symbols",
            "spy,Efa",
            "--start",
            "2020-01-02",
            "--end",
            "2020-01-02",
            "--feed",
            "sip",
            "--out",
            "x.parquet",
        ])
        .unwrap();
        assert_eq!(args.symbols, vec!["SPY", "EFA"]);
        assert_eq!(args.feed, Feed::Sip);
    }

    #[test]
    fn spaces_around_symbols_are_trimmed() {
        let args = parse(&[
            "--symbols",
            " SPY , efa ",
            "--start",
            "2020-01-02",
            "--end",
            "2020-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ])
        .unwrap();
        assert_eq!(args.symbols, vec!["SPY", "EFA"]);
    }

    #[test]
    fn same_day_range_is_allowed() {
        let args = parse(&[
            "--symbols",
            "SPY",
            "--start",
            "2020-01-02",
            "--end",
            "2020-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ])
        .unwrap();
        assert_eq!(args.start, args.end);
    }

    #[test]
    fn repeated_flag_names_the_flag() {
        let mut items = EXAMPLE.to_vec();
        items.extend(["--feed", "sip"]);
        assert_error_names(parse(&items), "--feed");
    }

    #[test]
    fn unknown_flag_names_the_flag() {
        let mut items = EXAMPLE.to_vec();
        items.extend(["--adjust", "all"]);
        assert_error_names(parse(&items), "--adjust");
    }

    #[test]
    fn unexpected_positional_argument_is_an_error() {
        let mut items = EXAMPLE.to_vec();
        items.push("stray");
        assert!(parse(&items).is_err());
    }

    #[test]
    fn missing_value_names_the_flag() {
        assert_error_names(parse(&["--symbols", "SPY", "--out"]), "--out");
    }

    #[test]
    fn flag_followed_by_another_flag_is_a_missing_value() {
        let items = [
            "--symbols",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--symbols");
    }

    #[test]
    fn empty_symbol_list_names_the_flag() {
        let items = [
            "--symbols",
            "",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--symbols");
    }

    #[test]
    fn empty_symbol_in_list_names_the_flag() {
        let items = [
            "--symbols",
            "SPY,,EFA",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--symbols");
    }

    #[test]
    fn duplicate_symbol_names_the_flag() {
        let items = [
            "--symbols",
            "SPY,spy",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--symbols");
    }

    #[test]
    fn bad_date_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2015-02-30",
            "--end",
            "2015-03-01",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--start");
    }

    #[test]
    fn bad_end_date_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2015-01-01",
            "--end",
            "01/03/2015",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--end");
    }

    #[test]
    fn start_after_end_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2016-01-01",
            "--end",
            "2015-12-31",
            "--feed",
            "iex",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--start");
    }

    #[test]
    fn unknown_feed_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "delayed",
            "--out",
            "x.parquet",
        ];
        assert_error_names(parse(&items), "--feed");
    }

    #[test]
    fn missing_flag_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
        ];
        assert_error_names(parse(&items), "--out");
    }

    #[test]
    fn empty_out_path_names_the_flag() {
        let items = [
            "--symbols",
            "SPY",
            "--start",
            "2015-01-01",
            "--end",
            "2015-01-02",
            "--feed",
            "iex",
            "--out",
            "",
        ];
        assert_error_names(parse(&items), "--out");
    }

    #[test]
    fn no_arguments_is_an_error() {
        assert!(parse(&[]).is_err());
    }
}
