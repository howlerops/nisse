//! The `ingest` step: fetch the whole bar history and write one complete file per session.
//!
//! The file for a session is `<bars_dir>/<YYYY-MM-DD>.parquet`. It holds every bar from
//! `history_start` through the session date, so a later file is a complete replacement and
//! never a patch. A file that already exists is kept as it is. See `docs/phases/phase-6.md`,
//! `session-close` step 2.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use nisse_bars::write_bars;
use nisse_core::MarketCalendar;
use nisse_core::Session;
use nisse_core::nautilus::{Bar, Symbol, UnixNanos};

use crate::AppError;
use crate::market::Market;

/// What the ingest step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestOutcome {
    pub path: PathBuf,
    /// Bars written; 0 when the file already existed.
    pub bars: usize,
    /// The file for this session already existed, so nothing was fetched.
    pub skipped: bool,
}

/// `<bars_dir>/<YYYY-MM-DD>.parquet` for a session date.
pub fn bar_file_path(bars_dir: &Path, session_date: NaiveDate) -> PathBuf {
    bars_dir.join(format!("{}.parquet", session_date.format("%Y-%m-%d")))
}

/// Fetches the whole history `history_start..=session.date` for `symbols` and writes it as one
/// complete file named after the session. See docs/phases/phase-6.md, session-close step 2.
///
/// Nothing is decided on a universe with a hole in it: every symbol must have a bar stamped at
/// the session close, and no bar may be stamped after it. On any error nothing is written.
pub async fn ingest_session<M: Market>(
    market: &M,
    calendar: &MarketCalendar,
    symbols: &[Symbol],
    history_start: NaiveDate,
    session: &Session,
    bars_dir: &Path,
    now: UnixNanos,
) -> Result<IngestOutcome, AppError> {
    let path = bar_file_path(bars_dir, session.date);
    if path.exists() {
        return Ok(IngestOutcome {
            path,
            bars: 0,
            skipped: true,
        });
    }
    if now < session.close {
        return Err(AppError::Market(format!(
            "the session on {} has not closed (closes at {} ns, now {} ns)",
            session.date,
            session.close.as_u64(),
            now.as_u64()
        )));
    }

    let bars = market
        .daily_bars(symbols, history_start, session.date, calendar, now)
        .await?;
    check_closes(&bars, symbols, session.close)?;

    write_bars(&path, &bars)?;
    Ok(IngestOutcome {
        path,
        bars: bars.len(),
        skipped: false,
    })
}

/// One pass over `bars`: fails on a bar stamped after `close`, then names every symbol in
/// `symbols` with no bar stamped exactly at `close` (sorted).
fn check_closes(bars: &[Bar], symbols: &[Symbol], close: UnixNanos) -> Result<(), AppError> {
    let mut closed: HashSet<Symbol> = HashSet::with_capacity(symbols.len());
    for bar in bars {
        if bar.ts_event > close {
            return Err(AppError::Market(format!(
                "bar for {} at {} ns is after the session close {} ns; nothing written",
                bar.bar_type.instrument_id().symbol,
                bar.ts_event.as_u64(),
                close.as_u64()
            )));
        }
        if bar.ts_event == close {
            closed.insert(bar.bar_type.instrument_id().symbol);
        }
    }

    let mut missing: Vec<&str> = symbols
        .iter()
        .filter(|symbol| !closed.contains(symbol))
        .map(Symbol::as_str)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort_unstable();
    missing.dedup();
    Err(AppError::Market(format!(
        "no bar at the session close for {}; nothing written",
        missing.join(", ")
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use nisse_alpaca_adapter::VENUE;
    use nisse_bars::BarReader;
    use nisse_core::nautilus::{
        AggregationSource, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Venue,
    };

    /// Removes its directory when dropped, including on a failed assertion.
    struct Scratch(PathBuf);

    impl Scratch {
        /// A new, empty directory under the system temp dir, unique to this process and call.
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("nisse-app-ingest-test-{}-{n}", std::process::id()));
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

    /// A market that serves a fixed list of bars and counts its calls.
    struct FakeMarket {
        bars: Vec<Bar>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeMarket {
        fn new(bars: Vec<Bar>) -> Self {
            Self {
                bars,
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Market for FakeMarket {
        async fn calendar(
            &self,
            _start: NaiveDate,
            _end: NaiveDate,
        ) -> Result<MarketCalendar, AppError> {
            Err(AppError::Market("the fake has no calendar".to_owned()))
        }

        async fn daily_bars(
            &self,
            symbols: &[Symbol],
            _start: NaiveDate,
            _end: NaiveDate,
            _calendar: &MarketCalendar,
            as_of: UnixNanos,
        ) -> Result<Vec<Bar>, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(AppError::Market("the fake is down".to_owned()));
            }
            Ok(self
                .bars
                .iter()
                .filter(|bar| {
                    symbols.contains(&bar.bar_type.instrument_id().symbol) && bar.ts_event <= as_of
                })
                .cloned()
                .collect())
        }
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
    }

    /// `hour:00` UTC on `day` of January 2026, in nanoseconds since the epoch.
    fn utc(day: u32, hour: u32) -> UnixNanos {
        let secs = date(day)
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        UnixNanos::from(u64::try_from(secs).unwrap() * 1_000_000_000)
    }

    /// The session of `day`: 14:30 to 21:00 UTC.
    fn session(day: u32) -> Session {
        Session {
            date: date(day),
            open: utc(day, 14),
            close: utc(day, 21),
        }
    }

    fn calendar_for(session: &Session) -> MarketCalendar {
        MarketCalendar::new(vec![*session]).unwrap()
    }

    /// A daily bar for `symbol` stamped at `ts` (both event and init time).
    fn bar(symbol: &str, ts: UnixNanos) -> Bar {
        let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new(VENUE));
        let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
        let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
        Bar::new(
            bar_type,
            Price::from("10.00"),
            Price::from("11.00"),
            Price::from("9.00"),
            Price::from("10.50"),
            Quantity::from("1000"),
            ts,
            ts,
        )
    }

    /// Bars for `symbols`, each stamped at `session`'s close, plus one earlier bar for the first.
    fn closed_bars(session: &Session, symbols: &[&str]) -> Vec<Bar> {
        let mut bars: Vec<Bar> = symbols
            .iter()
            .map(|symbol| bar(symbol, session.close))
            .collect();
        bars.push(bar(
            symbols[0],
            UnixNanos::from(session.close.as_u64() - 86_400_000_000_000),
        ));
        bars
    }

    #[tokio::test]
    async fn writes_one_file_holding_all_bars() {
        let scratch = Scratch::new();
        let s = session(9);
        let market = FakeMarket::new(closed_bars(&s, &["SPY", "EFA"]));
        let symbols = [Symbol::new("SPY"), Symbol::new("EFA")];

        let outcome = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 22),
        )
        .await
        .unwrap();

        let expected = scratch.path().join("2026-01-09.parquet");
        assert_eq!(
            outcome,
            IngestOutcome {
                path: expected.clone(),
                bars: 3,
                skipped: false,
            }
        );
        assert!(expected.is_file());
        assert_eq!(market.calls(), 1);

        let read = BarReader::open(scratch.path())
            .unwrap()
            .read(
                &["SPY", "EFA"],
                UnixNanos::from(0),
                UnixNanos::from(u64::try_from(i64::MAX).unwrap()),
            )
            .unwrap();
        assert_eq!(read.len(), 3);
    }

    #[tokio::test]
    async fn second_call_skips_and_does_not_fetch() {
        let scratch = Scratch::new();
        let s = session(9);
        let market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        let symbols = [Symbol::new("SPY")];
        let calendar = calendar_for(&s);
        let now = utc(9, 22);

        ingest_session(
            &market,
            &calendar,
            &symbols,
            date(2),
            &s,
            scratch.path(),
            now,
        )
        .await
        .unwrap();
        assert_eq!(market.calls(), 1);

        let again = ingest_session(
            &market,
            &calendar,
            &symbols,
            date(2),
            &s,
            scratch.path(),
            now,
        )
        .await
        .unwrap();
        assert_eq!(
            again,
            IngestOutcome {
                path: scratch.path().join("2026-01-09.parquet"),
                bars: 0,
                skipped: true,
            }
        );
        assert_eq!(market.calls(), 1);
    }

    #[tokio::test]
    async fn existing_file_is_skipped_even_before_the_close() {
        let scratch = Scratch::new();
        let s = session(9);
        let symbols = [Symbol::new("SPY")];
        let market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 22),
        )
        .await
        .unwrap();
        let calls_before = market.calls();

        let outcome = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 1),
        )
        .await
        .unwrap();
        assert!(outcome.skipped);
        assert_eq!(market.calls(), calls_before);
    }

    #[tokio::test]
    async fn symbol_without_a_close_bar_is_an_error_and_nothing_is_written() {
        let scratch = Scratch::new();
        let s = session(9);
        // EFA has only yesterday's bar, so it has no bar at today's close.
        let mut bars = closed_bars(&s, &["SPY"]);
        bars.push(bar("EFA", utc(8, 21)));
        let market = FakeMarket::new(bars);
        let symbols = [Symbol::new("SPY"), Symbol::new("EFA")];

        let err = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 22),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, AppError::Market(message) if message.contains("EFA")),
            "{err:?}"
        );
        assert!(!scratch.path().join("2026-01-09.parquet").exists());
    }

    #[tokio::test]
    async fn every_missing_symbol_is_named_in_sorted_order() {
        let scratch = Scratch::new();
        let s = session(9);
        let market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        let symbols = [Symbol::new("IWM"), Symbol::new("SPY"), Symbol::new("EFA")];

        let err = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 22),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, AppError::Market(message) if message.contains("EFA, IWM")),
            "{err:?}"
        );
        assert!(!scratch.path().join("2026-01-09.parquet").exists());
    }

    #[tokio::test]
    async fn now_before_the_close_is_an_error_and_the_market_is_not_called() {
        let scratch = Scratch::new();
        let s = session(9);
        let market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        let symbols = [Symbol::new("SPY")];

        let err = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 20),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AppError::Market(_)));
        assert_eq!(market.calls(), 0);
        assert!(!scratch.path().join("2026-01-09.parquet").exists());
    }

    #[tokio::test]
    async fn now_exactly_at_the_close_is_allowed() {
        let scratch = Scratch::new();
        let s = session(9);
        let market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        let symbols = [Symbol::new("SPY")];

        let outcome = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            s.close,
        )
        .await
        .unwrap();
        assert!(!outcome.skipped);
        assert_eq!(market.calls(), 1);
    }

    #[tokio::test]
    async fn bar_after_the_session_close_is_an_error_and_nothing_is_written() {
        let scratch = Scratch::new();
        let s = session(9);
        let mut bars = closed_bars(&s, &["SPY"]);
        bars.push(bar(
            "SPY",
            UnixNanos::from(s.close.as_u64() + 60_000_000_000),
        ));
        let market = FakeMarket::new(bars);
        let symbols = [Symbol::new("SPY")];

        let err = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(10, 0),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AppError::Market(_)));
        assert!(!scratch.path().join("2026-01-09.parquet").exists());
    }

    #[tokio::test]
    async fn market_failure_is_returned_and_nothing_is_written() {
        let scratch = Scratch::new();
        let s = session(9);
        let mut market = FakeMarket::new(closed_bars(&s, &["SPY"]));
        market.fail = true;
        let symbols = [Symbol::new("SPY")];

        let err = ingest_session(
            &market,
            &calendar_for(&s),
            &symbols,
            date(2),
            &s,
            scratch.path(),
            utc(9, 22),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AppError::Market(_)));
        assert_eq!(market.calls(), 1);
        assert!(!scratch.path().join("2026-01-09.parquet").exists());
    }

    #[test]
    fn bar_file_path_formats_the_date_as_year_month_day() {
        assert_eq!(
            bar_file_path(Path::new("data/bars"), date(5)),
            PathBuf::from("data/bars/2026-01-05.parquet")
        );
    }
}
