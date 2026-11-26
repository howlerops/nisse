//! Shared domain types for the nisse workspace.
//!
//! Anything used by more than one crate lives here, once. NautilusTrader already defines the
//! market and order vocabulary (prices, quantities, identifiers, order enums); those types are
//! re-exported from [`nautilus`] and must be used as they are rather than redefined. Money and
//! quantities are always the Nautilus fixed-point types, never `f64`.

mod error;
mod hash;
mod ids;
mod types;

pub use error::CoreError;
pub use hash::Hash32;
pub use ids::{DecisionId, StrategyVersionId};
pub use types::{Action, SubjectType, TrialKind};

/// The NautilusTrader types this workspace builds on. Import them from here so the pinned
/// version is named in one place.
pub mod nautilus {
    pub use nautilus_core::UnixNanos;
    pub use nautilus_model::{
        enums::{OrderSide, OrderStatus, OrderType, TimeInForce},
        identifiers::{ClientOrderId, InstrumentId, StrategyId, Symbol, Venue, VenueOrderId},
        types::{Money, Price, Quantity},
    };
}
