//! Broker-agnostic order vocabulary shared by the engine, the store and the broker adapter.
//!
//! Prices and quantities are the Nautilus fixed-point types. Side, type and time in force are
//! the Nautilus enums; their `Display` strings (`BUY`, `LIMIT`, `DAY`, ...) are what the store
//! holds in `order_event.side`, `.type` and `.time_in_force`.

use std::future::Future;

use nautilus_core::UnixNanos;
use nautilus_model::{
    enums::{OrderSide, OrderType, PositionSide, TimeInForce},
    identifiers::{ClientOrderId, Symbol, TradeId, VenueOrderId},
    types::{Money, Price, Quantity},
};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::DecisionId;

/// Lifecycle state of an order (`order_event.state`). The snake_case strings are fixed by a
/// `CHECK` constraint in `migrations/`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum OrderState {
    /// Written to the intent log; nothing has been sent to the broker yet.
    Intent,
    /// Sent; the broker has not acknowledged it.
    Submitted,
    /// Live at the broker with nothing filled.
    Accepted,
    /// Live at the broker with part of the quantity filled.
    PartiallyFilled,
    /// A cancel or replace was requested and is not confirmed yet. Fills can still arrive.
    PendingCancel,
    Filled,
    Canceled,
    /// Superseded by a replacement order with a new client order id.
    Replaced,
    Rejected,
    Expired,
}

impl OrderState {
    /// No further broker updates are expected, except a late fill (which still counts).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Canceled | Self::Replaced | Self::Rejected | Self::Expired
        )
    }

    /// Position in the normal lifecycle, used to ignore updates that arrive out of order: an
    /// update with a lower rank than the current state is stale. All terminal states share
    /// the highest rank.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Intent => 0,
            Self::Submitted => 1,
            Self::Accepted => 2,
            Self::PartiallyFilled => 3,
            Self::PendingCancel => 4,
            Self::Filled | Self::Canceled | Self::Replaced | Self::Rejected | Self::Expired => 5,
        }
    }
}

/// What the engine intends to send. It is written to the intent log before submission, so a
/// crash between "decided" and "sent" can be resolved against the broker by `client_order_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderIntent {
    pub decision_id: DecisionId,
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    /// `Market` or `Limit`; adapters reject anything else.
    pub order_type: OrderType,
    pub qty: Quantity,
    /// Required for `Limit`, absent for `Market`.
    pub limit_px: Option<Price>,
    pub time_in_force: TimeInForce,
    /// When the engine formed the intent (from its injected clock).
    pub ts: UnixNanos,
}

/// The broker's view of one order, from a REST snapshot or a stream update. The broker is the
/// source of truth: `cum_filled_qty` is cumulative and is what fills are derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerOrder {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub qty: Quantity,
    pub limit_px: Option<Price>,
    pub time_in_force: TimeInForce,
    pub state: OrderState,
    /// Total filled so far across all fills of this order.
    pub cum_filled_qty: Quantity,
    /// Average price over `cum_filled_qty`; `None` while nothing is filled.
    pub avg_fill_px: Option<Price>,
    /// The broker's last-update time for the order.
    pub ts: UnixNanos,
}

/// The single execution a stream update reports, when it reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillDetail {
    /// The broker's execution id.
    pub trade_id: TradeId,
    pub px: Price,
    pub qty: Quantity,
}

/// One message from the broker's order stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderUpdate {
    pub order: BrokerOrder,
    pub fill: Option<FillDetail>,
    /// The broker's event time.
    pub ts: UnixNanos,
}

/// A fill as the engine records it (one `fill` row). Derived from the change in the broker's
/// cumulative filled quantity, so a replayed or duplicated broker message yields no second fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    pub client_order_id: ClientOrderId,
    /// The broker's execution id, or a deterministic synthetic id when the fill was inferred
    /// from a snapshot (`fill.broker_fill_id`, unique).
    pub trade_id: TradeId,
    pub px: Price,
    pub qty: Quantity,
    pub ts: UnixNanos,
}

/// A position as the broker reports it. `qty` is unsigned; `side` carries the direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerPosition {
    pub symbol: Symbol,
    pub side: PositionSide,
    pub qty: Quantity,
    pub avg_entry_px: Price,
}

/// The account figures the risk gate needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub equity: Money,
    pub cash: Money,
    pub buying_power: Money,
    /// The broker will not accept orders for this account.
    pub trading_blocked: bool,
}

/// A change to a live order. Brokers implement this as cancel-and-new, so the replacement gets
/// its own client order id (and its own intent-log row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceRequest {
    pub new_client_order_id: ClientOrderId,
    pub qty: Option<Quantity>,
    pub limit_px: Option<Price>,
}

/// Why a submit did not return an order.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError<E> {
    /// The broker definitely refused: no order exists.
    #[error("order rejected: {0}")]
    Rejected(#[source] E),
    /// Timeout, dropped connection or server error: the order may or may not exist. Resolve
    /// it by looking the client order id up at the broker; never resend blindly.
    #[error("order outcome unknown: {0}")]
    Unknown(#[source] E),
}

/// The trading side of a broker. Implemented by the Alpaca adapter and by test doubles.
pub trait Broker: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn submit(
        &self,
        intent: &OrderIntent,
    ) -> impl Future<Output = Result<BrokerOrder, SubmitError<Self::Error>>> + Send;

    fn cancel(
        &self,
        venue_order_id: VenueOrderId,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Cancels every open order (kill switch).
    fn cancel_all(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> impl Future<Output = Result<BrokerOrder, SubmitError<Self::Error>>> + Send;

    /// `Ok(None)` means the broker has never seen this client order id.
    fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> impl Future<Output = Result<Option<BrokerOrder>, Self::Error>> + Send;

    fn open_orders(&self) -> impl Future<Output = Result<Vec<BrokerOrder>, Self::Error>> + Send;

    fn positions(&self) -> impl Future<Output = Result<Vec<BrokerPosition>, Self::Error>> + Send;

    fn account(&self) -> impl Future<Output = Result<AccountSnapshot, Self::Error>> + Send;
}

/// The write-ahead intent log. `record_intent` must have returned `Ok` before the order is
/// sent to the broker; this is the one synchronous store write on the order path.
pub trait IntentLog: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn record_intent(
        &self,
        intent: &OrderIntent,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
