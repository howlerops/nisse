//! Exchange calendar: the regular trading sessions, including half-days.
//!
//! The calendar is data handed to the engine (loaded from the broker or a file), never a
//! lookup made while deciding, so backtest and live see the same sessions. It fails closed:
//! outside the range it covers the market is treated as shut.

use chrono::NaiveDate;
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

#[allow(unused_variables, reason = "stub: Phase 1 task T4 implements these")]
impl MarketCalendar {
    /// Builds a calendar.
    ///
    /// Errors with [`CoreError::InvalidCalendar`] when `sessions` is empty, a session has
    /// `open >= close`, dates are not strictly increasing, or a session opens before the
    /// previous one closed.
    pub fn new(sessions: Vec<Session>) -> Result<Self, CoreError> {
        todo!("Phase 1 task T4")
    }

    /// All sessions, in date order.
    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// The session on an exchange-local date, if that date is a trading day. Binary search.
    #[must_use]
    pub fn session_on(&self, date: NaiveDate) -> Option<&Session> {
        todo!("Phase 1 task T4")
    }

    /// The session containing `ts` (`open <= ts < close`). Binary search; no allocation.
    #[must_use]
    pub fn session_at(&self, ts: UnixNanos) -> Option<&Session> {
        todo!("Phase 1 task T4")
    }

    /// Whether the market is open at `ts`. `false` outside the covered range.
    #[must_use]
    pub fn is_open(&self, ts: UnixNanos) -> bool {
        self.session_at(ts).is_some()
    }

    /// The first session that opens strictly after `ts`, if the calendar reaches that far.
    #[must_use]
    pub fn next_session(&self, ts: UnixNanos) -> Option<&Session> {
        todo!("Phase 1 task T4")
    }

    /// Whether `date` is the last trading day of its calendar month. `None` when `date` is
    /// not a session, or it is the final session in the calendar (the answer is unknown).
    #[must_use]
    pub fn is_last_session_of_month(&self, date: NaiveDate) -> Option<bool> {
        todo!("Phase 1 task T4")
    }
}
