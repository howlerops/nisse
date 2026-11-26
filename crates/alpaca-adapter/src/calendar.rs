//! Alpaca calendar endpoint (`GET /v2/calendar`) to [`MarketCalendar`].

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono_tz::America::New_York;
use nisse_core::nautilus::UnixNanos;
use nisse_core::{MarketCalendar, Session};
use serde::Deserialize;

use crate::{AdapterError, AlpacaConfig, AlpacaHttp};

/// One entry of the calendar array. Alpaca also sends session and settlement times, which
/// are ignored here.
#[derive(Deserialize)]
struct WireDay {
    date: String,
    open: String,
    close: String,
}

/// Converts `HH:MM` New York local time on `date` to UTC nanoseconds. A local time that is
/// ambiguous or skipped by daylight saving is an error rather than a guess.
fn local_to_utc(date: NaiveDate, hhmm: &str) -> Result<UnixNanos, AdapterError> {
    let time = NaiveTime::parse_from_str(hhmm, "%H:%M")
        .map_err(|_| AdapterError::Decode(format!("bad calendar time {hhmm:?}")))?;
    let local = New_York
        .from_local_datetime(&date.and_time(time))
        .single()
        .ok_or_else(|| {
            AdapterError::Decode(format!("{date} {hhmm} is ambiguous or absent in New York"))
        })?;
    let nanos = local
        .timestamp_nanos_opt()
        .ok_or_else(|| AdapterError::Decode("calendar time out of range".to_owned()))?;
    u64::try_from(nanos)
        .map(UnixNanos::from)
        .map_err(|_| AdapterError::Decode("calendar time before the epoch".to_owned()))
}

/// Converts one wire entry to a [`Session`]. Shared by [`sessions_from_json`] and
/// [`fetch_calendar`].
fn session_from_wire(entry: &WireDay) -> Result<Session, AdapterError> {
    let date = NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
        .map_err(|_| AdapterError::Decode(format!("bad calendar date {:?}", entry.date)))?;
    Ok(Session {
        date,
        open: local_to_utc(date, &entry.open)?,
        close: local_to_utc(date, &entry.close)?,
    })
}

fn sessions_from_wire(days: &[WireDay]) -> Result<Vec<Session>, AdapterError> {
    days.iter().map(session_from_wire).collect()
}

/// Converts Alpaca calendar entries to sessions. `date` is `YYYY-MM-DD`; `open` and `close`
/// are `HH:MM` in America/New_York local time (so a half-day shows `close` of `13:00`) and are
/// converted to UTC with the tz database, which handles daylight saving.
pub(crate) fn sessions_from_json(json: &str) -> Result<Vec<Session>, AdapterError> {
    let days: Vec<WireDay> =
        serde_json::from_str(json).map_err(|e| AdapterError::Decode(e.to_string()))?;
    sessions_from_wire(&days)
}

/// Fetches the trading calendar for `start..=end` and builds a [`MarketCalendar`].
pub async fn fetch_calendar(
    http: &AlpacaHttp,
    config: &AlpacaConfig,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<MarketCalendar, AdapterError> {
    let start = start.format("%Y-%m-%d").to_string();
    let end = end.format("%Y-%m-%d").to_string();
    let url = format!("{}/v2/calendar", config.trading_base_url);
    let days: Vec<WireDay> = http
        .get(&url, &[("start", start.as_str()), ("end", end.as_str())])
        .await?;
    Ok(MarketCalendar::new(sessions_from_wire(&days)?)?)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::*;
    use crate::{Credentials, Feed};

    fn http() -> AlpacaHttp {
        let creds = Credentials::new("test-key-id", "test-secret");
        AlpacaHttp::new(&creds, Duration::from_secs(5)).unwrap()
    }

    fn one_day(date: &str, open: &str, close: &str) -> String {
        json!([{
            "date": date,
            "open": open,
            "close": close,
            "session_open": "0400",
            "session_close": "2000",
            "settlement_date": date,
        }])
        .to_string()
    }

    #[test]
    fn winter_session_is_utc_1430_to_2100() {
        let sessions = sessions_from_json(&one_day("2026-01-05", "09:30", "16:00")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].open, UnixNanos::from(1_767_623_400_000_000_000));
        assert_eq!(
            sessions[0].close,
            UnixNanos::from(1_767_646_800_000_000_000)
        );
        assert!(!sessions[0].is_half_day());
    }

    #[test]
    fn summer_session_is_utc_1330_to_2000() {
        let sessions = sessions_from_json(&one_day("2026-07-06", "09:30", "16:00")).unwrap();
        assert_eq!(sessions[0].open, UnixNanos::from(1_783_344_600_000_000_000));
        assert_eq!(
            sessions[0].close,
            UnixNanos::from(1_783_368_000_000_000_000)
        );
    }

    #[test]
    fn day_after_thanksgiving_is_a_half_day() {
        let sessions = sessions_from_json(&one_day("2026-11-27", "09:30", "13:00")).unwrap();
        assert_eq!(sessions[0].open, UnixNanos::from(1_795_789_800_000_000_000));
        // 13:00 EST is 18:00 UTC.
        assert_eq!(
            sessions[0].close,
            UnixNanos::from(1_795_802_400_000_000_000)
        );
        assert!(sessions[0].is_half_day());
    }

    #[test]
    fn empty_array_gives_no_sessions() {
        assert!(sessions_from_json("[]").unwrap().is_empty());
    }

    #[test]
    fn malformed_input_is_decode_error() {
        let bad = [
            one_day("2026/01/05", "09:30", "16:00"),
            one_day("2026-02-30", "09:30", "16:00"),
            one_day("2026-01-05", "9.30", "16:00"),
            one_day("2026-01-05", "25:00", "16:00"),
            one_day("2026-01-05", "09:30", ""),
            one_day("2026-01-05", "09:30:00", "16:00"),
            // 02:30 does not exist on 8 Mar 2026 (spring forward in New York).
            one_day("2026-03-08", "02:30", "16:00"),
            "{".to_owned(),
            r#"[{"date":"2026-01-05"}]"#.to_owned(),
        ];
        for json in &bad {
            assert!(
                matches!(sessions_from_json(json), Err(AdapterError::Decode(_))),
                "{json}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_sends_start_and_end_and_builds_calendar() {
        let server = MockServer::start().await;
        let body = json!([
            {"date": "2026-01-05", "open": "09:30", "close": "16:00", "session_open": "0400", "session_close": "2000", "settlement_date": "2026-01-06"},
            {"date": "2026-01-06", "open": "09:30", "close": "13:00", "session_open": "0400", "session_close": "2000", "settlement_date": "2026-01-07"},
        ]);
        Mock::given(method("GET"))
            .and(path("/v2/calendar"))
            .and(query_param("start", "2026-01-05"))
            .and(query_param("end", "2026-01-06"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let config = AlpacaConfig {
            trading_base_url: server.uri(),
            data_base_url: String::new(),
            stream_url: String::new(),
            feed: Feed::Iex,
        };
        let start = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let cal = fetch_calendar(&http(), &config, start, end).await.unwrap();
        assert_eq!(cal.sessions().len(), 2);
        assert_eq!(
            cal.session_on(start).map(|s| s.open),
            Some(UnixNanos::from(1_767_623_400_000_000_000))
        );
        assert!(cal.session_on(end).unwrap().is_half_day());
    }

    #[tokio::test]
    async fn fetch_with_no_sessions_is_core_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/calendar"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let config = AlpacaConfig {
            trading_base_url: server.uri(),
            data_base_url: String::new(),
            stream_url: String::new(),
            feed: Feed::Iex,
        };
        let day = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let err = fetch_calendar(&http(), &config, day, day)
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Core(_)));
    }
}
