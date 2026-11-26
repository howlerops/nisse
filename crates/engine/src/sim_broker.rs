//! A simulated broker for backtests, behind the same [`Broker`] trait as the real one, so the
//! engine's order path is identical in backtest and live.

use std::collections::BTreeMap;
use std::sync::{MutexGuard, PoisonError};

use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, FillDetail, OpenOrders, OrderIntent,
    OrderState, OrderUpdate, ReplaceRequest, SubmitError,
    nautilus::{
        Bar, ClientOrderId, Money, OrderSide, OrderType, PositionSide, Price, Quantity, Symbol,
        TimeInForce, TradeId, UnixNanos, VenueOrderId,
    },
};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::{CostModel, EngineError, PositionBook};

/// The simulated account. Interior state sits behind a `Mutex` because [`Broker`] methods
/// take `&self`; the lock is never held across an `.await` (no method awaits anything).
#[derive(Debug)]
pub struct SimBroker {
    state: std::sync::Mutex<SimState>,
    costs: CostModel,
}

/// Everything the simulation tracks.
#[derive(Debug, Default)]
struct SimState {
    cash: Decimal,
    /// Positions with average cost, kept by the same rules as the engine's own ledger.
    positions: PositionBook,
    /// Last close seen per symbol, used to mark positions.
    marks: BTreeMap<Symbol, Decimal>,
    /// Orders accepted and not yet filled or cancelled, in acceptance order.
    open: Vec<BrokerOrder>,
    /// Every order ever accepted, by client order id.
    orders: BTreeMap<ClientOrderId, BrokerOrder>,
    /// Updates produced by fills and cancels, waiting for the engine to drain them.
    updates: Vec<OrderUpdate>,
    next_order: u64,
    next_fill: u64,
    fees_paid: Decimal,
}

const OVERFLOW: &str = "arithmetic overflow";

impl SimBroker {
    /// A new account holding `starting_cash` and nothing else.
    #[must_use]
    pub fn new(starting_cash: Decimal, costs: CostModel) -> Self {
        Self {
            state: std::sync::Mutex::new(SimState {
                cash: starting_cash,
                ..SimState::default()
            }),
            costs,
        }
    }

    /// Advances the simulation by one bar: fills every open order for the bar's symbol at the
    /// bar's **open** (through the cost model), then marks the symbol at the bar's close.
    /// Market orders always fill; a limit order fills only if the open is at or better than
    /// its limit, at the open. Fills queue `OrderUpdate`s for [`Self::drain_updates`].
    /// Rules are in `docs/phases/phase-3.md`, "Simulated broker rules".
    pub fn on_bar(&self, bar: &Bar) -> Result<(), EngineError> {
        let symbol = bar.bar_type.instrument_id().symbol;
        let open = bar.open.as_decimal();
        let close = bar.close.as_decimal();
        if open <= Decimal::ZERO || close <= Decimal::ZERO {
            return Err(EngineError::InvalidBar("bar prices must be positive"));
        }
        let mut guard = self.lock();
        let state = &mut *guard;
        let mut i = 0;
        while let Some(order) = state.open.get(i).copied() {
            if order.symbol != symbol {
                i += 1;
                continue;
            }
            let held = state.positions.get(symbol).qty;
            match settle(&order, open, state.cash, held, &self.costs)? {
                Settlement::Wait => i += 1,
                Settlement::Cancel => {
                    state.open.remove(i);
                    cancel_order(state, order, bar.ts_event);
                }
                Settlement::Fill { px, fee, notional } => {
                    // Everything that can fail is computed before anything is changed.
                    let cash = match order.side {
                        OrderSide::Buy => state.cash.checked_sub(
                            notional
                                .checked_add(fee)
                                .ok_or(EngineError::InvalidBar(OVERFLOW))?,
                        ),
                        OrderSide::Sell => state
                            .cash
                            .checked_add(notional)
                            .and_then(|c| c.checked_sub(fee)),
                    }
                    .ok_or(EngineError::InvalidBar(OVERFLOW))?;
                    let fees_paid = state
                        .fees_paid
                        .checked_add(fee)
                        .ok_or(EngineError::InvalidBar(OVERFLOW))?;
                    let px_price = Price::from_decimal(px)
                        .map_err(|_| EngineError::InvalidBar("fill price out of range"))?;
                    let trade_n =
                        next_id(&mut state.next_fill).ok_or(EngineError::InvalidBar(OVERFLOW))?;

                    state.open.remove(i);
                    state.cash = cash;
                    state.fees_paid = fees_paid;
                    state.positions.apply_fill(
                        order.symbol,
                        order.side,
                        order.qty.as_decimal(),
                        px,
                        bar.ts_event,
                    );

                    let mut filled = order;
                    filled.state = OrderState::Filled;
                    filled.cum_filled_qty = order.qty;
                    filled.avg_fill_px = Some(px_price);
                    filled.ts = bar.ts_event;
                    state.orders.insert(filled.client_order_id, filled);
                    state.updates.push(OrderUpdate {
                        order: filled,
                        fill: Some(FillDetail {
                            trade_id: TradeId::new(format!("simfill-{trade_n}")),
                            px: px_price,
                            qty: order.qty,
                        }),
                        ts: bar.ts_event,
                    });
                }
            }
        }
        state.marks.insert(symbol, close);
        Ok(())
    }

    /// Takes the updates produced since the last call, oldest first. The engine applies them
    /// exactly as it applies the live order stream.
    #[must_use]
    pub fn drain_updates(&self) -> Vec<OrderUpdate> {
        std::mem::take(&mut self.lock().updates)
    }

    /// Total fees charged so far.
    #[must_use]
    pub fn fees_paid(&self) -> Decimal {
        self.lock().fees_paid
    }

    /// Takes the state lock, ignoring poisoning: the simulation keeps answering after a panic
    /// elsewhere. The guard is dropped before any `.await`.
    fn lock(&self) -> MutexGuard<'_, SimState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Equity: cash plus each position at its last close (or average cost if never marked).
    fn equity(&self) -> Result<Decimal, SimError> {
        let state = self.lock();
        let mut equity = state.cash;
        for (symbol, qty) in state.positions.quantities() {
            let mark = state
                .marks
                .get(&symbol)
                .copied()
                .unwrap_or_else(|| state.positions.get(symbol).avg_cost);
            let value = qty.checked_mul(mark).ok_or_else(overflow)?;
            equity = equity.checked_add(value).ok_or_else(overflow)?;
        }
        Ok(equity)
    }
}

/// What a bar does to one open order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settlement {
    /// Stays open for a later bar.
    Wait,
    /// Cancelled without a fill.
    Cancel,
    /// Fills in full at `px`; `notional` is `qty * px`.
    Fill {
        px: Decimal,
        fee: Decimal,
        notional: Decimal,
    },
}

/// Decides what the bar's open does to `order`. `held` is the signed quantity held in its
/// symbol; `cash` is the cash available. Pure: nothing is changed.
fn settle(
    order: &BrokerOrder,
    open: Decimal,
    cash: Decimal,
    held: Decimal,
    costs: &CostModel,
) -> Result<Settlement, EngineError> {
    let limit = match order.order_type {
        OrderType::Market => None,
        OrderType::Limit => match order.limit_px {
            Some(px) => Some(px.as_decimal()),
            None => return Ok(Settlement::Cancel),
        },
        _ => return Ok(Settlement::Cancel),
    };
    if let Some(limit) = limit {
        let reached = match order.side {
            OrderSide::Buy => open <= limit,
            OrderSide::Sell => open >= limit,
        };
        if !reached {
            return Ok(if order.time_in_force == TimeInForce::Day {
                Settlement::Cancel
            } else {
                Settlement::Wait
            });
        }
    }
    let qty = order.qty.as_decimal();
    let priced = costs.price(order.side, open, qty);
    let notional = qty
        .checked_mul(priced.px)
        .ok_or(EngineError::InvalidBar(OVERFLOW))?;
    let affordable = match order.side {
        OrderSide::Buy => {
            let total = notional
                .checked_add(priced.fee)
                .ok_or(EngineError::InvalidBar(OVERFLOW))?;
            total <= cash
        }
        OrderSide::Sell => qty <= held,
    };
    Ok(if affordable {
        Settlement::Fill {
            px: priced.px,
            fee: priced.fee,
            notional,
        }
    } else {
        Settlement::Cancel
    })
}

/// Marks `order` cancelled at `ts`, stores it and queues its update.
fn cancel_order(state: &mut SimState, mut order: BrokerOrder, ts: UnixNanos) {
    order.state = OrderState::Canceled;
    order.ts = ts;
    state.orders.insert(order.client_order_id, order);
    state.updates.push(OrderUpdate {
        order,
        fill: None,
        ts,
    });
}

/// Advances a counter and returns the new value; `None` on overflow.
fn next_id(counter: &mut u64) -> Option<u64> {
    *counter = counter.checked_add(1)?;
    Some(*counter)
}

fn overflow() -> SimError {
    SimError(OVERFLOW.to_owned())
}

fn rejected(message: &str) -> SubmitError<SimError> {
    SubmitError::Rejected(SimError(message.to_owned()))
}

/// A USD amount rounded to the cent (half-even), as a `Money`.
fn usd(amount: Decimal) -> Result<Money, SimError> {
    let mut rounded = amount.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
    rounded.rescale(2);
    Money::from_str_exact(&format!("{rounded} USD")).map_err(SimError)
}

fn to_quantity(value: Decimal) -> Result<Quantity, SimError> {
    Quantity::from_decimal(value).map_err(|e| SimError(e.to_string()))
}

fn to_price(value: Decimal) -> Result<Price, SimError> {
    Price::from_decimal(value).map_err(|e| SimError(e.to_string()))
}

/// The simulated broker never fails; this type only satisfies the trait.
#[derive(Debug, thiserror::Error)]
#[error("simulated broker: {0}")]
pub struct SimError(pub String);

impl Broker for SimBroker {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        let mut state = self.lock();
        if !intent.qty.is_positive() {
            return Err(rejected("quantity must be positive"));
        }
        if !matches!(intent.order_type, OrderType::Market | OrderType::Limit) {
            return Err(rejected("only market and limit orders are simulated"));
        }
        if matches!(intent.order_type, OrderType::Limit) && intent.limit_px.is_none() {
            return Err(rejected("limit order has no limit price"));
        }
        if state.orders.contains_key(&intent.client_order_id) {
            return Err(rejected("client order id already used"));
        }
        let Some(venue_n) = next_id(&mut state.next_order) else {
            return Err(rejected("order counter overflow"));
        };
        let order = BrokerOrder {
            client_order_id: intent.client_order_id,
            venue_order_id: VenueOrderId::new(format!("sim-{venue_n}")),
            symbol: intent.symbol,
            side: intent.side,
            order_type: intent.order_type,
            qty: intent.qty,
            limit_px: intent.limit_px,
            time_in_force: intent.time_in_force,
            state: OrderState::Accepted,
            cum_filled_qty: Quantity::zero(intent.qty.precision),
            avg_fill_px: None,
            ts: intent.ts,
        };
        state.open.push(order);
        state.orders.insert(order.client_order_id, order);
        Ok(order)
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        let mut state = self.lock();
        if let Some(pos) = state
            .open
            .iter()
            .position(|o| o.venue_order_id == venue_order_id)
        {
            let order = state.open.remove(pos);
            let ts = order.ts;
            cancel_order(&mut state, order, ts);
        }
        Ok(())
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        let mut state = self.lock();
        let open = std::mem::take(&mut state.open);
        for order in open {
            let ts = order.ts;
            cancel_order(&mut state, order, ts);
        }
        Ok(())
    }

    async fn replace(
        &self,
        _venue_order_id: VenueOrderId,
        _request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        Err(rejected("replace is not simulated"))
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        Ok(self.lock().orders.get(&client_order_id).copied())
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        Ok(OpenOrders {
            orders: self.lock().open.clone(),
            unrepresentable: 0,
        })
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        let state = self.lock();
        let mut positions = Vec::new();
        for (symbol, qty) in state.positions.quantities() {
            if qty.is_zero() {
                continue;
            }
            let side = if qty.is_sign_negative() {
                PositionSide::Short
            } else {
                PositionSide::Long
            };
            positions.push(BrokerPosition {
                symbol,
                side,
                qty: to_quantity(qty.abs())?,
                avg_entry_px: to_price(state.positions.get(symbol).avg_cost)?,
            });
        }
        Ok(positions)
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        let equity = self.equity()?;
        let cash = self.lock().cash;
        Ok(AccountSnapshot {
            equity: usd(equity)?,
            cash: usd(cash)?,
            buying_power: usd(cash)?,
            trading_blocked: false,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests build values from literals; a malformed literal is a bug in the test"
)]
mod tests {
    use nisse_core::{
        DecisionId,
        nautilus::{
            AggregationSource, BarAggregation, BarSpecification, BarType, InstrumentId, PriceType,
            Venue,
        },
    };

    use super::*;

    const T1: u64 = 1_700_000_000_000_000_000;
    const DAY: u64 = 86_400_000_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn costs() -> CostModel {
        CostModel::new(d("5"), d("0.01")).unwrap()
    }

    fn free() -> CostModel {
        CostModel::new(d("0"), d("0")).unwrap()
    }

    /// A daily bar for `symbol`; all prices are written to two places.
    fn bar(symbol: &str, ts: u64, open: &str, high: &str, low: &str, close: &str) -> Bar {
        let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
        let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
        let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
        Bar::new(
            bar_type,
            Price::from(open),
            Price::from(high),
            Price::from(low),
            Price::from(close),
            Quantity::from("1000"),
            UnixNanos::from(ts),
            UnixNanos::from(ts),
        )
    }

    /// A bar that opens and closes at `px` (high and low bracket it).
    fn flat_bar(symbol: &str, ts: u64, px: &str) -> Bar {
        bar(symbol, ts, px, px, px, px)
    }

    fn intent(
        id: &str,
        symbol: &str,
        side: OrderSide,
        order_type: OrderType,
        qty: &str,
        limit: Option<&str>,
        time_in_force: TimeInForce,
    ) -> OrderIntent {
        OrderIntent {
            decision_id: DecisionId(1),
            client_order_id: ClientOrderId::new(id),
            symbol: Symbol::new(symbol),
            side,
            order_type,
            qty: Quantity::from(qty),
            limit_px: limit.map(Price::from),
            time_in_force,
            ts: UnixNanos::from(T1),
        }
    }

    fn market(id: &str, symbol: &str, side: OrderSide, qty: &str) -> OrderIntent {
        intent(
            id,
            symbol,
            side,
            OrderType::Market,
            qty,
            None,
            TimeInForce::Day,
        )
    }

    fn limit(
        id: &str,
        symbol: &str,
        side: OrderSide,
        qty: &str,
        px: &str,
        tif: TimeInForce,
    ) -> OrderIntent {
        intent(id, symbol, side, OrderType::Limit, qty, Some(px), tif)
    }

    async fn cash(broker: &SimBroker) -> Decimal {
        broker.account().await.unwrap().cash.as_decimal()
    }

    async fn equity(broker: &SimBroker) -> Decimal {
        broker.account().await.unwrap().equity.as_decimal()
    }

    /// `(qty, avg_entry_px)` of a long or short symbol, `None` when flat.
    async fn held(broker: &SimBroker, symbol: &str) -> Option<(Decimal, Decimal)> {
        broker
            .positions()
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.symbol == Symbol::new(symbol))
            .map(|p| (p.qty.as_decimal(), p.avg_entry_px.as_decimal()))
    }

    fn trade_id(update: &OrderUpdate) -> String {
        update.fill.unwrap().trade_id.to_string()
    }

    #[tokio::test]
    async fn market_buy_fills_at_next_open_with_slippage_and_fee() {
        let broker = SimBroker::new(d("10000"), costs());
        let accepted = broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        assert_eq!(accepted.state, OrderState::Accepted);
        assert_eq!(accepted.venue_order_id, VenueOrderId::new("sim-1"));
        assert_eq!(accepted.ts, UnixNanos::from(T1));

        // Nothing happens until a bar for the symbol arrives.
        assert!(broker.drain_updates().is_empty());
        assert_eq!(cash(&broker).await, d("10000"));

        let b = bar("SPY", T1 + DAY, "100.00", "101.00", "99.00", "100.50");
        broker.on_bar(&b).unwrap();

        // 100 * 1.0005 = 100.05; fee 10 * 0.01 = 0.10; cash 10000 - 1000.5 - 0.10.
        assert_eq!(cash(&broker).await, d("8999.40"));
        assert_eq!(broker.fees_paid(), d("0.10"));
        assert_eq!(held(&broker, "SPY").await, Some((d("10"), d("100.05"))));

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        let update = updates[0];
        assert_eq!(update.order.state, OrderState::Filled);
        assert_eq!(update.order.cum_filled_qty.as_decimal(), d("10"));
        assert_eq!(update.order.avg_fill_px.unwrap().as_decimal(), d("100.05"));
        assert_eq!(update.order.ts, b.ts_event);
        let fill = update.fill.unwrap();
        assert_eq!(fill.trade_id, TradeId::new("simfill-1"));
        assert_eq!(fill.px.as_decimal(), d("100.05"));
        assert_eq!(fill.qty.as_decimal(), d("10"));
        assert_eq!(update.ts, b.ts_event);
        assert!(broker.drain_updates().is_empty());
    }

    #[tokio::test]
    async fn bar_for_other_symbol_fills_nothing_and_drains_nothing() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("QQQ", T1, "50.00")).unwrap();

        assert!(broker.drain_updates().is_empty());
        assert_eq!(broker.open_orders().await.unwrap().orders.len(), 1);
        assert_eq!(cash(&broker).await, d("10000"));
        assert!(broker.positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn market_sell_fills_at_next_open_minus_slippage() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("buy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        let _ = broker.drain_updates();
        assert_eq!(cash(&broker).await, d("8999.40"));

        broker
            .submit(&market("sell", "SPY", OrderSide::Sell, "4"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1 + DAY, "110.00")).unwrap();

        // 110 * 0.9995 = 109.945; fee 4 * 0.01 = 0.04; cash + 4 * 109.945 - 0.04.
        assert_eq!(cash(&broker).await, d("9439.14"));
        assert_eq!(broker.fees_paid(), d("0.14"));
        assert_eq!(held(&broker, "SPY").await, Some((d("6"), d("100.05"))));

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        let fill = updates[0].fill.unwrap();
        assert_eq!(fill.trade_id, TradeId::new("simfill-2"));
        assert_eq!(fill.px.as_decimal(), d("109.945"));
        assert_eq!(fill.qty.as_decimal(), d("4"));
    }

    #[tokio::test]
    async fn buy_that_cash_cannot_cover_is_cancelled() {
        let broker = SimBroker::new(d("500"), costs());
        let order = market("c1", "SPY", OrderSide::Buy, "10");
        broker.submit(&order).await.unwrap();
        let b = flat_bar("SPY", T1, "100.00");
        broker.on_bar(&b).unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert!(updates[0].fill.is_none());
        assert_eq!(updates[0].ts, b.ts_event);
        assert_eq!(cash(&broker).await, d("500"));
        assert!(broker.positions().await.unwrap().is_empty());
        assert!(broker.open_orders().await.unwrap().orders.is_empty());
        assert_eq!(
            broker
                .order_by_client_id(order.client_order_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            OrderState::Canceled
        );
    }

    #[tokio::test]
    async fn buy_that_cash_exactly_covers_fills_and_leaves_zero() {
        // 10 * 100.05 + 0.10 = 1000.60 exactly.
        let broker = SimBroker::new(d("1000.60"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        assert_eq!(broker.drain_updates()[0].order.state, OrderState::Filled);
        assert_eq!(cash(&broker).await, d("0"));
    }

    #[tokio::test]
    async fn oversell_and_sell_without_position_are_cancelled() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("buy", "SPY", OrderSide::Buy, "5"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        let _ = broker.drain_updates();
        let cash_before = cash(&broker).await;

        broker
            .submit(&market("oversell", "SPY", OrderSide::Sell, "6"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1 + DAY, "100.00")).unwrap();
        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert_eq!(cash(&broker).await, cash_before);
        assert_eq!(held(&broker, "SPY").await, Some((d("5"), d("100.05"))));

        // A sell with no position at all.
        let empty = SimBroker::new(d("10000"), costs());
        empty
            .submit(&market("naked", "QQQ", OrderSide::Sell, "1"))
            .await
            .unwrap();
        empty.on_bar(&flat_bar("QQQ", T1, "50.00")).unwrap();
        let updates = empty.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert_eq!(cash(&empty).await, d("10000"));
        assert!(empty.positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn limit_buy_day_cancels_when_open_is_above_limit() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&limit(
                "c1",
                "SPY",
                OrderSide::Buy,
                "10",
                "99.00",
                TimeInForce::Day,
            ))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert!(updates[0].fill.is_none());
        assert!(broker.open_orders().await.unwrap().orders.is_empty());
        assert_eq!(cash(&broker).await, d("10000"));
    }

    #[tokio::test]
    async fn limit_buy_day_fills_at_open_plus_slippage_when_open_is_at_or_below_limit() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&limit(
                "c1",
                "SPY",
                OrderSide::Buy,
                "10",
                "99.00",
                TimeInForce::Day,
            ))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "98.50")).unwrap();

        // 98.5 * 1.0005 = 98.54925; notional 985.4925; fee 0.10.
        // The internal cash is exact; `account()` rounds it to the cent for `Money`.
        assert_eq!(broker.lock().cash, d("9014.4075"));
        assert_eq!(cash(&broker).await, d("9014.41"));
        assert_eq!(broker.fees_paid(), d("0.10"));
        let updates = broker.drain_updates();
        assert_eq!(updates[0].order.state, OrderState::Filled);
        assert_eq!(updates[0].fill.unwrap().px.as_decimal(), d("98.54925"));
    }

    #[tokio::test]
    async fn limit_buy_at_exactly_the_limit_fills() {
        let broker = SimBroker::new(d("10000"), free());
        broker
            .submit(&limit(
                "c1",
                "SPY",
                OrderSide::Buy,
                "10",
                "99.00",
                TimeInForce::Day,
            ))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "99.00")).unwrap();
        assert_eq!(broker.drain_updates()[0].order.state, OrderState::Filled);
        assert_eq!(cash(&broker).await, d("9010"));
    }

    #[tokio::test]
    async fn gtc_limit_buy_waits_then_fills_on_a_later_bar() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&limit(
                "c1",
                "SPY",
                OrderSide::Buy,
                "10",
                "99.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        assert!(broker.drain_updates().is_empty());
        assert_eq!(broker.open_orders().await.unwrap().orders.len(), 1);

        broker.on_bar(&flat_bar("SPY", T1 + DAY, "98.00")).unwrap();
        // 98 * 1.0005 = 98.049; fee 0.10.
        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].fill.unwrap().px.as_decimal(), d("98.049"));
        assert_eq!(cash(&broker).await, d("10000") - d("980.49") - d("0.10"));
        assert!(broker.open_orders().await.unwrap().orders.is_empty());
    }

    #[tokio::test]
    async fn limit_sell_is_the_mirror_image() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("buy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        let _ = broker.drain_updates();

        broker
            .submit(&limit(
                "day",
                "SPY",
                OrderSide::Sell,
                "10",
                "105.00",
                TimeInForce::Day,
            ))
            .await
            .unwrap();
        broker
            .submit(&limit(
                "gtc",
                "SPY",
                OrderSide::Sell,
                "10",
                "105.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();

        // Open 104 is below the 105 limit: the Day order is cancelled, the Gtc order waits.
        broker.on_bar(&flat_bar("SPY", T1 + DAY, "104.00")).unwrap();
        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.client_order_id, ClientOrderId::new("day"));
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert_eq!(held(&broker, "SPY").await, Some((d("10"), d("100.05"))));

        // Open 106 is at or above the limit: the Gtc order fills at 106 less slippage.
        broker
            .on_bar(&flat_bar("SPY", T1 + 2 * DAY, "106.00"))
            .unwrap();
        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.client_order_id, ClientOrderId::new("gtc"));
        // 106 * 0.9995 = 105.947; fee 0.10. Cash 8999.40 + 1059.47 - 0.10.
        assert_eq!(updates[0].fill.unwrap().px.as_decimal(), d("105.947"));
        assert_eq!(cash(&broker).await, d("10058.77"));
        assert!(broker.positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn equity_is_cash_plus_quantity_times_latest_close() {
        let broker = SimBroker::new(d("10000"), costs());
        assert_eq!(equity(&broker).await, d("10000"));
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .on_bar(&bar("SPY", T1, "100.00", "102.00", "99.00", "101.00"))
            .unwrap();
        // Cash 8999.40 + 10 * 101.00.
        assert_eq!(equity(&broker).await, d("9999.40") + d("10"));
        assert_eq!(equity(&broker).await, d("10009.40"));

        broker
            .on_bar(&bar("SPY", T1 + DAY, "101.00", "101.00", "90.00", "95.00"))
            .unwrap();
        assert_eq!(equity(&broker).await, d("9949.40"));
    }

    #[tokio::test]
    async fn drain_returns_each_update_once_in_order() {
        let broker = SimBroker::new(d("10000"), free());
        broker
            .submit(&market("spy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .submit(&market("qqq", "QQQ", OrderSide::Buy, "5"))
            .await
            .unwrap();

        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        broker.on_bar(&flat_bar("QQQ", T1, "50.00")).unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].order.client_order_id, ClientOrderId::new("spy"));
        assert_eq!(trade_id(&updates[0]), "simfill-1");
        assert_eq!(updates[1].order.client_order_id, ClientOrderId::new("qqq"));
        assert_eq!(trade_id(&updates[1]), "simfill-2");
        assert!(broker.drain_updates().is_empty());

        broker.on_bar(&flat_bar("SPY", T1 + DAY, "100.00")).unwrap();
        assert!(broker.drain_updates().is_empty());
    }

    #[tokio::test]
    async fn orders_on_one_symbol_fill_in_acceptance_order_seeing_each_others_cash() {
        // First buy costs 1000.60, leaving 500.40; the second costs 500.30 and fits.
        let broker = SimBroker::new(d("1501"), costs());
        broker
            .submit(&market("a", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .submit(&market("b", "SPY", OrderSide::Buy, "5"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].order.client_order_id, ClientOrderId::new("a"));
        assert_eq!(trade_id(&updates[0]), "simfill-1");
        assert_eq!(updates[1].order.client_order_id, ClientOrderId::new("b"));
        assert_eq!(trade_id(&updates[1]), "simfill-2");
        assert_eq!(updates[1].order.state, OrderState::Filled);
        assert_eq!(cash(&broker).await, d("0.10"));
        assert_eq!(held(&broker, "SPY").await, Some((d("15"), d("100.05"))));

        // With 1500 the second order no longer fits and is cancelled.
        let short = SimBroker::new(d("1500"), costs());
        short
            .submit(&market("a", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        short
            .submit(&market("b", "SPY", OrderSide::Buy, "5"))
            .await
            .unwrap();
        short.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        let updates = short.drain_updates();
        assert_eq!(updates[0].order.state, OrderState::Filled);
        assert_eq!(updates[1].order.state, OrderState::Canceled);
    }

    #[tokio::test]
    async fn rejects_duplicate_zero_quantity_and_priceless_limit() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "1"))
            .await
            .unwrap();

        let duplicate = broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "1"))
            .await;
        assert!(matches!(duplicate, Err(SubmitError::Rejected(_))));

        let zero = broker
            .submit(&market("c2", "SPY", OrderSide::Buy, "0"))
            .await;
        assert!(matches!(zero, Err(SubmitError::Rejected(_))));

        let priceless = broker
            .submit(&intent(
                "c3",
                "SPY",
                OrderSide::Buy,
                OrderType::Limit,
                "1",
                None,
                TimeInForce::Day,
            ))
            .await;
        assert!(matches!(priceless, Err(SubmitError::Rejected(_))));

        let stop = broker
            .submit(&intent(
                "c4",
                "SPY",
                OrderSide::Buy,
                OrderType::StopMarket,
                "1",
                None,
                TimeInForce::Day,
            ))
            .await;
        assert!(matches!(stop, Err(SubmitError::Rejected(_))));

        // Rejected submissions consume no venue id and no client id.
        assert_eq!(broker.open_orders().await.unwrap().orders.len(), 1);
        assert!(
            broker
                .order_by_client_id(ClientOrderId::new("c2"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn fees_paid_is_the_sum_of_all_fill_fees() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("buy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .submit(&market("cancelled", "SPY", OrderSide::Buy, "100000"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        assert_eq!(broker.fees_paid(), d("0.10"));

        broker
            .submit(&market("sell", "SPY", OrderSide::Sell, "4"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1 + DAY, "110.00")).unwrap();
        assert_eq!(broker.fees_paid(), d("0.14"));
    }

    #[tokio::test]
    async fn order_by_client_id_finds_filled_orders_and_misses_unknown_ids() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();

        let found = broker
            .order_by_client_id(ClientOrderId::new("c1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.state, OrderState::Filled);
        assert_eq!(found.cum_filled_qty.as_decimal(), d("10"));
        assert_eq!(found.avg_fill_px.unwrap().as_decimal(), d("100.05"));

        assert!(
            broker
                .order_by_client_id(ClientOrderId::new("missing"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn open_orders_are_listed_in_acceptance_order() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&limit(
                "z",
                "SPY",
                OrderSide::Buy,
                "1",
                "1.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        broker
            .submit(&limit(
                "a",
                "QQQ",
                OrderSide::Buy,
                "1",
                "1.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        let open = broker.open_orders().await.unwrap();
        assert_eq!(open.unrepresentable, 0);
        let ids: Vec<ClientOrderId> = open.orders.iter().map(|o| o.client_order_id).collect();
        assert_eq!(ids, vec![ClientOrderId::new("z"), ClientOrderId::new("a")]);
    }

    #[tokio::test]
    async fn positions_list_only_non_flat_symbols_in_symbol_order() {
        let broker = SimBroker::new(d("10000"), free());
        broker
            .submit(&market("spy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .submit(&market("qqq", "QQQ", OrderSide::Buy, "4"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        broker.on_bar(&flat_bar("QQQ", T1, "50.00")).unwrap();

        let positions = broker.positions().await.unwrap();
        let symbols: Vec<Symbol> = positions.iter().map(|p| p.symbol).collect();
        assert_eq!(symbols, vec![Symbol::new("QQQ"), Symbol::new("SPY")]);
        assert_eq!(positions[0].side, PositionSide::Long);
        assert_eq!(positions[0].qty.as_decimal(), d("4"));
        assert_eq!(positions[0].avg_entry_px.as_decimal(), d("50"));
        assert_eq!(positions[1].qty.as_decimal(), d("10"));

        broker
            .submit(&market("sell", "SPY", OrderSide::Sell, "10"))
            .await
            .unwrap();
        broker.on_bar(&flat_bar("SPY", T1 + DAY, "100.00")).unwrap();
        let positions = broker.positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, Symbol::new("QQQ"));
    }

    #[tokio::test]
    async fn cancel_queues_an_update_and_the_order_never_fills() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker.cancel(VenueOrderId::new("sim-1")).await.unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].order.state, OrderState::Canceled);
        assert!(updates[0].fill.is_none());

        broker.on_bar(&flat_bar("SPY", T1, "100.00")).unwrap();
        assert!(broker.drain_updates().is_empty());
        assert!(broker.positions().await.unwrap().is_empty());
        assert_eq!(cash(&broker).await, d("10000"));

        // Cancelling an unknown or already-finished order is a no-op.
        broker.cancel(VenueOrderId::new("sim-999")).await.unwrap();
        broker.cancel(VenueOrderId::new("sim-1")).await.unwrap();
        assert!(broker.drain_updates().is_empty());
    }

    #[tokio::test]
    async fn cancel_all_cancels_every_open_order_in_acceptance_order() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&limit(
                "a",
                "SPY",
                OrderSide::Buy,
                "1",
                "1.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        broker
            .submit(&limit(
                "b",
                "QQQ",
                OrderSide::Buy,
                "1",
                "1.00",
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        broker.cancel_all().await.unwrap();

        let updates = broker.drain_updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].order.client_order_id, ClientOrderId::new("a"));
        assert_eq!(updates[1].order.client_order_id, ClientOrderId::new("b"));
        assert!(
            updates
                .iter()
                .all(|u| u.order.state == OrderState::Canceled)
        );
        assert!(broker.open_orders().await.unwrap().orders.is_empty());
    }

    #[tokio::test]
    async fn replace_is_rejected() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "1"))
            .await
            .unwrap();
        let request = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("c2"),
            qty: None,
            limit_px: None,
        };
        let result = broker.replace(VenueOrderId::new("sim-1"), &request).await;
        assert!(matches!(
            result,
            Err(SubmitError::Rejected(SimError(ref message)))
                if message == "replace is not simulated"
        ));
    }

    #[tokio::test]
    async fn zero_costs_round_trip_marks_and_cash_exactly() {
        let broker = SimBroker::new(d("10000"), free());
        broker
            .submit(&market("buy", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        broker
            .on_bar(&bar("SPY", T1, "100.00", "111.00", "99.00", "110.00"))
            .unwrap();
        assert_eq!(equity(&broker).await, d("10100"));

        broker
            .submit(&market("sell", "SPY", OrderSide::Sell, "10"))
            .await
            .unwrap();
        broker
            .on_bar(&bar(
                "SPY",
                T1 + DAY,
                "120.00",
                "121.00",
                "119.00",
                "120.00",
            ))
            .unwrap();
        assert_eq!(cash(&broker).await, d("10200"));
        assert_eq!(equity(&broker).await, d("10200"));
        assert_eq!(broker.fees_paid(), d("0"));
    }

    #[tokio::test]
    async fn non_positive_bar_price_is_an_error_and_changes_nothing() {
        let broker = SimBroker::new(d("10000"), costs());
        broker
            .submit(&market("c1", "SPY", OrderSide::Buy, "10"))
            .await
            .unwrap();
        let result = broker.on_bar(&flat_bar("SPY", T1, "0.00"));
        assert!(matches!(result, Err(EngineError::InvalidBar(_))));
        assert_eq!(broker.open_orders().await.unwrap().orders.len(), 1);
        assert!(broker.drain_updates().is_empty());
    }

    #[tokio::test]
    async fn account_reports_usd_and_is_never_blocked() {
        let broker = SimBroker::new(d("1234.5"), costs());
        let account = broker.account().await.unwrap();
        assert_eq!(account.cash.as_decimal(), d("1234.50"));
        assert_eq!(account.buying_power.as_decimal(), d("1234.50"));
        assert_eq!(account.equity.as_decimal(), d("1234.50"));
        assert!(!account.trading_blocked);
        assert_eq!(account.cash.currency.code.as_str(), "USD");
    }
}
