use nisse_core::nautilus::ClientOrderId;

/// A boxed error from a collaborator (broker, intent log) whose concrete type the engine does
/// not need to know.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    /// An intent reused a client order id that is already tracked.
    #[error("client order id {0} is already tracked")]
    DuplicateOrder(ClientOrderId),
    /// A broker update or request named an order the engine is not tracking.
    #[error("order {0} is not tracked")]
    UnknownOrder(ClientOrderId),
    /// The write-ahead intent could not be stored, so the order was not sent.
    #[error("intent for {client_order_id} was not recorded; order not sent: {source}")]
    IntentNotRecorded {
        client_order_id: ClientOrderId,
        #[source]
        source: BoxError,
    },
    /// The broker definitely refused the order; nothing exists at the broker.
    #[error("order {client_order_id} rejected: {source}")]
    OrderRejected {
        client_order_id: ClientOrderId,
        #[source]
        source: BoxError,
    },
    /// The submit may or may not have reached the broker. Reconcile by client order id;
    /// never resend.
    #[error("outcome of order {client_order_id} is unknown: {source}")]
    OrderOutcomeUnknown {
        client_order_id: ClientOrderId,
        #[source]
        source: BoxError,
    },
    /// A replacement intent names a different symbol or side than the order it replaces.
    #[error(
        "replacement {replacement} does not match order {original} (symbol and side must be equal)"
    )]
    ReplaceMismatch {
        original: ClientOrderId,
        replacement: ClientOrderId,
    },
    /// A broker read failed during reconciliation.
    #[error("broker request failed: {0}")]
    Broker(#[source] BoxError),
    /// Broker figures that cannot be combined (for example a cumulative average price
    /// without a quantity).
    #[error("inconsistent broker data for {client_order_id}: {reason}")]
    InconsistentBrokerData {
        client_order_id: ClientOrderId,
        reason: &'static str,
    },
}
