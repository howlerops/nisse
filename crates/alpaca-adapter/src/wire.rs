//! Alpaca trading-API JSON shapes and their mapping to the shared order types. Used by both
//! the REST client and the order stream, which embeds the same order object.
//!
//! Field and status tables: `docs/phases/phase-1.md`, "Alpaca reference".

use nisse_core::nautilus::{
    ClientOrderId, Money, OrderSide, OrderType, PositionSide, Symbol, TimeInForce, VenueOrderId,
};
use nisse_core::{
    AccountSnapshot, BrokerOrder, BrokerPosition, OrderIntent, OrderState, ReplaceRequest,
};
use serde::{Deserialize, Serialize};

use crate::AdapterError;
use crate::parse::{parse_price, parse_qty, parse_ts};

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
    /// Required, like the other two flags: a missing flag must fail the read, not read as
    /// "not blocked".
    pub account_blocked: bool,
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
pub(crate) fn order_state(status: &str) -> Result<OrderState, AdapterError> {
    Ok(match status {
        "pending_new" | "accepted" | "accepted_for_bidding" | "pending_review" | "held" => {
            OrderState::Submitted
        }
        "new" | "done_for_day" | "stopped" | "suspended" | "calculated" => OrderState::Accepted,
        "partially_filled" => OrderState::PartiallyFilled,
        "pending_cancel" | "pending_replace" => OrderState::PendingCancel,
        "filled" => OrderState::Filled,
        "canceled" => OrderState::Canceled,
        "replaced" => OrderState::Replaced,
        "rejected" => OrderState::Rejected,
        "expired" => OrderState::Expired,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown order status: {other}"
            )));
        }
    })
}

/// Builds a Nautilus identifier with its checked constructor. The unchecked ones panic on
/// empty, all-whitespace or non-ASCII text, and these strings come from the broker.
fn checked_id<T, E: std::fmt::Display>(
    built: Result<T, E>,
    field: &str,
) -> Result<T, AdapterError> {
    built.map_err(|e| AdapterError::Decode(format!("bad {field}: {e}")))
}

fn parse_side(text: &str) -> Result<OrderSide, AdapterError> {
    match text {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        other => Err(AdapterError::Decode(format!("unknown side: {other}"))),
    }
}

/// Parses a decimal amount in the given currency through Nautilus' exact parser (no rounding
/// to the currency's precision and no `f64`).
fn parse_money(amount: &str, currency: &str) -> Result<Money, AdapterError> {
    Money::from_str_exact(&format!("{amount} {currency}"))
        .map_err(|e| AdapterError::Decode(format!("bad amount {amount} {currency}: {e}")))
}

impl WireOrder {
    /// Converts to the shared type. Errors: `Decode` for bad numbers, timestamps, side or
    /// status; `Unsupported` for an order type other than market or limit, a time in force
    /// other than day or gtc, or a notional order (`qty` is `null`).
    pub(crate) fn into_broker_order(self) -> Result<BrokerOrder, AdapterError> {
        let qty = self
            .qty
            .as_deref()
            .ok_or(AdapterError::Unsupported("notional order without qty"))?;
        let order_type = match self.order_type.as_str() {
            "market" => OrderType::Market,
            "limit" => OrderType::Limit,
            _ => {
                return Err(AdapterError::Unsupported(
                    "order type other than market or limit",
                ));
            }
        };
        let time_in_force = match self.time_in_force.as_str() {
            "day" => TimeInForce::Day,
            "gtc" => TimeInForce::Gtc,
            _ => {
                return Err(AdapterError::Unsupported(
                    "time in force other than day or gtc",
                ));
            }
        };
        // The engine only writes plain ASCII client order ids, so one that Nautilus cannot hold
        // belongs to an order placed by someone else: unsupported, not a decode failure.
        let client_order_id = ClientOrderId::new_checked(&self.client_order_id).map_err(|_| {
            AdapterError::Unsupported("client order id the engine could not have written")
        })?;
        Ok(BrokerOrder {
            client_order_id,
            venue_order_id: checked_id(VenueOrderId::new_checked(&self.id), "order id")?,
            symbol: checked_id(Symbol::new_checked(&self.symbol), "symbol")?,
            side: parse_side(&self.side)?,
            order_type,
            qty: parse_qty(qty)?,
            limit_px: self.limit_price.as_deref().map(parse_price).transpose()?,
            time_in_force,
            state: order_state(&self.status)?,
            cum_filled_qty: parse_qty(&self.filled_qty)?,
            avg_fill_px: self
                .filled_avg_price
                .as_deref()
                .map(parse_price)
                .transpose()?,
            ts: parse_ts(&self.updated_at)?,
        })
    }
}

impl WirePosition {
    pub(crate) fn into_broker_position(self) -> Result<BrokerPosition, AdapterError> {
        let symbol = checked_id(Symbol::new_checked(&self.symbol), "symbol")?;
        let side = match self.side.as_str() {
            "long" => PositionSide::Long,
            "short" => PositionSide::Short,
            other => {
                return Err(AdapterError::Decode(format!(
                    "unknown position side: {other}"
                )));
            }
        };
        let unsigned = self.qty.strip_prefix('-').unwrap_or(self.qty.as_str());
        Ok(BrokerPosition {
            symbol,
            side,
            qty: parse_qty(unsigned)?,
            avg_entry_px: parse_price(&self.avg_entry_price)?,
        })
    }
}

impl WireAccount {
    /// `trading_blocked` in the result is true when any of `trading_blocked`,
    /// `account_blocked` or `trade_suspended_by_user` is set. Money is in `currency`
    /// (default USD).
    pub(crate) fn into_account_snapshot(self) -> Result<AccountSnapshot, AdapterError> {
        let currency = self.currency.as_deref().unwrap_or("USD");
        Ok(AccountSnapshot {
            equity: parse_money(&self.equity, currency)?,
            cash: parse_money(&self.cash, currency)?,
            buying_power: parse_money(&self.buying_power, currency)?,
            trading_blocked: self.trading_blocked
                || self.account_blocked
                || self.trade_suspended_by_user,
        })
    }
}

impl OrderRequest {
    /// Builds the request body. Errors with `Unsupported` unless the intent is a market order
    /// without a limit price or a limit order with one, and its time in force is day or gtc.
    pub(crate) fn from_intent(intent: &OrderIntent) -> Result<Self, AdapterError> {
        let time_in_force = match intent.time_in_force {
            TimeInForce::Day => "day",
            TimeInForce::Gtc => "gtc",
            _ => {
                return Err(AdapterError::Unsupported(
                    "time in force other than day or gtc",
                ));
            }
        };
        let (order_type, limit_price) = match (intent.order_type, intent.limit_px) {
            (OrderType::Market, None) => ("market", None),
            (OrderType::Limit, Some(px)) => ("limit", Some(px.to_string())),
            (OrderType::Market, Some(_)) => {
                return Err(AdapterError::Unsupported("market order with a limit price"));
            }
            (OrderType::Limit, None) => {
                return Err(AdapterError::Unsupported(
                    "limit order without a limit price",
                ));
            }
            _ => {
                return Err(AdapterError::Unsupported(
                    "order type other than market or limit",
                ));
            }
        };
        let side = match intent.side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        };
        Ok(Self {
            symbol: intent.symbol.to_string(),
            qty: intent.qty.to_string(),
            side,
            order_type,
            time_in_force,
            limit_price,
            client_order_id: intent.client_order_id.to_string(),
        })
    }
}

impl ReplaceBody {
    pub(crate) fn from_request(request: &ReplaceRequest) -> Self {
        Self {
            qty: request.qty.map(|qty| qty.to_string()),
            limit_price: request.limit_px.map(|px| px.to_string()),
            client_order_id: request.new_client_order_id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use nisse_core::DecisionId;
    use nisse_core::nautilus::{Money, UnixNanos};
    use serde_json::{Value, json};

    use super::*;
    use crate::parse::{parse_price, parse_qty};

    /// The order object from the "Alpaca reference" section of the phase doc.
    const ORDER_JSON: &str = r#"{"asset_class": "us_equity", "asset_id": "b0b6dd9d-8b9b-48a9-ba46-b9d54906e415", "canceled_at": null, "client_order_id": "5680c4bc-9ac1-4a12-a44c-df427ba53032", "created_at": "2023-12-12T22:31:24.668464435Z", "expired_at": null, "extended_hours": false, "failed_at": null, "filled_at": null, "filled_avg_price": null, "filled_qty": "0", "hwm": null, "id": "7b08df51-c1ac-453c-99f9-323a5f075f0d", "legs": null, "limit_price": "150", "notional": null, "order_class": "", "order_type": "limit", "qty": "2", "replaced_at": null, "replaced_by": null, "replaces": null, "side": "buy", "source": null, "status": "accepted", "stop_price": null, "submitted_at": "2023-12-12T22:31:24.577215743Z", "subtag": null, "symbol": "AAPL", "time_in_force": "gtc", "trail_percent": null, "trail_price": null, "type": "limit", "updated_at": "2023-12-12T22:31:24.668464435Z"}"#;

    fn order_value() -> Value {
        serde_json::from_str(ORDER_JSON).unwrap()
    }

    fn order_from(value: Value) -> Result<BrokerOrder, AdapterError> {
        let wire: WireOrder = serde_json::from_value(value).unwrap();
        wire.into_broker_order()
    }

    fn intent(
        order_type: OrderType,
        limit_px: Option<&str>,
        time_in_force: TimeInForce,
    ) -> OrderIntent {
        OrderIntent {
            decision_id: DecisionId(1),
            client_order_id: ClientOrderId::new("nisse-1-0"),
            symbol: Symbol::new("SPY"),
            side: OrderSide::Buy,
            order_type,
            qty: parse_qty("1.5").unwrap(),
            limit_px: limit_px.map(|text| parse_price(text).unwrap()),
            time_in_force,
            ts: UnixNanos::from(1_u64),
        }
    }

    #[test]
    fn phase_doc_order_json_maps_every_field() {
        let wire: WireOrder = serde_json::from_str(ORDER_JSON).unwrap();
        let order = wire.into_broker_order().unwrap();
        assert_eq!(
            order.client_order_id.to_string(),
            "5680c4bc-9ac1-4a12-a44c-df427ba53032"
        );
        assert_eq!(
            order.venue_order_id.to_string(),
            "7b08df51-c1ac-453c-99f9-323a5f075f0d"
        );
        assert_eq!(order.symbol.to_string(), "AAPL");
        assert_eq!(order.side, OrderSide::Buy);
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.qty.to_string(), "2");
        assert_eq!(order.limit_px.unwrap().to_string(), "150");
        assert_eq!(order.time_in_force, TimeInForce::Gtc);
        assert_eq!(order.state, OrderState::Submitted);
        assert_eq!(order.cum_filled_qty.to_string(), "0");
        assert_eq!(order.avg_fill_px, None);
        assert_eq!(order.ts.as_u64(), 1_702_420_284_668_464_435);
    }

    #[test]
    fn filled_market_order_keeps_cum_and_avg_as_strings() {
        let mut value = order_value();
        value["type"] = json!("market");
        value["order_type"] = json!("market");
        value["limit_price"] = Value::Null;
        value["status"] = json!("filled");
        value["filled_qty"] = json!("1790.86");
        value["filled_avg_price"] = json!("105.8988475");
        value["qty"] = json!("1790.86");
        let order = order_from(value).unwrap();
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(order.order_type, OrderType::Market);
        assert_eq!(order.limit_px, None);
        assert_eq!(order.cum_filled_qty.to_string(), "1790.86");
        assert_eq!(order.avg_fill_px.unwrap().to_string(), "105.8988475");
    }

    #[test]
    fn every_status_row_maps_to_its_state() {
        let rows = [
            ("pending_new", OrderState::Submitted),
            ("accepted", OrderState::Submitted),
            ("accepted_for_bidding", OrderState::Submitted),
            ("pending_review", OrderState::Submitted),
            ("held", OrderState::Submitted),
            ("new", OrderState::Accepted),
            ("done_for_day", OrderState::Accepted),
            ("stopped", OrderState::Accepted),
            ("suspended", OrderState::Accepted),
            ("calculated", OrderState::Accepted),
            ("partially_filled", OrderState::PartiallyFilled),
            ("pending_cancel", OrderState::PendingCancel),
            ("pending_replace", OrderState::PendingCancel),
            ("filled", OrderState::Filled),
            ("canceled", OrderState::Canceled),
            ("replaced", OrderState::Replaced),
            ("rejected", OrderState::Rejected),
            ("expired", OrderState::Expired),
        ];
        for (status, expected) in rows {
            assert_eq!(order_state(status).unwrap(), expected, "status {status}");
        }
        assert!(matches!(order_state("bogus"), Err(AdapterError::Decode(_))));
        assert!(matches!(order_state(""), Err(AdapterError::Decode(_))));
        assert!(matches!(
            order_state("Filled"),
            Err(AdapterError::Decode(_))
        ));
    }

    #[test]
    fn unknown_status_in_an_order_is_decode() {
        let mut value = order_value();
        value["status"] = json!("bogus");
        assert!(matches!(order_from(value), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn unsupported_order_type_time_in_force_and_notional_are_unsupported() {
        let mut stop = order_value();
        stop["type"] = json!("stop");
        assert!(matches!(
            order_from(stop),
            Err(AdapterError::Unsupported(_))
        ));

        let mut ioc = order_value();
        ioc["time_in_force"] = json!("ioc");
        assert!(matches!(order_from(ioc), Err(AdapterError::Unsupported(_))));

        let mut notional = order_value();
        notional["qty"] = Value::Null;
        assert!(matches!(
            order_from(notional),
            Err(AdapterError::Unsupported(_))
        ));
    }

    #[test]
    fn bad_side_or_timestamp_is_decode() {
        let mut hold = order_value();
        hold["side"] = json!("hold");
        assert!(matches!(order_from(hold), Err(AdapterError::Decode(_))));

        let mut bad_ts = order_value();
        bad_ts["updated_at"] = json!("yesterday");
        assert!(matches!(order_from(bad_ts), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn sell_side_and_day_tif_map_to_core_enums() {
        let mut value = order_value();
        value["side"] = json!("sell");
        value["time_in_force"] = json!("day");
        let order = order_from(value).unwrap();
        assert_eq!(order.side, OrderSide::Sell);
        assert_eq!(order.time_in_force, TimeInForce::Day);
    }

    #[test]
    fn empty_identifiers_are_decode_not_panics() {
        let mut no_id = order_value();
        no_id["id"] = json!("");
        assert!(matches!(order_from(no_id), Err(AdapterError::Decode(_))));

        let mut no_symbol = order_value();
        no_symbol["symbol"] = json!("");
        assert!(matches!(
            order_from(no_symbol),
            Err(AdapterError::Decode(_))
        ));
    }

    #[test]
    fn hostile_identifiers_never_panic() {
        // Nautilus' unchecked constructors panic on these; they come from the broker.
        for bad in ["", "   ", "ordr\u{e9}"] {
            let mut order = order_value();
            order["client_order_id"] = json!(bad);
            assert!(
                matches!(order_from(order), Err(AdapterError::Unsupported(_))),
                "a client order id of {bad:?} is someone else's order"
            );
            let mut order = order_value();
            order["symbol"] = json!(bad);
            // Must not panic; Nautilus accepts some non-ASCII symbols, so only blank ones
            // are required to fail.
            let result = order_from(order);
            if bad.trim().is_empty() {
                assert!(matches!(result, Err(AdapterError::Decode(_))));
            }
            let position: WirePosition = serde_json::from_value(json!({
                "symbol": bad, "side": "long", "qty": "1", "avg_entry_price": "1",
            }))
            .unwrap();
            let result = position.into_broker_position();
            if bad.trim().is_empty() {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn a_missing_blocking_flag_fails_the_account_read() {
        let missing = r#"{"equity":"10","cash":"10","buying_power":"10","trading_blocked":false}"#;
        assert!(serde_json::from_str::<WireAccount>(missing).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut value = order_value();
        value["brand_new_field"] = json!({"nested": [1, 2, 3]});
        value["another_unknown"] = json!(42);
        let order = order_from(value).unwrap();
        assert_eq!(order.state, OrderState::Submitted);
    }

    #[test]
    fn missing_required_field_fails_to_deserialize() {
        let mut value = order_value();
        value.as_object_mut().unwrap().remove("status");
        assert!(serde_json::from_value::<WireOrder>(value).is_err());
    }

    #[test]
    fn long_position_maps_unsigned_quantity() {
        let wire: WirePosition = serde_json::from_str(
            r#"{"symbol":"AAPL","qty":"5","side":"long","avg_entry_price":"100.0"}"#,
        )
        .unwrap();
        let position = wire.into_broker_position().unwrap();
        assert_eq!(position.symbol.to_string(), "AAPL");
        assert_eq!(position.side, PositionSide::Long);
        assert_eq!(position.qty.to_string(), "5");
        assert_eq!(position.avg_entry_px.to_string(), "100.0");
    }

    #[test]
    fn short_position_drops_leading_minus() {
        let wire: WirePosition = serde_json::from_str(
            r#"{"symbol":"TSLA","qty":"-3","side":"short","avg_entry_price":"250.5"}"#,
        )
        .unwrap();
        let position = wire.into_broker_position().unwrap();
        assert_eq!(position.side, PositionSide::Short);
        assert_eq!(position.qty, parse_qty("3").unwrap());
        assert_eq!(position.qty.to_string(), "3");
    }

    #[test]
    fn position_with_unknown_side_is_decode() {
        let wire: WirePosition = serde_json::from_str(
            r#"{"symbol":"AAPL","qty":"5","side":"flat-ish","avg_entry_price":"100"}"#,
        )
        .unwrap();
        assert!(matches!(
            wire.into_broker_position(),
            Err(AdapterError::Decode(_))
        ));
    }

    #[test]
    fn account_phase_doc_json_maps_money_and_no_block() {
        let wire: WireAccount = serde_json::from_str(
            r#"{"equity":"103820.56","cash":"-23140.2","buying_power":"262113.632","currency":"USD","trading_blocked":false,"account_blocked":false,"trade_suspended_by_user":false,"pattern_day_trader":false}"#,
        )
        .unwrap();
        let account = wire.into_account_snapshot().unwrap();
        assert_eq!(account.equity, Money::from_str("103820.56 USD").unwrap());
        assert_eq!(account.equity.to_string(), "103820.56 USD");
        assert_eq!(account.cash, Money::from_str("-23140.20 USD").unwrap());
        assert_eq!(account.cash.to_string(), "-23140.20 USD");
        assert!(!account.trading_blocked);
    }

    #[test]
    fn each_blocking_flag_alone_sets_trading_blocked() {
        for flag in [
            "trading_blocked",
            "account_blocked",
            "trade_suspended_by_user",
        ] {
            let mut value = json!({
                "equity": "1", "cash": "1", "buying_power": "1", "trading_blocked": false,
                "account_blocked": false, "trade_suspended_by_user": false,
            });
            value[flag] = json!(true);
            let wire: WireAccount = serde_json::from_value(value).unwrap();
            let account = wire.into_account_snapshot().unwrap();
            assert!(account.trading_blocked, "{flag} alone must block");
        }
    }

    #[test]
    fn account_currency_defaults_to_usd_and_bad_amount_is_decode() {
        let wire: WireAccount = serde_json::from_str(
            r#"{"equity":"10","cash":"10","buying_power":"10","trading_blocked":false,"account_blocked":false,"trade_suspended_by_user":false}"#,
        )
        .unwrap();
        let account = wire.into_account_snapshot().unwrap();
        assert_eq!(account.equity, Money::from_str("10 USD").unwrap());

        let wire: WireAccount = serde_json::from_str(
            r#"{"equity":"NaN","cash":"10","buying_power":"10","trading_blocked":false,"account_blocked":false,"trade_suspended_by_user":false}"#,
        )
        .unwrap();
        assert!(matches!(
            wire.into_account_snapshot(),
            Err(AdapterError::Decode(_))
        ));
    }

    #[test]
    fn limit_intent_serialises_to_the_documented_body() {
        let request =
            OrderRequest::from_intent(&intent(OrderType::Limit, Some("500.10"), TimeInForce::Day))
                .unwrap();
        let body = serde_json::to_value(&request).unwrap();
        let expected: Value = serde_json::from_str(
            r#"{"symbol":"SPY","qty":"1.5","side":"buy","type":"limit","time_in_force":"day","limit_price":"500.10","client_order_id":"nisse-1-0"}"#,
        )
        .unwrap();
        assert_eq!(body, expected);
    }

    #[test]
    fn market_intent_omits_limit_price() {
        let request =
            OrderRequest::from_intent(&intent(OrderType::Market, None, TimeInForce::Gtc)).unwrap();
        let body = serde_json::to_value(&request).unwrap();
        let expected: Value = serde_json::from_str(
            r#"{"symbol":"SPY","qty":"1.5","side":"buy","type":"market","time_in_force":"gtc","client_order_id":"nisse-1-0"}"#,
        )
        .unwrap();
        assert_eq!(body, expected);
    }

    #[test]
    fn invalid_intents_are_unsupported() {
        assert!(matches!(
            OrderRequest::from_intent(&intent(OrderType::Limit, None, TimeInForce::Day)),
            Err(AdapterError::Unsupported(_))
        ));
        assert!(matches!(
            OrderRequest::from_intent(&intent(OrderType::Market, Some("1"), TimeInForce::Day)),
            Err(AdapterError::Unsupported(_))
        ));
        assert!(matches!(
            OrderRequest::from_intent(&intent(OrderType::Limit, Some("1"), TimeInForce::Ioc)),
            Err(AdapterError::Unsupported(_))
        ));
    }

    #[test]
    fn replace_body_omits_none_fields() {
        let both = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: Some(parse_qty("2").unwrap()),
            limit_px: Some(parse_price("499.00").unwrap()),
        };
        let body = serde_json::to_value(ReplaceBody::from_request(&both)).unwrap();
        assert_eq!(
            body,
            json!({"qty": "2", "limit_price": "499.00", "client_order_id": "nisse-1-1"})
        );

        let neither = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: None,
            limit_px: None,
        };
        let body = serde_json::to_value(ReplaceBody::from_request(&neither)).unwrap();
        assert_eq!(body, json!({"client_order_id": "nisse-1-1"}));
    }
}
