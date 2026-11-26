//! Turning broker order activity into store rows: the glue between the order stream, the
//! periodic reconciliation, the tracker, the position book and the store.

use nisse_core::{
    Broker, Clock, IntentLog, OrderEvent, OrderIntent, OrderUpdate, RecordSink, StoreRecord,
    nautilus::{ClientOrderId, UnixNanos},
};

use crate::{
    EngineError, Finding, OrderGateway, OrderTracker, PositionBook, Submitted, SyncOptions,
    SyncReport, TrackedOrder, sync_with_broker,
};

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
pub async fn record_stream_update<K: RecordSink>(
    state: &mut OrderBookState,
    sink: &K,
    update: &OrderUpdate,
) -> Result<bool, EngineError> {
    let applied = match state
        .tracker
        .apply(&update.order, update.fill.as_ref(), update.ts)
    {
        Ok(applied) => applied,
        Err(EngineError::UnknownOrder(_)) => return Ok(false),
        Err(err) => return Err(err),
    };
    if applied.state_change.is_none() && applied.fill.is_none() {
        return Ok(true);
    }

    let tracked = tracked_order(&state.tracker, update.order.client_order_id)?;
    emit(
        sink,
        StoreRecord::Order {
            event: order_row(&tracked, update.ts),
            fill: applied.fill,
        },
    )
    .await?;

    if let Some(fill) = applied.fill {
        let event = state.positions.apply_fill(
            tracked.intent.symbol,
            tracked.intent.side,
            fill.qty.as_decimal(),
            fill.px.as_decimal(),
            fill.ts,
        );
        emit(sink, StoreRecord::Position(event)).await?;
    }
    Ok(true)
}

/// Submits an order through the gateway and records what came back.
///
/// The gateway has already written the intent row (write-ahead). This adds the row for what
/// the broker answered, which no stream message will repeat:
///
/// - accepted, or already (partly) filled: one `StoreRecord::Order` with the order's row and
///   the fill if there is one, and for a fill the position update and its
///   `StoreRecord::Position`. Without this a market order that comes back filled would never
///   get a fill row: the later stream message carries no new quantity.
/// - definitely rejected by the broker: one `StoreRecord::Order` with state `rejected`, so
///   the store shows the order ended. The original error is returned.
/// - the intent could not be recorded: nothing is recorded. The order has no intent row of
///   its own, so a `rejected` row would either be an orphan or, when the id was a reused one,
///   land on top of another order's history.
/// - outcome unknown: nothing is recorded; the order is still an intent and reconciliation
///   resolves it.
///
/// If recording fails after a successful submit the error is `Store(..)`: the order is live
/// at the broker and the caller must stop trading and reconcile.
pub async fn submit_and_record<B: Broker, L: IntentLog, K: RecordSink>(
    gateway: &OrderGateway<B, L>,
    state: &mut OrderBookState,
    sink: &K,
    intent: &OrderIntent,
) -> Result<Submitted, EngineError> {
    let result = gateway.submit(&mut state.tracker, intent).await;
    record_outcome(state, sink, intent.client_order_id, result).await
}

/// Replaces a live order through the gateway and records what came back, exactly as
/// [`submit_and_record`] does for the replacement order. The original order's own end
/// (`replaced`) arrives from the broker through the stream or reconciliation.
pub async fn replace_and_record<B: Broker, L: IntentLog, K: RecordSink>(
    gateway: &OrderGateway<B, L>,
    state: &mut OrderBookState,
    sink: &K,
    original: ClientOrderId,
    replacement: &OrderIntent,
) -> Result<Submitted, EngineError> {
    let result = gateway
        .replace(&mut state.tracker, original, replacement)
        .await;
    record_outcome(state, sink, replacement.client_order_id, result).await
}

/// Records the gateway's answer for the order `id`. Shared by submit and replace.
async fn record_outcome<K: RecordSink>(
    state: &mut OrderBookState,
    sink: &K,
    id: ClientOrderId,
    result: Result<Submitted, EngineError>,
) -> Result<Submitted, EngineError> {
    match result {
        Ok(submitted) => {
            let applied = submitted.applied;
            if applied.state_change.is_none() && applied.fill.is_none() {
                return Ok(submitted);
            }
            let tracked = tracked_order(&state.tracker, id)?;
            emit(
                sink,
                StoreRecord::Order {
                    event: order_row(&tracked, tracked.last_update_ts),
                    fill: applied.fill,
                },
            )
            .await?;
            if let Some(fill) = applied.fill {
                let event = state.positions.apply_fill(
                    tracked.intent.symbol,
                    tracked.intent.side,
                    fill.qty.as_decimal(),
                    fill.px.as_decimal(),
                    fill.ts,
                );
                emit(sink, StoreRecord::Position(event)).await?;
            }
            Ok(submitted)
        }
        Err(error @ EngineError::OrderRejected { .. }) => {
            let tracked = tracked_order(&state.tracker, id)?;
            emit(
                sink,
                StoreRecord::Order {
                    event: order_row(&tracked, tracked.last_update_ts),
                    fill: None,
                },
            )
            .await?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

/// Runs one reconciliation pass against the broker and records everything it changed.
///
/// 1. `sync_with_broker` with the tracker and `state.positions.quantities()`.
/// 2. For every order with a state change or a fill in the report, emit one
///    `StoreRecord::Order` (the tracked order's row at its last update time, with its fill if it has one in
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
pub async fn reconcile_and_record<B: Broker, K: RecordSink, C: Clock>(
    broker: &B,
    state: &mut OrderBookState,
    sink: &K,
    clock: &C,
    options: SyncOptions,
) -> Result<SyncReport, EngineError> {
    let now = clock.now();
    let mut quantities = state.positions.quantities();
    let report =
        sync_with_broker(broker, &mut state.tracker, &mut quantities, now, options).await?;

    // Each order once, in first-appearance order: state changes first, then fills.
    let mut ids: Vec<ClientOrderId> = Vec::with_capacity(report.state_changes.len());
    let changed = report
        .state_changes
        .iter()
        .map(|change| change.client_order_id);
    let filled = report.fills.iter().map(|fill| fill.client_order_id);
    for id in changed.chain(filled) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    for id in ids {
        let tracked = tracked_order(&state.tracker, id)?;
        // Stamped with the tracker's own time for the order: the broker's time when the state
        // came from the broker, `now` only for a locally decided state. Mixing the engine's
        // clock into broker-derived rows would let clock skew reorder an order's history.
        let event = order_row(&tracked, tracked.last_update_ts);
        let mut has_fill = false;
        for fill in report
            .fills
            .iter()
            .filter(|fill| fill.client_order_id == id)
        {
            has_fill = true;
            emit(
                sink,
                StoreRecord::Order {
                    event,
                    fill: Some(*fill),
                },
            )
            .await?;
        }
        if !has_fill {
            emit(sink, StoreRecord::Order { event, fill: None }).await?;
        }
    }

    // The broker's position already includes the fills found above, so only the position
    // figures are adopted here, never the fills.
    for finding in &report.findings {
        if let Finding::PositionMismatch {
            symbol,
            broker,
            broker_avg_px,
            ..
        } = *finding
            && let Some(event) = state.positions.adopt(symbol, broker, broker_avg_px, now)
        {
            emit(sink, StoreRecord::Position(event)).await?;
        }
    }

    for finding in &report.findings {
        if let Finding::UnknownAtBroker { order } = finding {
            tracing::error!(
                client_order_id = %order.client_order_id,
                "open order at the broker that the engine never sent"
            );
        }
    }
    for problem in &report.problems {
        tracing::error!(
            client_order_id = %problem.client_order_id,
            reason = %problem.reason,
            "order could not be reconciled"
        );
    }
    if report.unrepresentable_at_broker > 0 {
        tracing::error!(
            count = report.unrepresentable_at_broker,
            "open orders of a kind the engine never sends"
        );
    }

    Ok(report)
}

/// Sends one record to the sink. A sink failure means the store is gone.
async fn emit<K: RecordSink>(sink: &K, record: StoreRecord) -> Result<(), EngineError> {
    sink.emit(record)
        .await
        .map_err(|err| EngineError::Store(Box::new(err)))
}

/// The row for the engine's current view of a tracked order, at event time `ts`.
fn order_row(tracked: &TrackedOrder, ts: UnixNanos) -> OrderEvent {
    OrderEvent::of(
        &tracked.intent,
        tracked.state,
        tracked.venue_order_id,
        tracked.cum_filled_qty,
        ts,
    )
}

/// A copy of the tracked order. Errors with `UnknownOrder` when it is not tracked.
fn tracked_order(tracker: &OrderTracker, id: ClientOrderId) -> Result<TrackedOrder, EngineError> {
    tracker
        .get(id)
        .copied()
        .ok_or(EngineError::UnknownOrder(id))
}
