//! Shared domain types for the nisse workspace.
//!
//! Anything used by more than one crate lives here, once. NautilusTrader already defines the
//! market and order vocabulary (prices, quantities, identifiers, order enums); those types are
//! re-exported from [`nautilus`] and must be used as they are rather than redefined. Money and
//! quantities are always the Nautilus fixed-point types, never `f64`.

mod calendar;
mod canonical;
mod clock;
mod error;
mod hash;
mod ids;
mod order;
mod record;
mod types;

pub use calendar::{FULL_SESSION_NANOS, MarketCalendar, Session};
pub use canonical::{
    Features, bars_snapshot_id, canonical_json, config_hash, feature_hash, features_from_json,
    features_to_json,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::CoreError;
pub use hash::Hash32;
pub use ids::{DecisionId, StrategyVersionId};
pub use order::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, Fill, FillDetail, IntentLog, OpenOrders,
    OrderIntent, OrderState, OrderUpdate, ReplaceRequest, SubmitError,
};
pub use record::{
    Decision, DecisionLog, EvalScore, KillSwitchSource, MarketSnapshot, OrderEvent, PositionEvent,
    RecordSink, RestoredOrder, RiskCheck, RiskEvent, StoreRecord, StrategyVersion, Trial,
};
pub use types::{Action, RiskEventKind, SubjectType, TrialKind};

/// The NautilusTrader types this workspace builds on. Import them from here so the pinned
/// version is named in one place.
pub mod nautilus {
    pub use nautilus_core::UnixNanos;
    pub use nautilus_model::{
        data::{Bar, BarSpecification, BarType, QuoteTick},
        enums::{
            AggregationSource, BarAggregation, OrderSide, OrderStatus, OrderType, PositionSide,
            PriceType, TimeInForce,
        },
        identifiers::{
            ClientOrderId, InstrumentId, StrategyId, Symbol, TradeId, Venue, VenueOrderId,
        },
        types::{Currency, Money, Price, Quantity},
    };
}
