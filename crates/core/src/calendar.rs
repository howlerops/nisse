//! Exchange calendar: the regular trading sessions, including half-days.
//!
//! The calendar is data handed to the engine (loaded from the broker or a file), never a
//! lookup made while deciding, so backtest and live see the same sessions. It fails closed:
//! outside the range it covers the market is treated as shut.

use chrono::{Datelike, NaiveDate};
use nautilus_core::UnixNanos;

use crate::CoreError;

/// A regular session shorter than this is a half-day (a normal US session is 6.5 hours).
pub const FULL_SESSION_NANOS: u64 = 6 * 3_600_000_000_000 + 1_800_000_000_000;

/// One regular trading session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    /// The exchange-local calendar date.
    pub date: NaiveDate,
    pub open: UnixNanos,
    /// Exclusive: the market is open for `open <= t < close`.
    pub close: UnixNanos,
}

impl Session {
    /// The session closes early.
    #[must_use]
    pub fn is_half_day(&self) -> bool {
        self.close.as_u64().saturating_sub(self.open.as_u64()) < FULL_SESSION_NANOS
    }
}

/// Trading sessions in date order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketCalendar {
    sessions: Vec<Session>,
}

impl MarketCalendar {
    /// Builds a calendar from session close times alone, for backtests that have bars but no
    /// broker calendar. Each close becomes a session dated by the UTC date of the close
    /// (for a US close that is the same calendar day as in New York) and assumed to have
    /// opened [`FULL_SESSION_NANOS`] earlier, so half-days are not recognised: use the
    /// broker's calendar when that matters. `closes` must be strictly
    /// increasing; duplicates are an `InvalidCalendar` error.
    pub fn from_closes(closes: &[UnixNanos]) -> Result<Self, CoreError> {
        let sessions = closes
            .iter()
            .map(|close| {
                let date = i64::try_from(close.as_u64())
                    .ok()
                    .map(chrono::DateTime::from_timestamp_nanos)
                    .map(|ts| ts.date_naive())
                    .ok_or(CoreError::InvalidCalendar("close time out of range"))?;
                Ok(Session {
                    date,
                    open: UnixNanos::from(close.as_u64().saturating_sub(FULL_SESSION_NANOS)),
                    close: *close,
                })
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        Self::new(sessions)
    }

    /// Builds a calendar.
    ///
    /// Errors with [`CoreError::InvalidCalendar`] when `sessions` is empty, a session has
    /// `open >= close`, dates are not strictly increasing, or a session opens before the
    /// previous one closed.
    pub fn new(sessions: Vec<Session>) -> Result<Self, CoreError> {
        if sessions.is_empty() {
            return Err(CoreError::InvalidCalendar("no sessions"));
        }
        if sessions.iter().any(|s| s.open.as_u64() >= s.close.as_u64()) {
            return Err(CoreError::InvalidCalendar(
                "session opens at or after its close",
            ));
        }
        for pair in sessions.windows(2) {
            if let [prev, next] = pair {
                if next.date == prev.date {
                    return Err(CoreError::InvalidCalendar("duplicate session date"));
                }
                if next.date < prev.date {
                    return Err(CoreError::InvalidCalendar(
                        "session dates are not increasing",
                    ));
                }
                if next.open.as_u64() < prev.close.as_u64() {
                    return Err(CoreError::InvalidCalendar("sessions overlap"));
                }
            }
        }
        Ok(Self { sessions })
    }

    /// All sessions, in date order.
    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Index of the session on `date`, if that date is a trading day.
    fn index_on(&self, date: NaiveDate) -> Option<usize> {
        self.sessions.binary_search_by(|s| s.date.cmp(&date)).ok()
    }

    /// Number of sessions that have opened at or before `ts`.
    fn opened_by(&self, ts: UnixNanos) -> usize {
        let ts = ts.as_u64();
        self.sessions.partition_point(|s| s.open.as_u64() <= ts)
    }

    /// The session on an exchange-local date, if that date is a trading day. Binary search.
    #[must_use]
    pub fn session_on(&self, date: NaiveDate) -> Option<&Session> {
        self.sessions.get(self.index_on(date)?)
    }

    /// The session containing `ts` (`open <= ts < close`). Binary search; no allocation.
    #[must_use]
    pub fn session_at(&self, ts: UnixNanos) -> Option<&Session> {
        let session = self.sessions.get(self.opened_by(ts).checked_sub(1)?)?;
        (ts.as_u64() < session.close.as_u64()).then_some(session)
    }

    /// Whether the market is open at `ts`. `false` outside the covered range.
    #[must_use]
    pub fn is_open(&self, ts: UnixNanos) -> bool {
        self.session_at(ts).is_some()
    }

    /// The first session that opens strictly after `ts`, if the calendar reaches that far.
    #[must_use]
    pub fn next_session(&self, ts: UnixNanos) -> Option<&Session> {
        self.sessions.get(self.opened_by(ts))
    }

    /// Whether `date` is the last trading day of its calendar month. `None` when `date` is
    /// not a session, or it is the final session in the calendar (the answer is unknown).
    #[must_use]
    pub fn is_last_session_of_month(&self, date: NaiveDate) -> Option<bool> {
        let next = self.sessions.get(self.index_on(date)? + 1)?;
        Some(next.date.year() != date.year() || next.date.month() != date.month())
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use nautilus_core::UnixNanos;

    use super::{FULL_SESSION_NANOS, MarketCalendar, Session};
    use crate::CoreError;

    /// `hour:minute` UTC on `date`, in nanoseconds since the epoch.
    fn utc(date: NaiveDate, hour: u32, minute: u32) -> UnixNanos {
        let secs = date
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        UnixNanos::from(u64::try_from(secs).unwrap() * 1_000_000_000)
    }

    /// A session on `y-m-d` from `open_hour_utc:00` to `close_hour_utc:00` UTC.
    fn session(y: i32, m: u32, d: u32, open_hour_utc: u32, close_hour_utc: u32) -> Session {
        let date = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        Session {
            date,
            open: utc(date, open_hour_utc, 0),
            close: utc(date, close_hour_utc, 0),
        }
    }

    /// Mon 5 Jan, Tue 6 Jan and Fri 9 Jan 2026, 14:00 to 21:00 UTC.
    fn week() -> MarketCalendar {
        MarketCalendar::new(vec![
            session(2026, 1, 5, 14, 21),
            session(2026, 1, 6, 14, 21),
            session(2026, 1, 9, 14, 21),
        ])
        .unwrap()
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, d).unwrap()
    }

    #[test]
    fn new_rejects_empty() {
        assert_eq!(
            MarketCalendar::new(vec![]),
            Err(CoreError::InvalidCalendar("no sessions"))
        );
    }

    #[test]
    fn new_rejects_open_not_before_close() {
        let backwards = session(2026, 1, 5, 21, 14);
        assert!(matches!(
            MarketCalendar::new(vec![backwards]),
            Err(CoreError::InvalidCalendar(_))
        ));
        let empty = Session {
            close: utc(day(5), 14, 0),
            ..session(2026, 1, 5, 14, 21)
        };
        assert!(MarketCalendar::new(vec![empty]).is_err());
    }

    #[test]
    fn new_rejects_unsorted_dates() {
        let unsorted = vec![session(2026, 1, 6, 14, 21), session(2026, 1, 5, 14, 21)];
        assert!(matches!(
            MarketCalendar::new(unsorted),
            Err(CoreError::InvalidCalendar(_))
        ));
    }

    #[test]
    fn new_rejects_duplicate_dates() {
        let dup = vec![session(2026, 1, 5, 14, 21), session(2026, 1, 5, 14, 21)];
        assert_eq!(
            MarketCalendar::new(dup),
            Err(CoreError::InvalidCalendar("duplicate session date"))
        );
    }

    #[test]
    fn new_rejects_overlapping_sessions() {
        // Dated 6 Jan but opening at 20:00 UTC on 5 Jan, before the previous close at 21:00.
        let next = Session {
            date: day(6),
            open: utc(day(5), 20, 0),
            close: utc(day(6), 22, 0),
        };
        let overlap = vec![session(2026, 1, 5, 14, 21), next];
        assert_eq!(
            MarketCalendar::new(overlap),
            Err(CoreError::InvalidCalendar("sessions overlap"))
        );
    }

    #[test]
    fn new_accepts_sessions_that_touch() {
        // The next session opens exactly when the previous one closes.
        let touching = vec![session(2026, 1, 5, 14, 21), session(2026, 1, 6, 21, 23)];
        assert!(MarketCalendar::new(touching).is_ok());
    }

    #[test]
    fn session_on_hit_and_weekend_miss() {
        let cal = week();
        assert_eq!(cal.session_on(day(6)).map(|s| s.date), Some(day(6)));
        // 10 Jan 2026 is a Saturday.
        assert_eq!(cal.session_on(day(10)), None);
        assert_eq!(cal.session_on(day(1)), None);
    }

    #[test]
    fn session_at_boundaries() {
        let cal = week();
        let s = session(2026, 1, 5, 14, 21);
        // open is in.
        assert_eq!(cal.session_at(s.open), Some(&s));
        // close - 1 ns is in.
        let just_before_close = UnixNanos::from(s.close.as_u64() - 1);
        assert_eq!(cal.session_at(just_before_close), Some(&s));
        // close is out.
        assert_eq!(cal.session_at(s.close), None);
        // one ns before open is out.
        assert_eq!(cal.session_at(UnixNanos::from(s.open.as_u64() - 1)), None);
    }

    #[test]
    fn session_at_between_and_outside_coverage() {
        let cal = week();
        // Between Mon's close and Tue's open.
        assert_eq!(cal.session_at(utc(day(5), 22, 0)), None);
        // Before the first session.
        assert_eq!(cal.session_at(utc(day(1), 15, 0)), None);
        // After the last session.
        assert_eq!(cal.session_at(utc(day(12), 15, 0)), None);
        assert!(!cal.is_open(utc(day(1), 15, 0)));
        assert!(cal.is_open(utc(day(6), 15, 0)));
    }

    #[test]
    fn next_session_from_before_first_mid_session_and_after_last() {
        let cal = week();
        let tue = session(2026, 1, 6, 14, 21);
        assert_eq!(
            cal.next_session(utc(day(1), 0, 0)).map(|s| s.date),
            Some(day(5))
        );
        // Mid-session on Monday: the following session.
        assert_eq!(cal.next_session(utc(day(5), 16, 0)), Some(&tue));
        // Exactly at an open: strictly after, so the following session.
        assert_eq!(cal.next_session(utc(day(5), 14, 0)), Some(&tue));
        assert_eq!(cal.next_session(utc(day(9), 15, 0)), None);
    }

    #[test]
    fn is_last_session_of_month_cases() {
        let cal = MarketCalendar::new(vec![
            session(2026, 1, 5, 14, 21),
            session(2026, 1, 30, 14, 21),
            session(2026, 2, 2, 14, 21),
        ])
        .unwrap();
        // Not the last one of January.
        assert_eq!(cal.is_last_session_of_month(day(5)), Some(false));
        // Last one of January, followed by February.
        assert_eq!(cal.is_last_session_of_month(day(30)), Some(true));
        // 31 Jan 2026 is a Saturday, not a session.
        assert_eq!(cal.is_last_session_of_month(day(31)), None);
        // Final session in the calendar: unknown.
        let feb = NaiveDate::from_ymd_opt(2026, 2, 2).unwrap();
        assert_eq!(cal.is_last_session_of_month(feb), None);
    }

    #[test]
    fn is_half_day_for_early_close() {
        let half = Session {
            date: day(9),
            open: utc(day(9), 14, 30),
            close: utc(day(9), 18, 0),
        };
        assert!(half.is_half_day());
        let full = Session {
            close: utc(day(9), 21, 0),
            ..half
        };
        assert!(!full.is_half_day());
        assert_eq!(full.close.as_u64() - full.open.as_u64(), FULL_SESSION_NANOS);
    }
}
