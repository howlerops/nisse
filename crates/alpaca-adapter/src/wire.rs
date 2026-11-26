//! Alpaca trading-API JSON shapes and their mapping to the shared order types. Used by both
//! the REST client and the order stream, which embeds the same order object.
//!
//! Field and status tables: `docs/phases/phase-1.md`, "Alpaca reference".

use nisse_core::{
    AccountSnapshot, BrokerOrder, BrokerPosition, OrderIntent, OrderState, ReplaceRequest,
};
use serde::{Deserialize, Serialize};

use crate::AdapterError;

/// Alpaca's order object (REST responses and the `order` field of stream messages). Unknown
/// fields are ignored. Decimals arrive as JSON strings.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireOrder {
    pub id: String,
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub time_in_force: String,
    /// `null` for notional (dollar-amount) orders, which this adapter never sends.
    pub qty: Option<String>,
    pub limit_price: Option<String>,
    pub filled_qty: String,
    pub filled_avg_price: Option<String>,
    pub status: String,
    pub updated_at: String,
}

/// Alpaca's position object.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WirePosition {
    pub symbol: String,
    /// `long` or `short`.
    pub side: String,
    /// Unsigned for longs; Alpaca writes a leading `-` for shorts, which is dropped here
    /// because `side` carries the direction.
    pub qty: String,
    pub avg_entry_price: String,
}

/// Alpaca's account object (the fields the risk gate needs).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireAccount {
    pub equity: String,
    pub cash: String,
    pub buying_power: String,
    pub trading_blocked: bool,
    #[serde(default)]
    pub account_blocked: bool,
    #[serde(default)]
    pub trade_suspended_by_user: bool,
    pub currency: Option<String>,
}

/// Body of `POST /v2/orders`. Decimals are sent as strings, exactly as the engine wrote them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OrderRequest {
    pub symbol: String,
    pub qty: String,
    pub side: &'static str,
    #[serde(rename = "type")]
    pub order_type: &'static str,
    pub time_in_force: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
    pub client_order_id: String,
}

/// Body of `PATCH /v2/orders/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReplaceBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
    pub client_order_id: String,
}

/// Maps an Alpaca order `status` to [`OrderState`] (table in the phase doc). An unknown
/// status is an [`AdapterError::Decode`], never a guess.
#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
pub(crate) fn order_state(status: &str) -> Result<OrderState, AdapterError> {
    todo!("Phase 1 task T0")
}

#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
impl WireOrder {
    /// Converts to the shared type. Errors: `Decode` for bad numbers, timestamps, side or
    /// status; `Unsupported` for an order type other than market or limit, a time in force
    /// other than day or gtc, or a notional order (`qty` is `null`).
    pub(crate) fn into_broker_order(self) -> Result<BrokerOrder, AdapterError> {
        todo!("Phase 1 task T0")
    }
}

#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
impl WirePosition {
    pub(crate) fn into_broker_position(self) -> Result<BrokerPosition, AdapterError> {
        todo!("Phase 1 task T0")
    }
}

#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
impl WireAccount {
    /// `trading_blocked` in the result is true when any of `trading_blocked`,
    /// `account_blocked` or `trade_suspended_by_user` is set. Money is in `currency`
    /// (default USD).
    pub(crate) fn into_account_snapshot(self) -> Result<AccountSnapshot, AdapterError> {
        todo!("Phase 1 task T0")
    }
}

#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
impl OrderRequest {
    /// Builds the request body. Errors with `Unsupported` unless the intent is a market order
    /// without a limit price or a limit order with one, and its time in force is day or gtc.
    pub(crate) fn from_intent(intent: &OrderIntent) -> Result<Self, AdapterError> {
        todo!("Phase 1 task T0")
    }
}

#[allow(unused_variables, reason = "stub: Phase 1 task T0")]
impl ReplaceBody {
    pub(crate) fn from_request(request: &ReplaceRequest) -> Self {
        todo!("Phase 1 task T0")
    }
}
