//! Alpaca calendar endpoint (`GET /v2/calendar`) to [`MarketCalendar`].

use chrono::NaiveDate;
use nisse_core::{MarketCalendar, Session};

use crate::{AdapterError, AlpacaConfig, AlpacaHttp};

/// Converts Alpaca calendar entries to sessions. `date` is `YYYY-MM-DD`; `open` and `close`
/// are `HH:MM` in America/New_York local time (so a half-day shows `close` of `13:00`) and are
/// converted to UTC with the tz database, which handles daylight saving.
#[allow(unused_variables, reason = "stub: Phase 1 task T4")]
pub(crate) fn sessions_from_json(json: &str) -> Result<Vec<Session>, AdapterError> {
    todo!("Phase 1 task T4")
}

/// Fetches the trading calendar for `start..=end` and builds a [`MarketCalendar`].
#[allow(unused_variables, reason = "stub: Phase 1 task T4")]
pub async fn fetch_calendar(
    http: &AlpacaHttp,
    config: &AlpacaConfig,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<MarketCalendar, AdapterError> {
    todo!("Phase 1 task T4")
}
