//! The engine's own ledger of positions, kept from fills and corrected from the broker.

use std::collections::BTreeMap;

use nisse_core::{
    PositionEvent,
    nautilus::{OrderSide, Symbol, UnixNanos},
};
use rust_decimal::Decimal;

/// One position: signed quantity and the average cost of what is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    /// Negative when short.
    pub qty: Decimal,
    /// Average price paid (long) or received (short) for the open quantity. Zero when flat.
    pub avg_cost: Decimal,
}

/// Positions by symbol. A `BTreeMap`, so iteration order is deterministic. Flat positions are
/// not stored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PositionBook {
    positions: BTreeMap<Symbol, Position>,
}

impl PositionBook {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The position in `symbol`; flat (`Position::default()`) when there is none.
    #[must_use]
    pub fn get(&self, symbol: Symbol) -> Position {
        self.positions.get(&symbol).copied().unwrap_or_default()
    }

    /// Signed quantity per symbol, in the form `sync_with_broker` reconciles.
    #[must_use]
    pub fn quantities(&self) -> BTreeMap<Symbol, Decimal> {
        self.positions
            .iter()
            .map(|(symbol, position)| (*symbol, position.qty))
            .collect()
    }

    /// Every held position, in symbol order.
    pub fn iter(&self) -> impl Iterator<Item = (Symbol, Position)> + '_ {
        self.positions
            .iter()
            .map(|(symbol, position)| (*symbol, *position))
    }

    /// Applies one fill (`qty` and `px` positive; `side` gives the direction) and returns the
    /// row to record. Rules, with `signed = +qty` for a buy and `-qty` for a sell:
    ///
    /// - Adding to a position, or opening one from flat: quantity grows and `avg_cost` becomes
    ///   the quantity-weighted average of the old cost and `px`.
    /// - Reducing a position without crossing zero: quantity shrinks, `avg_cost` unchanged.
    /// - Closing exactly: the position is removed; the event has `qty_after` 0, `avg_cost` 0.
    /// - Crossing zero (a long sold past flat, or the reverse): the remainder is a new
    ///   position opened at `px`, so `avg_cost` becomes `px`.
    ///
    /// If the quantity sum overflows, the book is left unchanged; if only the weighted average
    /// overflows, the previous `avg_cost` is kept.
    pub fn apply_fill(
        &mut self,
        symbol: Symbol,
        side: OrderSide,
        qty: Decimal,
        px: Decimal,
        ts: UnixNanos,
    ) -> PositionEvent {
        let old = self.get(symbol);
        let signed = signed_qty(side, qty);
        let Some(new_qty) = old.qty.checked_add(signed) else {
            return current_event(symbol, old, ts);
        };
        if new_qty.is_zero() {
            self.positions.remove(&symbol);
            return PositionEvent {
                symbol,
                qty_after: Decimal::ZERO,
                avg_cost: Decimal::ZERO,
                ts,
            };
        }
        let raw_cost = if old.qty.is_zero() || same_sign(old.qty, signed) {
            weighted_cost(old, qty, px, new_qty).unwrap_or(old.avg_cost)
        } else if same_sign(new_qty, old.qty) {
            old.avg_cost
        } else {
            px
        };
        let avg_cost = raw_cost.normalize();
        self.positions.insert(
            symbol,
            Position {
                qty: new_qty,
                avg_cost,
            },
        );
        PositionEvent {
            symbol,
            qty_after: new_qty,
            avg_cost,
            ts,
        }
    }

    /// Overwrites the position with the broker's figures (the broker is the source of truth)
    /// and returns the row to record, or `None` when nothing changed. A `broker_qty` of zero
    /// removes the position. `broker_avg_cost` is used when given; when it is `None` the old
    /// cost is kept if the sign of the quantity did not change, else it becomes zero.
    pub fn adopt(
        &mut self,
        symbol: Symbol,
        broker_qty: Decimal,
        broker_avg_cost: Option<Decimal>,
        ts: UnixNanos,
    ) -> Option<PositionEvent> {
        let old = self.get(symbol);
        let raw_cost = if broker_qty.is_zero() {
            Decimal::ZERO
        } else if let Some(cost) = broker_avg_cost {
            cost
        } else if same_sign(broker_qty, old.qty) {
            old.avg_cost
        } else {
            Decimal::ZERO
        };
        let new_cost = raw_cost.normalize();
        if broker_qty == old.qty && new_cost == old.avg_cost {
            return None;
        }
        if broker_qty.is_zero() {
            self.positions.remove(&symbol);
        } else {
            self.positions.insert(
                symbol,
                Position {
                    qty: broker_qty,
                    avg_cost: new_cost,
                },
            );
        }
        Some(PositionEvent {
            symbol,
            qty_after: broker_qty,
            avg_cost: new_cost,
            ts,
        })
    }

    /// Puts back a position read from the store at start-up (the newest `position_event` row
    /// for the symbol). Unlike `adopt` it returns no row: nothing changed, so nothing is
    /// recorded. A zero `qty` removes the entry. `avg_cost` is stored normalized.
    pub fn restore(&mut self, symbol: Symbol, qty: Decimal, avg_cost: Decimal) {
        if qty.is_zero() {
            self.positions.remove(&symbol);
        } else {
            self.positions.insert(
                symbol,
                Position {
                    qty,
                    avg_cost: avg_cost.normalize(),
                },
            );
        }
    }
}

/// The signed quantity a fill adds. The pinned `OrderSide` has only buy and sell variants.
fn signed_qty(side: OrderSide, qty: Decimal) -> Decimal {
    match side {
        OrderSide::Buy => qty,
        OrderSide::Sell => -qty,
    }
}

/// True when both are strictly positive or both strictly negative.
fn same_sign(a: Decimal, b: Decimal) -> bool {
    (a > Decimal::ZERO && b > Decimal::ZERO) || (a < Decimal::ZERO && b < Decimal::ZERO)
}

/// The event that reports a position unchanged.
fn current_event(symbol: Symbol, position: Position, ts: UnixNanos) -> PositionEvent {
    PositionEvent {
        symbol,
        qty_after: position.qty,
        avg_cost: position.avg_cost,
        ts,
    }
}

/// Quantity-weighted average of the old cost and the fill price over `new_qty`'s magnitude.
/// `None` on overflow (or a zero divisor, which the caller rules out).
fn weighted_cost(old: Position, qty: Decimal, px: Decimal, new_qty: Decimal) -> Option<Decimal> {
    let old_notional = old.qty.abs().checked_mul(old.avg_cost)?;
    let fill_notional = qty.checked_mul(px)?;
    old_notional
        .checked_add(fill_notional)?
        .checked_div(new_qty.abs())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests build decimals from literals")]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse::<Decimal>().unwrap()
    }

    fn ts() -> UnixNanos {
        UnixNanos::from(1_u64)
    }

    fn pos(qty: &str, avg_cost: &str) -> Position {
        Position {
            qty: d(qty),
            avg_cost: d(avg_cost),
        }
    }

    fn fill(
        book: &mut PositionBook,
        symbol: Symbol,
        side: OrderSide,
        qty: &str,
        px: &str,
    ) -> PositionEvent {
        book.apply_fill(symbol, side, d(qty), d(px), ts())
    }

    fn event(symbol: Symbol, qty_after: &str, avg_cost: &str) -> PositionEvent {
        PositionEvent {
            symbol,
            qty_after: d(qty_after),
            avg_cost: d(avg_cost),
            ts: ts(),
        }
    }

    #[test]
    fn open_long_from_flat() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        let ev = fill(&mut book, spy, OrderSide::Buy, "10", "100");
        assert_eq!(ev, event(spy, "10", "100"));
        assert_eq!(book.get(spy), pos("10", "100"));
    }

    #[test]
    fn add_to_long_uses_weighted_average() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = fill(&mut book, spy, OrderSide::Buy, "10", "110");
        assert_eq!(ev, event(spy, "20", "105"));
        assert_eq!(book.get(spy), pos("20", "105"));
    }

    #[test]
    fn reduce_long_keeps_cost() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        fill(&mut book, spy, OrderSide::Buy, "10", "110");
        let ev = fill(&mut book, spy, OrderSide::Sell, "5", "120");
        assert_eq!(ev, event(spy, "15", "105"));
        assert_eq!(book.get(spy), pos("15", "105"));
    }

    #[test]
    fn close_long_exactly_removes_position() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        fill(&mut book, spy, OrderSide::Buy, "10", "110");
        fill(&mut book, spy, OrderSide::Sell, "5", "120");
        let ev = fill(&mut book, spy, OrderSide::Sell, "15", "90");
        assert_eq!(ev, event(spy, "0", "0"));
        assert_eq!(book.get(spy), Position::default());
        assert!(book.quantities().is_empty());
    }

    #[test]
    fn cross_long_to_short_opens_remainder_at_fill_price() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = fill(&mut book, spy, OrderSide::Sell, "25", "95");
        assert_eq!(ev, event(spy, "-15", "95"));
        assert_eq!(book.get(spy), pos("-15", "95"));
    }

    #[test]
    fn open_short_from_flat() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        let ev = fill(&mut book, spy, OrderSide::Sell, "10", "100");
        assert_eq!(ev, event(spy, "-10", "100"));
        assert_eq!(book.get(spy), pos("-10", "100"));
    }

    #[test]
    fn add_to_short_uses_weighted_average() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Sell, "10", "100");
        let ev = fill(&mut book, spy, OrderSide::Sell, "10", "90");
        assert_eq!(ev, event(spy, "-20", "95"));
        assert_eq!(book.get(spy), pos("-20", "95"));
    }

    #[test]
    fn reduce_short_keeps_cost() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Sell, "10", "100");
        fill(&mut book, spy, OrderSide::Sell, "10", "90");
        let ev = fill(&mut book, spy, OrderSide::Buy, "5", "80");
        assert_eq!(ev, event(spy, "-15", "95"));
        assert_eq!(book.get(spy), pos("-15", "95"));
    }

    #[test]
    fn cross_short_to_long_opens_remainder_at_fill_price() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Sell, "10", "100");
        let ev = fill(&mut book, spy, OrderSide::Buy, "25", "70");
        assert_eq!(ev, event(spy, "15", "70"));
        assert_eq!(book.get(spy), pos("15", "70"));
    }

    #[test]
    fn close_short_exactly_removes_position() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Sell, "10", "100");
        let ev = fill(&mut book, spy, OrderSide::Buy, "10", "90");
        assert_eq!(ev, event(spy, "0", "0"));
        assert_eq!(book.get(spy), Position::default());
        assert!(book.quantities().is_empty());
    }

    #[test]
    fn fractional_quantities_stay_exact() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "0.1", "10");
        let ev = fill(&mut book, spy, OrderSide::Buy, "0.2", "10");
        assert_eq!(ev.qty_after, d("0.3"));
        assert_eq!(ev.avg_cost, d("10"));
        assert_eq!(book.get(spy).qty, d("0.3"));
    }

    #[test]
    fn weighted_average_is_not_rounded() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "1", "10");
        let ev = fill(&mut book, spy, OrderSide::Buy, "2", "11");
        assert_eq!(ev.qty_after, d("3"));
        assert_eq!(ev.avg_cost, d("32") / d("3"));
        assert_eq!(book.get(spy).avg_cost, d("32") / d("3"));
    }

    #[test]
    fn normalized_average_drops_trailing_zeros() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        let ev = fill(&mut book, spy, OrderSide::Buy, "1", "105.00");
        assert_eq!(ev.avg_cost, d("105"));
        assert_eq!(ev.avg_cost.scale(), 0);
        assert_eq!(book.get(spy).avg_cost.scale(), 0);
    }

    #[test]
    fn unknown_symbol_is_flat() {
        let book = PositionBook::new();
        assert_eq!(book.get(Symbol::new("NOPE")), Position::default());
        assert!(book.quantities().is_empty());
    }

    #[test]
    fn symbols_are_independent_and_listed_in_order() {
        let spy = Symbol::new("SPY");
        let qqq = Symbol::new("QQQ");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        fill(&mut book, qqq, OrderSide::Sell, "4", "50");

        assert_eq!(book.get(spy), pos("10", "100"));
        assert_eq!(book.get(qqq), pos("-4", "50"));

        let quantities = book.quantities();
        let listed: Vec<(Symbol, Decimal)> = quantities.into_iter().collect();
        assert_eq!(listed, vec![(qqq, d("-4")), (spy, d("10"))]);
    }

    #[test]
    fn overflowing_quantity_leaves_book_unchanged() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        book.apply_fill(spy, OrderSide::Buy, Decimal::MAX, d("1"), ts());
        let before = book.clone();
        let ev = fill(&mut book, spy, OrderSide::Buy, "1", "1");
        assert_eq!(ev.qty_after, Decimal::MAX);
        assert_eq!(ev.avg_cost, d("1"));
        assert_eq!(book, before);
    }

    #[test]
    fn overflowing_average_keeps_previous_cost() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        book.apply_fill(spy, OrderSide::Buy, d("1"), Decimal::MAX, ts());
        let ev = book.apply_fill(spy, OrderSide::Buy, d("1"), Decimal::MAX, ts());
        assert_eq!(ev.qty_after, d("2"));
        assert_eq!(ev.avg_cost, Decimal::MAX);
        assert_eq!(
            book.get(spy),
            Position {
                qty: d("2"),
                avg_cost: Decimal::MAX
            }
        );
    }

    #[test]
    fn adopt_same_figures_is_none() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        assert_eq!(book.adopt(spy, d("10"), Some(d("100")), ts()), None);
        assert_eq!(book.get(spy), pos("10", "100"));
    }

    #[test]
    fn adopt_five_equals_five_point_zero() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "5", "100");
        assert_eq!(book.adopt(spy, d("5.0"), Some(d("100.0")), ts()), None);
        assert_eq!(book.get(spy), pos("5", "100"));
    }

    #[test]
    fn adopt_different_quantity_with_broker_average() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = book.adopt(spy, d("12"), Some(d("101")), ts());
        assert_eq!(ev, Some(event(spy, "12", "101")));
        assert_eq!(book.get(spy), pos("12", "101"));
    }

    #[test]
    fn adopt_none_average_keeps_cost_when_sign_unchanged() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = book.adopt(spy, d("15"), None, ts());
        assert_eq!(ev, Some(event(spy, "15", "100")));
        assert_eq!(book.get(spy), pos("15", "100"));
    }

    #[test]
    fn adopt_none_average_zeroes_cost_when_sign_flips() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = book.adopt(spy, d("-5"), None, ts());
        assert_eq!(ev, Some(event(spy, "-5", "0")));
        assert_eq!(book.get(spy), pos("-5", "0"));
    }

    #[test]
    fn adopt_flat_removes_held_position() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        let ev = book.adopt(spy, d("0"), None, ts());
        assert_eq!(ev, Some(event(spy, "0", "0")));
        assert_eq!(book.get(spy), Position::default());
        assert!(book.quantities().is_empty());
    }

    #[test]
    fn adopt_flat_when_already_flat_is_none() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        assert_eq!(book.adopt(spy, d("0"), None, ts()), None);
        assert_eq!(book.adopt(spy, d("0"), Some(d("50")), ts()), None);
        assert!(book.quantities().is_empty());
    }

    #[test]
    fn adopt_unseen_symbol_is_stored() {
        let qqq = Symbol::new("QQQ");
        let mut book = PositionBook::new();
        let ev = book.adopt(qqq, d("7"), Some(d("3")), ts());
        assert_eq!(ev, Some(event(qqq, "7", "3")));
        assert_eq!(book.get(qqq), pos("7", "3"));
        assert_eq!(book.quantities().get(&qqq), Some(&d("7")));
    }

    #[test]
    fn stored_positions_are_never_zero() {
        let spy = Symbol::new("SPY");
        let qqq = Symbol::new("QQQ");
        let iwm = Symbol::new("IWM");
        let mut book = PositionBook::new();

        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        fill(&mut book, qqq, OrderSide::Sell, "3", "20");
        fill(&mut book, spy, OrderSide::Sell, "10", "105");
        book.adopt(qqq, d("0"), None, ts());
        fill(&mut book, iwm, OrderSide::Buy, "4", "7");
        book.adopt(iwm, d("-4"), None, ts());
        fill(&mut book, iwm, OrderSide::Buy, "4", "6");
        book.adopt(spy, d("0"), None, ts());
        fill(&mut book, qqq, OrderSide::Buy, "2", "21");

        for (_, qty) in book.quantities() {
            assert!(!qty.is_zero());
        }
        assert_eq!(book.quantities().len(), 1);
        assert_eq!(book.get(spy), Position::default());
        assert_eq!(book.get(qqq), pos("2", "21"));
        assert_eq!(book.get(iwm), pos("0", "0"));
    }

    #[test]
    fn restore_then_get_returns_the_position() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        book.restore(spy, d("-4"), d("12.50"));
        assert_eq!(book.get(spy), pos("-4", "12.5"));
        assert_eq!(book.get(spy).avg_cost.scale(), 1);
    }

    #[test]
    fn restore_normalizes_the_average_cost() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        book.restore(spy, d("10"), d("105.00"));
        assert_eq!(book.get(spy).avg_cost, d("105"));
        assert_eq!(book.get(spy).avg_cost.scale(), 0);
    }

    #[test]
    fn restore_with_zero_removes_an_existing_position() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        fill(&mut book, spy, OrderSide::Buy, "10", "100");
        book.restore(spy, d("0"), d("100"));
        assert_eq!(book.get(spy), Position::default());
        assert!(book.quantities().is_empty());
        assert_eq!(book.iter().count(), 0);
    }

    #[test]
    fn restore_then_adopt_same_figures_is_none() {
        let spy = Symbol::new("SPY");
        let mut book = PositionBook::new();
        book.restore(spy, d("10"), d("100.0"));
        assert_eq!(book.adopt(spy, d("10"), Some(d("100")), ts()), None);
        assert_eq!(book.get(spy), pos("10", "100"));
    }

    #[test]
    fn restore_is_reflected_in_quantities() {
        let spy = Symbol::new("SPY");
        let qqq = Symbol::new("QQQ");
        let mut book = PositionBook::new();
        book.restore(qqq, d("-3"), d("20"));
        book.restore(spy, d("10"), d("100"));
        let quantities: Vec<(Symbol, Decimal)> = book.quantities().into_iter().collect();
        assert_eq!(quantities, vec![(qqq, d("-3")), (spy, d("10"))]);
    }

    #[test]
    fn iter_lists_positions_in_symbol_order() {
        let spy = Symbol::new("SPY");
        let qqq = Symbol::new("QQQ");
        let iwm = Symbol::new("IWM");
        let mut book = PositionBook::new();
        book.restore(spy, d("10"), d("100"));
        book.restore(iwm, d("2"), d("7"));
        book.restore(qqq, d("-4"), d("50"));
        let listed: Vec<(Symbol, Position)> = book.iter().collect();
        assert_eq!(
            listed,
            vec![
                (iwm, pos("2", "7")),
                (qqq, pos("-4", "50")),
                (spy, pos("10", "100")),
            ]
        );
    }
}
