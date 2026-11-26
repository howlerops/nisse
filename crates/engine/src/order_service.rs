//! Turning broker order activity into store rows: the glue between the order stream, the
//! periodic reconciliation, the tracker, the position book and the store.

use nisse_core::{Broker, Clock, OrderUpdate, RecordSink};

use crate::{EngineError, OrderTracker, PositionBook, SyncOptions, SyncReport};

/// The engine's order state, kept together because every broker message touches both.
#[derive(Debug, Default)]
pub struct OrderBookState {
    pub tracker: OrderTracker,
    pub positions: PositionBook,
}

/// Applies one order stream update and records what it changed.
///
/// - The tracker applies the update (`OrderTracker::apply`).
/// - If the state changed or a fill was produced, one `StoreRecord::Order` is emitted with
///   the order's new row (`OrderEvent::of` from the tracked order, event time `update.ts`) and
///   the fill, so the store writes both in one transaction.
/// - A fill also updates the position book (`PositionBook::apply_fill` with the tracked
///   order's symbol and side) and emits the resulting `StoreRecord::Position`.
/// - An update for an order the engine is not tracking is not an error here: it returns
///   `Ok(false)` and changes nothing (reconciliation reports foreign orders).
///
/// Returns `Ok(true)` when the update belonged to a tracked order. A sink failure is
/// `Store(..)`: the caller must stop trading.
#[allow(unused_variables, reason = "stub: Phase 2 task E3")]
pub async fn record_stream_update<K: RecordSink>(
    state: &mut OrderBookState,
    sink: &K,
    update: &OrderUpdate,
) -> Result<bool, EngineError> {
    todo!("Phase 2 task E3")
}

/// Runs one reconciliation pass against the broker and records everything it changed.
///
/// 1. `sync_with_broker` with the tracker and `state.positions.quantities()`.
/// 2. For every order with a state change or a fill in the report, emit one
///    `StoreRecord::Order` (the tracked order's row at `now`, with its fill if it has one in
///    the report; an order with several fills in one pass gets one record per fill, and the
///    row's cumulative quantity is the tracker's current figure).
/// 3. For every `Finding::PositionMismatch`, `PositionBook::adopt` the broker's quantity and
///    average price and emit the resulting `StoreRecord::Position`. Fills found by the pass
///    are **not** applied to the position book: the broker's position already includes them.
/// 4. Log, with `tracing::error!`, each `UnknownAtBroker` finding, each problem, and a
///    non-zero `unrepresentable_at_broker`: something else is trading the account, or the
///    broker's figures are inconsistent.
///
/// Returns the report. A broker failure is `Broker(..)` (nothing was changed or recorded
/// beyond what the sync applied before failing); a sink failure is `Store(..)`.
#[allow(unused_variables, reason = "stub: Phase 2 task E3")]
pub async fn reconcile_and_record<B: Broker, K: RecordSink, C: Clock>(
    broker: &B,
    state: &mut OrderBookState,
    sink: &K,
    clock: &C,
    options: SyncOptions,
) -> Result<SyncReport, EngineError> {
    todo!("Phase 2 task E3")
}
