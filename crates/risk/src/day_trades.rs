//! Counting day trades: a buy and a sell of the same symbol on the same UTC day.

use std::collections::BTreeMap;

use nisse_core::nautilus::{OrderSide, Symbol, UnixNanos};

/// Nanoseconds in a UTC day.
pub const DAY_NANOS: u64 = 86_400_000_000_000;
/// The window day trades are counted over: 7 calendar days, which always covers 5 sessions.
pub const WINDOW_DAYS: u64 = 7;

/// Which sides of each symbol filled on which UTC day, and the day trades that made.
///
/// A day trade is counted once per symbol per day, the first time the symbol has both a buy
/// fill and a sell fill on that day.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DayTradeTracker {
    /// `(day, symbol)` → `(bought, sold)` on that day. Days older than the window are
    /// dropped as new fills arrive.
    sides: BTreeMap<(u64, Symbol), (bool, bool)>,
}

#[allow(unused_variables, reason = "stub: Phase 4 task R3")]
impl DayTradeTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one fill. Entries older than the window (relative to this fill's day) are
    /// removed, so the map stays small.
    pub fn record_fill(&mut self, symbol: Symbol, side: OrderSide, ts: UnixNanos) {
        todo!("Phase 4 task R3")
    }

    /// Whether an order on `side` for `symbol`, filled on the day of `now`, would complete a
    /// day trade that is not already counted: the opposite side has filled that day and this
    /// side has not.
    #[must_use]
    pub fn would_day_trade(&self, symbol: Symbol, side: OrderSide, now: UnixNanos) -> bool {
        todo!("Phase 4 task R3")
    }

    /// Day trades on the day of `now` and the six days before it.
    #[must_use]
    pub fn count_in_window(&self, now: UnixNanos) -> u32 {
        todo!("Phase 4 task R3")
    }
}
