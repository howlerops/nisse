//! Alpaca market data REST client: historical bars and latest quotes.

use std::borrow::Cow;
use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate};
use chrono_tz::America::New_York;
use nisse_core::MarketCalendar;
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, PriceType,
    QuoteTick, Symbol, UnixNanos, Venue,
};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::parse::{decimal_text, parse_price, parse_qty, parse_ts};
use crate::{AdapterError, AlpacaConfig, AlpacaHttp, Feed};

/// Venue code used in every instrument id this adapter produces (`SPY.ALPACA`).
pub const VENUE: &str = "ALPACA";

/// Upper bound on pages followed in one bars request, so a server that keeps returning a
/// page token cannot loop forever (1000 pages is ten million bars).
const MAX_BAR_PAGES: usize = 1000;

/// The largest page the bars endpoint returns.
const BARS_PAGE_LIMIT: &str = "10000";

/// Market data over Alpaca's data API, on the configured feed.
#[derive(Debug, Clone)]
pub struct AlpacaData {
    http: AlpacaHttp,
    base_url: String,
    feed: Feed,
}

/// One page of `GET /v2/stocks/bars`.
#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: Option<BTreeMap<String, Vec<RawBar>>>,
    next_page_token: Option<String>,
}

/// One bar. Prices and volume are JSON numbers, read from their text.
#[derive(Deserialize)]
struct RawBar {
    o: Box<RawValue>,
    h: Box<RawValue>,
    l: Box<RawValue>,
    c: Box<RawValue>,
    v: Box<RawValue>,
    t: String,
}

/// `GET /v2/stocks/quotes/latest`. A symbol with no quote maps to `null`.
#[derive(Deserialize)]
struct QuotesResponse {
    #[serde(default)]
    quotes: BTreeMap<String, Option<RawQuote>>,
}

/// One latest quote. `as` is a keyword in Rust, so it is renamed.
#[derive(Deserialize)]
struct RawQuote {
    ap: Box<RawValue>,
    bp: Box<RawValue>,
    #[serde(rename = "as")]
    ask_size: Box<RawValue>,
    bs: Box<RawValue>,
    t: String,
}

impl AlpacaData {
    #[must_use]
    pub fn new(http: AlpacaHttp, config: &AlpacaConfig) -> Self {
        Self {
            http,
            base_url: config.data_base_url.clone(),
            feed: config.feed,
        }
    }
}

impl AlpacaData {
    /// Daily bars for `symbols` from `start` to `end` (inclusive dates), fully adjusted
    /// (`adjustment=all`), following `next_page_token` until exhausted. The result is sorted
    /// by symbol, then time. Each bar is `{SYMBOL}.ALPACA-1-DAY-LAST-EXTERNAL`.
    ///
    /// Alpaca stamps a daily bar with the *start* of its day (midnight New York), when its
    /// close is not yet known. To keep look-ahead out of backtests, `ts_event` and `ts_init`
    /// are set to the close of that day's session in `calendar`, which is when the bar
    /// became knowable, and a bar whose session closes after `as_of` (today's bar while the
    /// market is still open) is left out. A bar dated on a day that is not a session in
    /// `calendar` is an error: the calendar does not cover the request. No wall clock is read.
    pub async fn daily_bars(
        &self,
        symbols: &[&str],
        start: NaiveDate,
        end: NaiveDate,
        calendar: &MarketCalendar,
        as_of: UnixNanos,
    ) -> Result<Vec<Bar>, AdapterError> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/v2/stocks/bars", self.base_url);
        let symbols_param = symbols.join(",");
        let start_param = start.format("%Y-%m-%d").to_string();
        let end_param = end.format("%Y-%m-%d").to_string();
        let spec = BarSpecification::new_checked(1, BarAggregation::Day, PriceType::Last)
            .map_err(decode_error)?;

        let mut bars = Vec::new();
        let mut page_token: Option<String> = None;
        for _page in 0..MAX_BAR_PAGES {
            let mut query: Vec<(&str, &str)> = Vec::with_capacity(9);
            query.extend([
                ("symbols", symbols_param.as_str()),
                ("timeframe", "1Day"),
                ("start", start_param.as_str()),
                ("end", end_param.as_str()),
                ("adjustment", "all"),
                ("feed", self.feed.as_str()),
                ("limit", BARS_PAGE_LIMIT),
                ("sort", "asc"),
            ]);
            if let Some(token) = page_token.as_deref() {
                query.push(("page_token", token));
            }
            let page: BarsPage = self.http.get(&url, &query).await?;

            if let Some(by_symbol) = &page.bars {
                let count: usize = by_symbol.values().map(Vec::len).sum();
                bars.reserve(count);
                for (symbol, raw_bars) in by_symbol {
                    let bar_type = bar_type(symbol, spec)?;
                    for raw in raw_bars {
                        if let Some(bar) = bar_from_raw(bar_type, raw, calendar, as_of)? {
                            bars.push(bar);
                        }
                    }
                }
            }

            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => {
                    bars.sort_by(|a, b| {
                        a.instrument_id()
                            .symbol
                            .as_str()
                            .cmp(b.instrument_id().symbol.as_str())
                            .then(a.ts_event.cmp(&b.ts_event))
                    });
                    return Ok(bars);
                }
            }
        }
        Err(AdapterError::Decode(format!(
            "bars did not end after {MAX_BAR_PAGES} pages"
        )))
    }

    /// The latest quote for each symbol, sorted by symbol. A symbol with no quote, or with a
    /// zero bid or ask (Alpaca's "no active quote on that side"), is left out.
    pub async fn latest_quotes(&self, symbols: &[&str]) -> Result<Vec<QuoteTick>, AdapterError> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/v2/stocks/quotes/latest", self.base_url);
        let symbols_param = symbols.join(",");
        let query = [
            ("symbols", symbols_param.as_str()),
            ("feed", self.feed.as_str()),
        ];
        let response: QuotesResponse = self.http.get(&url, &query).await?;

        // The map is a BTreeMap, so iterating it yields the symbols in sorted order.
        let mut quotes = Vec::with_capacity(response.quotes.len());
        for (symbol, raw) in &response.quotes {
            let Some(raw) = raw else { continue };
            if let Some(quote) = quote_from_raw(symbol, raw)? {
                quotes.push(quote);
            }
        }
        Ok(quotes)
    }
}

fn decode_error(e: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(e.to_string())
}

/// `{symbol}.ALPACA` with the daily spec, as a Nautilus bar type.
fn bar_type(symbol: &str, spec: BarSpecification) -> Result<BarType, AdapterError> {
    let symbol = Symbol::new_checked(symbol).map_err(decode_error)?;
    let venue = Venue::new_checked(VENUE).map_err(decode_error)?;
    let instrument_id = InstrumentId::new(symbol, venue);
    Ok(BarType::new(
        instrument_id,
        spec,
        AggregationSource::External,
    ))
}

/// `None` when the bar's session has not closed by `as_of`.
fn bar_from_raw(
    bar_type: BarType,
    raw: &RawBar,
    calendar: &MarketCalendar,
    as_of: UnixNanos,
) -> Result<Option<Bar>, AdapterError> {
    // Alpaca's `t` for a daily bar is midnight New York on the bar's date.
    let date = DateTime::parse_from_rfc3339(&raw.t)
        .map_err(|_| AdapterError::Decode(format!("bad bar time {}", raw.t)))?
        .with_timezone(&New_York)
        .date_naive();
    let session = calendar.session_on(date).ok_or_else(|| {
        AdapterError::Decode(format!(
            "bar dated {date}, which is not a session in the calendar"
        ))
    })?;
    if session.close > as_of {
        return Ok(None);
    }
    let ts = session.close;
    let open = decimal_text(&raw.o)?;
    let high = decimal_text(&raw.h)?;
    let low = decimal_text(&raw.l)?;
    let close = decimal_text(&raw.c)?;
    // A bar's four prices must share one precision.
    let digits = [open, high, low, close]
        .into_iter()
        .map(fraction_digits)
        .fold(0, usize::max);
    let bar = Bar::new_checked(
        bar_type,
        parse_price(&pad_fraction(open, digits))?,
        parse_price(&pad_fraction(high, digits))?,
        parse_price(&pad_fraction(low, digits))?,
        parse_price(&pad_fraction(close, digits))?,
        parse_qty(decimal_text(&raw.v)?)?,
        ts,
        ts,
    )
    .map_err(decode_error)?;
    Ok(Some(bar))
}

/// `None` when the bid or the ask is zero (no quote on that side).
fn quote_from_raw(symbol: &str, raw: &RawQuote) -> Result<Option<QuoteTick>, AdapterError> {
    let bid = decimal_text(&raw.bp)?;
    let ask = decimal_text(&raw.ap)?;
    if is_zero(bid) || is_zero(ask) {
        return Ok(None);
    }
    let bid_size = decimal_text(&raw.bs)?;
    let ask_size = decimal_text(&raw.ask_size)?;
    let price_digits = fraction_digits(bid).max(fraction_digits(ask));
    let size_digits = fraction_digits(bid_size).max(fraction_digits(ask_size));
    let ts = parse_ts(&raw.t)?;

    let symbol = Symbol::new_checked(symbol).map_err(decode_error)?;
    let venue = Venue::new_checked(VENUE).map_err(decode_error)?;
    let quote = QuoteTick::new_checked(
        InstrumentId::new(symbol, venue),
        parse_price(&pad_fraction(bid, price_digits))?,
        parse_price(&pad_fraction(ask, price_digits))?,
        parse_qty(&pad_fraction(bid_size, size_digits))?,
        parse_qty(&pad_fraction(ask_size, size_digits))?,
        ts,
        ts,
    )
    .map_err(decode_error)?;
    Ok(Some(quote))
}

/// Digits after the decimal point; 0 when there is none.
fn fraction_digits(text: &str) -> usize {
    text.find('.').map_or(0, |point| text.len() - point - 1)
}

/// `text` with trailing zeros added so it has `digits` fractional digits. Borrowed unchanged
/// when it already has them, so the common case does not allocate.
fn pad_fraction(text: &str, digits: usize) -> Cow<'_, str> {
    let have = fraction_digits(text);
    if have >= digits {
        return Cow::Borrowed(text);
    }
    let mut padded = String::with_capacity(text.len() + digits - have + 1);
    padded.push_str(text);
    if !text.contains('.') {
        padded.push('.');
    }
    padded.extend(std::iter::repeat_n('0', digits - have));
    Cow::Owned(padded)
}

/// True for a decimal text made only of zeros (e.g. `0`, `0.0`).
fn is_zero(text: &str) -> bool {
    text.bytes().all(|b| b == b'0' || b == b'.')
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param, query_param_is_missing},
    };

    use super::*;
    use crate::Credentials;

    fn data(server: &MockServer) -> AlpacaData {
        let config = AlpacaConfig {
            trading_base_url: "http://127.0.0.1:1".to_owned(),
            data_base_url: server.uri(),
            stream_url: "ws://127.0.0.1:1".to_owned(),
            feed: Feed::Iex,
        };
        let creds = Credentials::new("test-key-id", "test-secret");
        let http = AlpacaHttp::new(&creds, Duration::from_secs(5)).unwrap();
        AlpacaData::new(http, &config)
    }

    /// Sessions for 3, 4 and 5 January 2022 (09:30 to 16:00 New York = 14:30 to 21:00 UTC).
    fn calendar() -> MarketCalendar {
        let session = |day: u32| {
            let date = NaiveDate::from_ymd_opt(2022, 1, day).unwrap();
            let at = |h: u32, m: u32| {
                let nanos = date
                    .and_hms_opt(h, m, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_nanos_opt()
                    .unwrap();
                UnixNanos::from(u64::try_from(nanos).unwrap())
            };
            nisse_core::Session {
                date,
                open: at(14, 30),
                close: at(21, 0),
            }
        };
        MarketCalendar::new(vec![session(3), session(4), session(5)]).unwrap()
    }

    fn far_future() -> UnixNanos {
        UnixNanos::from(u64::MAX)
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    /// A 200 response whose body is `text` verbatim, so JSON numbers keep their exact text.
    fn raw_json(text: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(text.to_owned(), "application/json")
    }

    #[tokio::test]
    async fn daily_bars_sends_the_documented_query_and_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .and(query_param("symbols", "AAPL,TSLA"))
            .and(query_param("timeframe", "1Day"))
            .and(query_param("start", "2022-01-01"))
            .and(query_param("end", "2022-01-31"))
            .and(query_param("adjustment", "all"))
            .and(query_param("feed", "iex"))
            .and(query_param("limit", "10000"))
            .and(query_param("sort", "asc"))
            .and(query_param_is_missing("page_token"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":178.21,"h":178.26,"l":178.21,"n":65,"o":178.26,"t":"2022-01-03T09:00:00Z","v":1118,"vw":178.235733}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &["AAPL", "TSLA"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();

        assert_eq!(bars.len(), 1);
        let bar = &bars[0];
        assert_eq!(bar.bar_type.to_string(), "AAPL.ALPACA-1-DAY-LAST-EXTERNAL");
        assert_eq!(bar.open.to_string(), "178.26");
        assert_eq!(bar.high.to_string(), "178.26");
        assert_eq!(bar.low.to_string(), "178.21");
        assert_eq!(bar.close.to_string(), "178.21");
        assert_eq!(bar.volume.to_string(), "1118");
        // Stamped at the session close (21:00 UTC), not at Alpaca's start-of-day `t`.
        let ts = parse_ts("2022-01-03T21:00:00Z").unwrap();
        assert_eq!(bar.ts_event, ts);
        assert_eq!(bar.ts_init, bar.ts_event);
    }

    #[tokio::test]
    async fn daily_bars_follows_two_pages_with_one_request_each() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .and(query_param_is_missing("page_token"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":10.5,"h":11,"l":10,"o":10,"t":"2022-01-03T05:00:00Z","v":100}]},"next_page_token":"QUFQ"}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .and(query_param("page_token", "QUFQ"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":12,"h":12,"l":12,"o":12,"t":"2022-01-04T05:00:00Z","v":200}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();

        let closes: Vec<String> = bars.iter().map(|b| b.close.to_string()).collect();
        assert_eq!(closes, ["10.5", "12"]);
        assert_eq!(bars[1].volume.to_string(), "200");
    }

    #[tokio::test]
    async fn daily_bars_sorts_by_symbol_then_time_across_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param_is_missing("page_token"))
            .respond_with(raw_json(
                r#"{"bars":{"TSLA":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-04T05:00:00Z","v":1}],"AAPL":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-05T05:00:00Z","v":1}]},"next_page_token":"tok"}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("page_token", "tok"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-03T05:00:00Z","v":1}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &["AAPL", "TSLA"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();

        let keys: Vec<(String, u64)> = bars
            .iter()
            .map(|b| {
                (
                    b.instrument_id().symbol.as_str().to_owned(),
                    b.ts_event.as_u64(),
                )
            })
            .collect();
        let aapl_first = parse_ts("2022-01-03T21:00:00Z").unwrap().as_u64();
        let aapl_second = parse_ts("2022-01-05T21:00:00Z").unwrap().as_u64();
        let tsla = parse_ts("2022-01-04T21:00:00Z").unwrap().as_u64();
        assert_eq!(
            keys,
            [
                ("AAPL".to_owned(), aapl_first),
                ("AAPL".to_owned(), aapl_second),
                ("TSLA".to_owned(), tsla),
            ]
        );
    }

    #[tokio::test]
    async fn daily_bars_keeps_exact_prices_and_pads_to_one_precision() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":0.1,"h":0.1,"l":0.1,"o":0.1,"t":"2022-01-03T05:00:00Z","v":1},{"c":178.21,"h":178.26,"l":178.2,"o":178.2,"t":"2022-01-04T05:00:00Z","v":1}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();

        assert_eq!(bars[0].close.to_string(), "0.1");
        assert_eq!(bars[1].close.to_string(), "178.21");
        assert_eq!(bars[1].open.to_string(), "178.20");
        assert_eq!(bars[1].low.to_string(), "178.20");
    }

    #[tokio::test]
    async fn daily_bars_leave_out_a_session_that_has_not_closed_and_reject_non_sessions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"bars":{"AAPL":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-03T05:00:00Z","v":1},{"c":2,"h":2,"l":2,"o":2,"t":"2022-01-04T05:00:00Z","v":1}]},"next_page_token":null}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        // One second before the 4 January close: that day's bar is still forming.
        let as_of = parse_ts("2022-01-04T20:59:59Z").unwrap();
        let bars = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                as_of,
            )
            .await
            .unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].close.to_string(), "1");
        // At the close it is known.
        let at_close = parse_ts("2022-01-04T21:00:00Z").unwrap();
        let bars = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                at_close,
            )
            .await
            .unwrap();
        assert_eq!(bars.len(), 2);

        // A bar dated on a day the calendar does not hold (a Saturday) is refused.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"bars":{"AAPL":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-08T05:00:00Z","v":1}]},"next_page_token":null}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        let err = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Decode(_)));
    }

    #[tokio::test]
    async fn daily_bars_with_no_symbols_makes_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &[],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();
        assert!(bars.is_empty());
        let quotes = data(&server).latest_quotes(&[]).await.unwrap();
        assert!(quotes.is_empty());
    }

    #[tokio::test]
    async fn daily_bars_omits_symbols_without_data() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":1,"h":1,"l":1,"o":1,"t":"2022-01-03T05:00:00Z","v":1}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bars = data(&server)
            .daily_bars(
                &["AAPL", "NOPE"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(
            bars[0].bar_type.to_string(),
            "AAPL.ALPACA-1-DAY-LAST-EXTERNAL"
        );
    }

    #[tokio::test]
    async fn daily_bars_surfaces_forbidden_as_status_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(json!({"code": 40310000, "message": "forbidden"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let err = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Status { status: 403, .. }));
    }

    #[tokio::test]
    async fn daily_bars_rejects_null_price_and_inconsistent_ohlc() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":null,"h":1,"l":1,"o":1,"t":"2022-01-03T05:00:00Z","v":1}]},"next_page_token":null}"#,
            ))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        let err = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Decode(_)));

        let server = MockServer::start().await;
        // high below close is not a valid bar.
        Mock::given(method("GET"))
            .respond_with(raw_json(
                r#"{"bars":{"AAPL":[{"c":5,"h":4,"l":1,"o":1,"t":"2022-01-03T05:00:00Z","v":1}]},"next_page_token":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let err = data(&server)
            .daily_bars(
                &["AAPL"],
                date("2022-01-01"),
                date("2022-01-31"),
                &calendar(),
                far_future(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Decode(_)));
    }

    #[tokio::test]
    async fn latest_quotes_maps_fields_and_pads_to_one_precision() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/quotes/latest"))
            .and(query_param("symbols", "AAPL"))
            .and(query_param("feed", "iex"))
            .respond_with(raw_json(
                r#"{"quotes":{"AAPL":{"ap":172.7,"as":1,"ax":"Q","bp":172.62,"bs":2,"bx":"Q","c":["R"],"t":"2022-08-17T10:07:40.286587431Z","z":"C"}}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let quotes = data(&server).latest_quotes(&["AAPL"]).await.unwrap();

        assert_eq!(quotes.len(), 1);
        let quote = &quotes[0];
        assert_eq!(quote.instrument_id.to_string(), "AAPL.ALPACA");
        assert_eq!(quote.bid_price.to_string(), "172.62");
        assert_eq!(quote.ask_price.to_string(), "172.70");
        assert_eq!(quote.bid_size.to_string(), "2");
        assert_eq!(quote.ask_size.to_string(), "1");
        let ts = parse_ts("2022-08-17T10:07:40.286587431Z").unwrap();
        assert_eq!(quote.ts_event, ts);
        assert_eq!(quote.ts_init, ts);
    }

    #[tokio::test]
    async fn latest_quotes_drops_missing_and_zero_sided_quotes_and_sorts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(raw_json(
                r#"{"quotes":{"ZZZ":{"ap":1,"as":1,"bp":0,"bs":1,"t":"2022-08-17T10:07:40Z"},"MSFT":{"ap":0,"as":1,"bp":2.5,"bs":1,"t":"2022-08-17T10:07:40Z"},"NVDA":null,"TSLA":{"ap":3,"as":4,"bp":2.5,"bs":5,"t":"2022-08-17T10:07:40Z"},"AAPL":{"ap":1.5,"as":1,"bp":1.25,"bs":2,"t":"2022-08-17T10:07:40Z"}}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let quotes = data(&server)
            .latest_quotes(&["ZZZ", "MSFT", "NVDA", "TSLA", "AAPL"])
            .await
            .unwrap();

        let symbols: Vec<String> = quotes
            .iter()
            .map(|q| q.instrument_id.symbol.as_str().to_owned())
            .collect();
        assert_eq!(symbols, ["AAPL", "TSLA"]);
        assert_eq!(quotes[1].bid_price.to_string(), "2.5");
        assert_eq!(quotes[1].ask_price.to_string(), "3.0");
    }

    #[tokio::test]
    async fn latest_quotes_surfaces_forbidden_as_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let err = data(&server).latest_quotes(&["AAPL"]).await.unwrap_err();
        assert!(matches!(err, AdapterError::Status { status: 403, .. }));
    }

    #[test]
    fn pad_fraction_borrows_when_no_padding_is_needed() {
        assert!(matches!(pad_fraction("1.25", 2), Cow::Borrowed("1.25")));
        assert_eq!(pad_fraction("1.2", 2), "1.20");
        assert_eq!(pad_fraction("7", 2), "7.00");
        assert_eq!(pad_fraction("7", 0), "7");
    }

    #[test]
    fn is_zero_accepts_only_zero_texts() {
        assert!(is_zero("0"));
        assert!(is_zero("0.00"));
        assert!(!is_zero("0.01"));
        assert!(!is_zero("10"));
    }
}
