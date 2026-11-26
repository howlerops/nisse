//! What the engine writes to the decision store, and the traits it writes through.
//!
//! One struct per store table. Quantities and prices are exact decimals; ratios and
//! statistics are `f64` (they are never money).

use std::future::Future;

use nautilus_core::UnixNanos;
use nautilus_model::{
    enums::{OrderSide, OrderType, TimeInForce},
    identifiers::{ClientOrderId, Symbol, VenueOrderId},
    types::{Price, Quantity},
};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::{
    Action, BrokerOrder, DecisionId, Features, Fill, Hash32, OrderIntent, OrderState,
    StrategyVersionId, SubjectType, TrialKind,
};

/// One immutable strategy configuration at one git SHA (`strategy_version`).
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyVersion {
    /// 40 lowercase hex characters.
    pub git_sha: String,
    /// [`crate::config_hash`] of `params`.
    pub config_hash: Hash32,
    /// Must contain a string field `"strategy"` naming the strategy implementation.
    pub params: Value,
    pub parent_id: Option<StrategyVersionId>,
    pub ts: UnixNanos,
}

/// One evaluation run (`trial_registry`). Every backtest, shadow and live run registers one;
/// the lifetime count never resets.
#[derive(Debug, Clone, PartialEq)]
pub struct Trial {
    pub strategy_version_id: StrategyVersionId,
    pub kind: TrialKind,
    /// The data the run covered: `[data_start, data_end)`.
    pub data_start: UnixNanos,
    pub data_end: UnixNanos,
    pub result: Option<Value>,
    pub ts: UnixNanos,
}

/// One decision for one symbol (`decision`), with everything needed to replay it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub strategy_version_id: StrategyVersionId,
    pub symbol: Symbol,
    pub action: Action,
    /// Signed target change in shares: positive buys, negative sells, zero for hold and skip.
    pub intended_qty: Decimal,
    pub expected_edge_bps: Option<f64>,
    pub expected_vol: Option<f64>,
    pub features: Features,
    /// [`crate::feature_hash`] of `features`.
    pub feature_hash: Hash32,
    /// Names the market data the features were computed from.
    pub data_snapshot_id: String,
    pub rng_seed: u64,
    pub reason_code: String,
    pub ts: UnixNanos,
}

/// The quote and market state seen at a point in time (`market_snapshot`).
#[derive(Debug, Clone, PartialEq)]
pub struct MarketSnapshot {
    pub symbol: Symbol,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    pub mid: Option<Decimal>,
    pub spread_bps: Option<f64>,
    pub adv: Option<Decimal>,
    pub vol: Option<f64>,
    pub regime_tag: Option<String>,
    pub ts: UnixNanos,
}

/// The position in a symbol after a change (`position_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEvent {
    pub symbol: Symbol,
    /// Signed: negative is short.
    pub qty_after: Decimal,
    pub avg_cost: Decimal,
    pub ts: UnixNanos,
}

/// One risk rule evaluated for a decision (`risk_check`).
#[derive(Debug, Clone, PartialEq)]
pub struct RiskCheck {
    pub decision_id: DecisionId,
    pub rule: String,
    pub limit_value: Decimal,
    pub actual_value: Decimal,
    pub passed: bool,
    pub ts: UnixNanos,
}

/// One state of an order (`order_event`). Self-contained: the intent row alone is enough to
/// recover the order after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderEvent {
    pub decision_id: DecisionId,
    pub client_order_id: ClientOrderId,
    /// `None` on the intent row.
    pub broker_order_id: Option<VenueOrderId>,
    pub state: OrderState,
    pub order_type: OrderType,
    pub limit_px: Option<Price>,
    pub qty: Quantity,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub time_in_force: TimeInForce,
    pub cum_filled_qty: Quantity,
    pub ts: UnixNanos,
}

impl OrderEvent {
    /// The write-ahead row for an intent: state `Intent`, nothing filled, no broker id.
    #[must_use]
    pub fn intent(intent: &OrderIntent) -> Self {
        Self {
            decision_id: intent.decision_id,
            client_order_id: intent.client_order_id,
            broker_order_id: None,
            state: OrderState::Intent,
            order_type: intent.order_type,
            limit_px: intent.limit_px,
            qty: intent.qty,
            symbol: intent.symbol,
            side: intent.side,
            time_in_force: intent.time_in_force,
            cum_filled_qty: Quantity::zero(intent.qty.precision),
            ts: intent.ts,
        }
    }

    /// A row for the engine's current view of an order: the intent's terms with the state,
    /// broker id and cumulative fill the tracker holds. `state` and `venue_order_id` are
    /// passed separately because a locally decided state (rejected, never-arrived) has no
    /// broker order behind it.
    #[must_use]
    pub fn of(
        intent: &OrderIntent,
        state: OrderState,
        venue_order_id: Option<VenueOrderId>,
        cum_filled_qty: Quantity,
        ts: UnixNanos,
    ) -> Self {
        Self {
            broker_order_id: venue_order_id,
            state,
            cum_filled_qty,
            ts,
            ..Self::intent(intent)
        }
    }

    /// A row for a broker order the engine has no intent for (it only knows the broker's
    /// terms). Used when recording what reconciliation found.
    #[must_use]
    pub fn from_broker(decision_id: DecisionId, order: &BrokerOrder, ts: UnixNanos) -> Self {
        Self {
            decision_id,
            client_order_id: order.client_order_id,
            broker_order_id: Some(order.venue_order_id),
            state: order.state,
            order_type: order.order_type,
            limit_px: order.limit_px,
            qty: order.qty,
            symbol: order.symbol,
            side: order.side,
            time_in_force: order.time_in_force,
            cum_filled_qty: order.cum_filled_qty,
            ts,
        }
    }
}

/// What the store knows about an order that may still be live: enough to rebuild the engine's
/// order tracker after a restart without re-emitting fills that were already recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoredOrder {
    /// Rebuilt from the order's intent row.
    pub intent: OrderIntent,
    pub venue_order_id: Option<VenueOrderId>,
    /// From the order's latest row (highest `event_ts`, then `id`).
    pub state: OrderState,
    pub cum_filled_qty: Quantity,
    /// Quantity-weighted average of the order's `fill` rows; `None` when it has none.
    pub avg_fill_px: Option<Price>,
    pub last_update_ts: UnixNanos,
}

/// One metric value for a decision or strategy version (`eval_score`).
#[derive(Debug, Clone, PartialEq)]
pub struct EvalScore {
    pub subject_id: i64,
    pub subject_type: SubjectType,
    pub metric: String,
    pub value: f64,
    pub eval_version: String,
    pub ts: UnixNanos,
}

/// A row the engine hands to the store without waiting for it to be written.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "boxing the common variants would add a heap allocation per record"
)]
pub enum StoreRecord {
    Snapshot(MarketSnapshot),
    Position(PositionEvent),
    Risk(RiskCheck),
    /// An order's new state and, when it filled, the fill. The store writes both in one
    /// transaction: a fill row without the order row carrying its cumulative quantity would
    /// make a restart count that fill again.
    Order {
        event: OrderEvent,
        fill: Option<Fill>,
    },
}

/// Where decisions are written. A decision must be stored (and have its id) before any order
/// can refer to it, so this write is awaited.
pub trait DecisionLog: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn record_decision(
        &self,
        decision: &Decision,
    ) -> impl Future<Output = Result<DecisionId, Self::Error>> + Send;
}

/// Where everything else is written, off the decision path: a bounded queue to a writer
/// task. `emit` waits when the queue is full (back-pressure) and fails only when the store is
/// gone, in which case the engine must stop trading.
pub trait RecordSink: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn emit(&self, record: StoreRecord) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
