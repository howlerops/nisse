//! Alpaca trading REST client: the [`Broker`] implementation.

use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, OrderIntent, ReplaceRequest, SubmitError,
    nautilus::{ClientOrderId, VenueOrderId},
};
use reqwest::Method;

use crate::wire::{OrderRequest, ReplaceBody, WireAccount, WireOrder, WirePosition};
use crate::{AdapterError, AlpacaConfig, AlpacaHttp};

/// Orders, positions and account over Alpaca's trading API.
#[derive(Debug, Clone)]
pub struct AlpacaTrading {
    http: AlpacaHttp,
    base_url: String,
}

impl AlpacaTrading {
    #[must_use]
    pub fn new(http: AlpacaHttp, config: &AlpacaConfig) -> Self {
        Self {
            http,
            base_url: config.trading_base_url.clone(),
        }
    }
}

/// Classifies the result of a state-changing request that was sent exactly once. A send error
/// that Alpaca definitely refused is `Rejected`; any other send error, and a response that
/// does not map to an order (the order exists at the broker), is `Unknown`.
fn order_outcome(
    sent: Result<WireOrder, AdapterError>,
) -> Result<BrokerOrder, SubmitError<AdapterError>> {
    match sent {
        Ok(wire) => wire.into_broker_order().map_err(SubmitError::Unknown),
        Err(e) if e.is_definite_rejection() => Err(SubmitError::Rejected(e)),
        Err(e) => Err(SubmitError::Unknown(e)),
    }
}

impl Broker for AlpacaTrading {
    type Error = AdapterError;

    /// `POST /v2/orders`. Sent once. A definite 4xx refusal is `SubmitError::Rejected`;
    /// everything else (transport, timeout, 429, 5xx, undecodable response) is `Unknown`.
    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<AdapterError>> {
        let body = OrderRequest::from_intent(intent).map_err(SubmitError::Rejected)?;
        let url = format!("{}/v2/orders", self.base_url);
        order_outcome(
            self.http
                .send::<_, WireOrder>(Method::POST, &url, Some(&body))
                .await,
        )
    }

    /// `DELETE /v2/orders/{venue_order_id}`.
    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), AdapterError> {
        let url = format!("{}/v2/orders/{venue_order_id}", self.base_url);
        self.http
            .send_discard::<()>(Method::DELETE, &url, None)
            .await
    }

    /// `DELETE /v2/orders`.
    async fn cancel_all(&self) -> Result<(), AdapterError> {
        let url = format!("{}/v2/orders", self.base_url);
        self.http
            .send_discard::<()>(Method::DELETE, &url, None)
            .await
    }

    /// `PATCH /v2/orders/{venue_order_id}`. Same rejected/unknown split as `submit`.
    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<AdapterError>> {
        let body = ReplaceBody::from_request(request);
        let url = format!("{}/v2/orders/{venue_order_id}", self.base_url);
        order_outcome(
            self.http
                .send::<_, WireOrder>(Method::PATCH, &url, Some(&body))
                .await,
        )
    }

    /// `GET /v2/orders:by_client_order_id?client_order_id=...`; a 404 is `Ok(None)`.
    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, AdapterError> {
        let url = format!("{}/v2/orders:by_client_order_id", self.base_url);
        let id = client_order_id.to_string();
        match self
            .http
            .get::<WireOrder>(&url, &[("client_order_id", id.as_str())])
            .await
        {
            Ok(wire) => wire.into_broker_order().map(Some),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `GET /v2/orders?status=open&limit=500`. Orders of a kind this adapter never sends are
    /// left out; any other undecodable order is an error.
    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, AdapterError> {
        let url = format!("{}/v2/orders", self.base_url);
        let wire: Vec<WireOrder> = self
            .http
            .get(&url, &[("status", "open"), ("limit", "500")])
            .await?;
        // Orders this adapter cannot represent (other types or times in force) are not ones the
        // engine sent; leaving them out keeps one hand-placed order from blocking every
        // reconciliation. Their fills still surface as position mismatches.
        let mut orders = Vec::with_capacity(wire.len());
        for order in wire {
            match order.into_broker_order() {
                Ok(order) => orders.push(order),
                Err(AdapterError::Unsupported(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(orders)
    }

    /// `GET /v2/positions`.
    async fn positions(&self) -> Result<Vec<BrokerPosition>, AdapterError> {
        let url = format!("{}/v2/positions", self.base_url);
        let wire: Vec<WirePosition> = self.http.get(&url, &[]).await?;
        wire.into_iter()
            .map(WirePosition::into_broker_position)
            .collect()
    }

    /// `GET /v2/account`.
    async fn account(&self) -> Result<AccountSnapshot, AdapterError> {
        let url = format!("{}/v2/account", self.base_url);
        let wire: WireAccount = self.http.get(&url, &[]).await?;
        wire.into_account_snapshot()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nisse_core::{
        DecisionId, OrderState,
        nautilus::{OrderSide, OrderType, PositionSide, Price, Quantity, TimeInForce, UnixNanos},
    };
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{any, body_json, method, path, query_param},
    };

    use super::*;
    use crate::{Credentials, Feed};

    /// The order object from the "Alpaca reference" section of the phase doc.
    const ORDER_JSON: &str = r#"{"asset_class": "us_equity", "asset_id": "b0b6dd9d-8b9b-48a9-ba46-b9d54906e415", "canceled_at": null, "client_order_id": "5680c4bc-9ac1-4a12-a44c-df427ba53032", "created_at": "2023-12-12T22:31:24.668464435Z", "expired_at": null, "extended_hours": false, "failed_at": null, "filled_at": null, "filled_avg_price": null, "filled_qty": "0", "hwm": null, "id": "7b08df51-c1ac-453c-99f9-323a5f075f0d", "legs": null, "limit_price": "150", "notional": null, "order_class": "", "order_type": "limit", "qty": "2", "replaced_at": null, "replaced_by": null, "replaces": null, "side": "buy", "source": null, "status": "accepted", "stop_price": null, "submitted_at": "2023-12-12T22:31:24.577215743Z", "subtag": null, "symbol": "AAPL", "time_in_force": "gtc", "trail_percent": null, "trail_price": null, "type": "limit", "updated_at": "2023-12-12T22:31:24.668464435Z"}"#;

    const ORDER_ID: &str = "7b08df51-c1ac-453c-99f9-323a5f075f0d";
    const CLIENT_ID: &str = "5680c4bc-9ac1-4a12-a44c-df427ba53032";

    /// Positions from "Requests"; the short's quantity carries a leading minus.
    const POSITIONS_JSON: &str = r#"[{"symbol":"AAPL","qty":"5","side":"long","avg_entry_price":"100.0"},{"symbol":"TSLA","qty":"-3","side":"short","avg_entry_price":"250.5"}]"#;

    /// Account from "Requests".
    const ACCOUNT_JSON: &str = r#"{"equity":"103820.56","cash":"-23140.2","buying_power":"262113.632","currency":"USD","trading_blocked":false,"account_blocked":false,"trade_suspended_by_user":false,"pattern_day_trader":false}"#;

    fn config(base_url: String) -> AlpacaConfig {
        AlpacaConfig {
            trading_base_url: base_url,
            data_base_url: "http://127.0.0.1:1".to_owned(),
            stream_url: "ws://127.0.0.1:1".to_owned(),
            feed: Feed::Iex,
        }
    }

    fn trading(config: &AlpacaConfig) -> AlpacaTrading {
        let creds = Credentials::new("test-key-id", "test-secret");
        let http = AlpacaHttp::new(&creds, Duration::from_secs(5)).unwrap();
        AlpacaTrading::new(http, config)
    }

    fn limit_intent() -> OrderIntent {
        OrderIntent {
            decision_id: DecisionId(1),
            client_order_id: ClientOrderId::new("nisse-1-0"),
            symbol: nisse_core::nautilus::Symbol::new("SPY"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            qty: Quantity::from("1.5"),
            limit_px: Some("500.10".parse::<Price>().unwrap()),
            time_in_force: TimeInForce::Day,
            ts: UnixNanos::from(1),
        }
    }

    fn limit_body() -> Value {
        json!({"symbol":"SPY","qty":"1.5","side":"buy","type":"limit","time_in_force":"day","limit_price":"500.10","client_order_id":"nisse-1-0"})
    }

    fn order_value() -> Value {
        serde_json::from_str(ORDER_JSON).unwrap()
    }

    fn order_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(ORDER_JSON, "application/json")
    }

    fn error_response(status: u16) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({"code": 42210000, "message": "refused"}))
    }

    /// Mounts the one POST that a submit test expects: the documented body, sent exactly once.
    async fn mount_submit(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/v2/orders"))
            .and(body_json(limit_body()))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn submit_sends_exact_body_once_and_maps_the_order() {
        let server = MockServer::start().await;
        mount_submit(&server, order_response()).await;
        let config = config(server.uri());
        let order = trading(&config).submit(&limit_intent()).await.unwrap();
        assert_eq!(order.client_order_id.to_string(), CLIENT_ID);
        assert_eq!(order.venue_order_id.to_string(), ORDER_ID);
        assert_eq!(order.state, OrderState::Submitted);
        assert_eq!(order.qty.to_string(), "2");
        assert_eq!(order.cum_filled_qty.to_string(), "0");
        assert!(order.avg_fill_px.is_none());
    }

    #[tokio::test]
    async fn submit_422_is_rejected_and_sent_once() {
        let server = MockServer::start().await;
        mount_submit(&server, error_response(422)).await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Rejected(AdapterError::Status { status: 422, .. })
        ));
    }

    #[tokio::test]
    async fn submit_403_is_rejected_and_sent_once() {
        let server = MockServer::start().await;
        mount_submit(&server, error_response(403)).await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Rejected(AdapterError::Status { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn submit_500_is_unknown_and_sent_once() {
        let server = MockServer::start().await;
        mount_submit(&server, error_response(500)).await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Unknown(AdapterError::Status { status: 500, .. })
        ));
    }

    #[tokio::test]
    async fn submit_429_is_unknown_and_sent_once() {
        let server = MockServer::start().await;
        mount_submit(&server, error_response(429)).await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Unknown(AdapterError::Status { status: 429, .. })
        ));
    }

    #[tokio::test]
    async fn submit_connection_refused_is_unknown() {
        let config = config("http://127.0.0.1:9".to_owned());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Unknown(AdapterError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn submit_undecodable_200_body_is_unknown_and_sent_once() {
        let server = MockServer::start().await;
        mount_submit(
            &server,
            ResponseTemplate::new(200).set_body_string("not json"),
        )
        .await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(err, SubmitError::Unknown(_)));
    }

    #[tokio::test]
    async fn submit_200_with_unmappable_order_is_unknown_and_sent_once() {
        // The order exists at the broker, so a mapping failure must not read as a rejection.
        let server = MockServer::start().await;
        let mut value = order_value();
        value["type"] = json!("stop");
        mount_submit(&server, ResponseTemplate::new(200).set_body_json(value)).await;
        let config = config(server.uri());
        let err = trading(&config).submit(&limit_intent()).await.unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Unknown(AdapterError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn unsupported_intents_are_rejected_without_any_request() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(order_response())
            .expect(0)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let broker = trading(&config);
        let limit_without_price = OrderIntent {
            limit_px: None,
            ..limit_intent()
        };
        let market_with_price = OrderIntent {
            order_type: OrderType::Market,
            ..limit_intent()
        };
        let ioc = OrderIntent {
            time_in_force: TimeInForce::Ioc,
            ..limit_intent()
        };
        for intent in [&limit_without_price, &market_with_price, &ioc] {
            let err = broker.submit(intent).await.unwrap_err();
            assert!(matches!(
                err,
                SubmitError::Rejected(AdapterError::Unsupported(_))
            ));
        }
    }

    #[tokio::test]
    async fn cancel_hits_delete_order_by_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        trading(&config)
            .cancel(VenueOrderId::new(ORDER_ID))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_error_propagates_as_adapter_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .respond_with(error_response(404))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let err = trading(&config)
            .cancel(VenueOrderId::new(ORDER_ID))
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(404));
    }

    #[tokio::test]
    async fn cancel_all_hits_delete_orders_and_ignores_the_207_list() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v2/orders"))
            .respond_with(
                ResponseTemplate::new(207)
                    .set_body_json(json!([{"id": ORDER_ID, "status": 200, "body": {}}])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        trading(&config).cancel_all().await.unwrap();
    }

    #[tokio::test]
    async fn replace_patches_body_and_maps_the_order() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .and(body_json(
                json!({"qty":"2","limit_price":"499.00","client_order_id":"nisse-1-1"}),
            ))
            .respond_with(order_response())
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let request = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: Some(Quantity::from("2")),
            limit_px: Some("499.00".parse::<Price>().unwrap()),
        };
        let order = trading(&config)
            .replace(VenueOrderId::new(ORDER_ID), &request)
            .await
            .unwrap();
        assert_eq!(order.venue_order_id.to_string(), ORDER_ID);
        assert_eq!(order.state, OrderState::Submitted);
    }

    #[tokio::test]
    async fn replace_omits_absent_fields_from_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .and(body_json(json!({"client_order_id":"nisse-1-1"})))
            .respond_with(order_response())
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let request = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: None,
            limit_px: None,
        };
        trading(&config)
            .replace(VenueOrderId::new(ORDER_ID), &request)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn replace_422_is_rejected_and_sent_once() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .respond_with(error_response(422))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let request = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: Some(Quantity::from("2")),
            limit_px: None,
        };
        let err = trading(&config)
            .replace(VenueOrderId::new(ORDER_ID), &request)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Rejected(AdapterError::Status { status: 422, .. })
        ));
    }

    #[tokio::test]
    async fn replace_500_is_unknown_and_sent_once() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/v2/orders/{ORDER_ID}")))
            .respond_with(error_response(500))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let request = ReplaceRequest {
            new_client_order_id: ClientOrderId::new("nisse-1-1"),
            qty: Some(Quantity::from("2")),
            limit_px: None,
        };
        let err = trading(&config)
            .replace(VenueOrderId::new(ORDER_ID), &request)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SubmitError::Unknown(AdapterError::Status { status: 500, .. })
        ));
    }

    #[tokio::test]
    async fn order_by_client_id_sends_query_and_returns_some() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders:by_client_order_id"))
            .and(query_param("client_order_id", "nisse-1-0"))
            .respond_with(order_response())
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let order = trading(&config)
            .order_by_client_id(ClientOrderId::new("nisse-1-0"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(order.venue_order_id.to_string(), ORDER_ID);
    }

    #[tokio::test]
    async fn order_by_client_id_404_is_none_and_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders:by_client_order_id"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let found = trading(&config)
            .order_by_client_id(ClientOrderId::new("nisse-1-0"))
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn order_by_client_id_500_is_an_error() {
        let server = MockServer::start().await;
        // Reads are retried on 5xx: three attempts in total.
        Mock::given(method("GET"))
            .and(path("/v2/orders:by_client_order_id"))
            .respond_with(ResponseTemplate::new(500))
            .expect(3)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let err = trading(&config)
            .order_by_client_id(ClientOrderId::new("nisse-1-0"))
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(500));
    }

    #[tokio::test]
    async fn open_orders_sends_status_and_limit_and_maps_each_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .and(query_param("status", "open"))
            .and(query_param("limit", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([order_value()])))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let orders = trading(&config).open_orders().await.unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].venue_order_id.to_string(), ORDER_ID);
        assert_eq!(orders[0].state, OrderState::Submitted);
    }

    #[tokio::test]
    async fn open_orders_skips_foreign_order_kinds_but_fails_on_undecodable_ones() {
        let server = MockServer::start().await;
        let mut stop = order_value();
        stop["type"] = json!("stop");
        stop["client_order_id"] = json!("placed-by-hand");
        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([stop, order_value()])))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let orders = trading(&config).open_orders().await.unwrap();
        assert_eq!(
            orders.len(),
            1,
            "the stop order is not one we could have sent"
        );
        assert_eq!(orders[0].venue_order_id.to_string(), ORDER_ID);

        let server = MockServer::start().await;
        let mut bad = order_value();
        bad["status"] = json!("bogus");
        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([order_value(), bad])))
            .mount(&server)
            .await;
        let config = self::config(server.uri());
        let err = trading(&config).open_orders().await.unwrap_err();
        assert!(matches!(err, AdapterError::Decode(_)));
    }

    #[tokio::test]
    async fn positions_map_long_and_short() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/positions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(POSITIONS_JSON, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let positions = trading(&config).positions().await.unwrap();
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0].symbol.to_string(), "AAPL");
        assert_eq!(positions[0].side, PositionSide::Long);
        assert_eq!(positions[0].qty.to_string(), "5");
        assert_eq!(positions[0].avg_entry_px.to_string(), "100.0");
        assert_eq!(positions[1].symbol.to_string(), "TSLA");
        assert_eq!(positions[1].side, PositionSide::Short);
        assert_eq!(positions[1].qty.to_string(), "3");
        assert_eq!(positions[1].avg_entry_px.to_string(), "250.5");
    }

    #[tokio::test]
    async fn account_maps_money_and_no_block() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/account"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(ACCOUNT_JSON, "application/json"))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let account = trading(&config).account().await.unwrap();
        assert_eq!(account.equity.to_string(), "103820.56 USD");
        assert_eq!(account.cash.to_string(), "-23140.20 USD");
        assert!(!account.trading_blocked);
    }

    #[tokio::test]
    async fn account_with_account_blocked_is_trading_blocked() {
        let server = MockServer::start().await;
        let blocked = ACCOUNT_JSON.replace("\"account_blocked\":false", "\"account_blocked\":true");
        Mock::given(method("GET"))
            .and(path("/v2/account"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(blocked, "application/json"))
            .expect(1)
            .mount(&server)
            .await;
        let config = config(server.uri());
        let account = trading(&config).account().await.unwrap();
        assert!(account.trading_blocked);
    }
}
