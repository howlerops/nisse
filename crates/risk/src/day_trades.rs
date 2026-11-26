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

impl DayTradeTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one fill. Entries older than the window (relative to this fill's day) are
    /// removed, so the map stays small.
    pub fn record_fill(&mut self, symbol: Symbol, side: OrderSide, ts: UnixNanos) {
        let (bought, sold) = match side {
            OrderSide::Buy => (true, false),
            OrderSide::Sell => (false, true),
        };
        let day = day_of(ts);
        let entry = self.sides.entry((day, symbol)).or_insert((false, false));
        entry.0 |= bought;
        entry.1 |= sold;

        // Prune relative to the newest day seen, so an out-of-order fill never removes newer
        // data. The map is ordered by day, so its newest day is the last key.
        let newest = self
            .sides
            .keys()
            .next_back()
            .map_or(day, |&(newest_day, _)| newest_day.max(day));
        let oldest_kept = window_start(newest);
        self.sides.retain(|&(d, _), _| d >= oldest_kept);
    }

    /// Whether an order on `side` for `symbol`, filled on the day of `now`, would complete a
    /// day trade that is not already counted: the opposite side has filled that day and this
    /// side has not.
    #[must_use]
    pub fn would_day_trade(&self, symbol: Symbol, side: OrderSide, now: UnixNanos) -> bool {
        let Some(&(bought, sold)) = self.sides.get(&(day_of(now), symbol)) else {
            return false;
        };
        match side {
            OrderSide::Buy => sold && !bought,
            OrderSide::Sell => bought && !sold,
        }
    }

    /// Day trades on the day of `now` and the six days before it.
    #[must_use]
    pub fn count_in_window(&self, now: UnixNanos) -> u32 {
        let today = day_of(now);
        let oldest = window_start(today);
        let count = self
            .sides
            .iter()
            .filter(|&(&(d, _), &(bought, sold))| d >= oldest && d <= today && bought && sold)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// The UTC day number of `ts`.
fn day_of(ts: UnixNanos) -> u64 {
    ts.as_u64() / DAY_NANOS
}

/// The first day of the window that ends on `day`.
fn window_start(day: u64) -> u64 {
    day.saturating_sub(WINDOW_DAYS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(day: u64, hour: u64) -> UnixNanos {
        UnixNanos::from(day * DAY_NANOS + hour * 3_600_000_000_000)
    }

    fn aaa() -> Symbol {
        Symbol::new("AAA")
    }

    fn bbb() -> Symbol {
        Symbol::new("BBB")
    }

    #[test]
    fn fresh_tracker_counts_nothing() {
        let t = DayTradeTracker::new();
        assert_eq!(t.count_in_window(ts(10, 12)), 0);
        assert!(!t.would_day_trade(aaa(), OrderSide::Buy, ts(10, 12)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Sell, ts(10, 12)));
    }

    #[test]
    fn buy_then_would_be_day_trade_on_sell_but_not_counted() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        assert!(t.would_day_trade(aaa(), OrderSide::Sell, ts(10, 12)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Buy, ts(10, 12)));
        assert!(!t.would_day_trade(bbb(), OrderSide::Sell, ts(10, 12)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Sell, ts(11, 12)));
        assert_eq!(t.count_in_window(ts(10, 12)), 0);
    }

    #[test]
    fn sell_completes_round_trip_and_is_counted_once() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(10, 14));
        assert_eq!(t.count_in_window(ts(10, 12)), 1);
        assert!(!t.would_day_trade(aaa(), OrderSide::Sell, ts(10, 12)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Buy, ts(10, 12)));
    }

    #[test]
    fn sell_first_then_buy_is_a_day_trade() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Sell, ts(10, 10));
        assert!(t.would_day_trade(aaa(), OrderSide::Buy, ts(10, 12)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Sell, ts(10, 12)));
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 14));
        assert_eq!(t.count_in_window(ts(10, 15)), 1);
    }

    #[test]
    fn repeated_fills_on_one_side_are_not_a_day_trade() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 11));
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 12));
        assert_eq!(t.count_in_window(ts(10, 13)), 0);
        assert!(t.would_day_trade(aaa(), OrderSide::Sell, ts(10, 13)));
        assert!(!t.would_day_trade(aaa(), OrderSide::Buy, ts(10, 13)));
    }

    #[test]
    fn two_symbols_or_two_days_each_count() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(10, 11));
        t.record_fill(bbb(), OrderSide::Sell, ts(10, 12));
        t.record_fill(bbb(), OrderSide::Buy, ts(10, 13));
        assert_eq!(t.count_in_window(ts(10, 23)), 2);

        t.record_fill(aaa(), OrderSide::Buy, ts(11, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(11, 11));
        assert_eq!(t.count_in_window(ts(11, 12)), 3);
    }

    #[test]
    fn same_symbol_two_days_counts_two() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(10, 11));
        t.record_fill(aaa(), OrderSide::Buy, ts(11, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(11, 11));
        assert_eq!(t.count_in_window(ts(11, 12)), 2);
    }

    #[test]
    fn day_trade_is_in_window_for_six_days_after() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(10, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(10, 11));
        for day in 10..=16 {
            assert_eq!(t.count_in_window(ts(day, 12)), 1, "day {day}");
        }
        assert_eq!(t.count_in_window(ts(17, 12)), 0);
        assert_eq!(t.count_in_window(ts(9, 12)), 0);
    }

    #[test]
    fn pruning_drops_entries_older_than_window() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(23, 10));
        t.record_fill(aaa(), OrderSide::Sell, ts(23, 11));
        t.record_fill(bbb(), OrderSide::Buy, ts(24, 10));
        t.record_fill(bbb(), OrderSide::Sell, ts(24, 11));
        assert_eq!(t.sides.len(), 2);

        t.record_fill(aaa(), OrderSide::Buy, ts(30, 10));
        // Window ending on day 30 starts on day 24: day 23 is gone, day 24 remains.
        let days: Vec<u64> = t.sides.keys().map(|&(d, _)| d).collect();
        assert_eq!(days, vec![24, 30]);
        assert_eq!(t.count_in_window(ts(30, 12)), 1);
        assert!(t.would_day_trade(aaa(), OrderSide::Sell, ts(30, 12)));
    }

    #[test]
    fn out_of_order_old_fill_does_not_prune_newer_data() {
        let mut t = DayTradeTracker::new();
        t.record_fill(aaa(), OrderSide::Buy, ts(30, 10));
        t.record_fill(bbb(), OrderSide::Buy, ts(29, 10));
        let before = t.clone();

        t.record_fill(aaa(), OrderSide::Sell, ts(5, 10));
        assert_eq!(t, before);
        assert!(t.would_day_trade(aaa(), OrderSide::Sell, ts(30, 12)));
        assert!(t.would_day_trade(bbb(), OrderSide::Sell, ts(29, 12)));
    }

    #[test]
    fn utc_day_boundary_is_not_a_day_trade() {
        let mut t = DayTradeTracker::new();
        let late_day_10 = UnixNanos::from(11 * DAY_NANOS - 1);
        let day_11_start = UnixNanos::from(11 * DAY_NANOS);
        t.record_fill(aaa(), OrderSide::Buy, late_day_10);
        t.record_fill(aaa(), OrderSide::Sell, day_11_start);
        assert_eq!(t.count_in_window(late_day_10), 0);
        assert_eq!(t.count_in_window(day_11_start), 0);
        assert!(!t.would_day_trade(aaa(), OrderSide::Sell, day_11_start));
    }
}
