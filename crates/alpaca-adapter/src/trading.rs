//! Alpaca trading REST client: the [`Broker`] implementation.

use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, OrderIntent, ReplaceRequest, SubmitError,
    nautilus::{ClientOrderId, VenueOrderId},
};

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

#[allow(unused_variables, reason = "stub: Phase 1 task T1")]
impl Broker for AlpacaTrading {
    type Error = AdapterError;

    /// `POST /v2/orders`. Sent once. A definite 4xx refusal is `SubmitError::Rejected`;
    /// everything else (transport, timeout, 429, 5xx, undecodable response) is `Unknown`.
    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<AdapterError>> {
        todo!("Phase 1 task T1")
    }

    /// `DELETE /v2/orders/{venue_order_id}`.
    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), AdapterError> {
        todo!("Phase 1 task T1")
    }

    /// `DELETE /v2/orders`.
    async fn cancel_all(&self) -> Result<(), AdapterError> {
        todo!("Phase 1 task T1")
    }

    /// `PATCH /v2/orders/{venue_order_id}`. Same rejected/unknown split as `submit`.
    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<AdapterError>> {
        todo!("Phase 1 task T1")
    }

    /// `GET /v2/orders:by_client_order_id?client_order_id=...`; a 404 is `Ok(None)`.
    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, AdapterError> {
        todo!("Phase 1 task T1")
    }

    /// `GET /v2/orders?status=open&limit=500`.
    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, AdapterError> {
        todo!("Phase 1 task T1")
    }

    /// `GET /v2/positions`.
    async fn positions(&self) -> Result<Vec<BrokerPosition>, AdapterError> {
        todo!("Phase 1 task T1")
    }

    /// `GET /v2/account`.
    async fn account(&self) -> Result<AccountSnapshot, AdapterError> {
        todo!("Phase 1 task T1")
    }
}
