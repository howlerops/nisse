//! Alpaca order update stream (`trade_updates` websocket).
//!
//! The stream is a latency optimisation, not the record: updates can be lost across a
//! reconnect, so every [`StreamEvent::Connected`] tells the consumer to reconcile against the
//! broker's REST state. Order state always comes from the embedded order's cumulative
//! figures, so a missed or duplicated message cannot corrupt it.

use std::time::Duration;

use nisse_core::OrderUpdate;
use tokio::sync::mpsc;

use crate::{AdapterError, AlpacaConfig, Credentials};

/// First reconnect delay; doubles up to [`MAX_RECONNECT_DELAY`].
pub const MIN_RECONNECT_DELAY: Duration = Duration::from_secs(1);
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// What the stream task sends to its consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "boxing the update would add a heap allocation per order event"
)]
pub enum StreamEvent {
    /// Authenticated and listening. Sent after every (re)connect: reconcile now.
    Connected,
    /// The connection dropped; a reconnect follows.
    Disconnected,
    Update(OrderUpdate),
}

/// One decoded websocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "boxing the update would add a heap allocation per order event"
)]
pub(crate) enum StreamMessage {
    /// `{"stream":"authorization","data":{"status":"authorized",...}}`
    Authorized,
    /// `{"stream":"listening","data":{"streams":["trade_updates"]}}`
    Listening,
    Update(OrderUpdate),
    /// A trade update whose event carries no order change we track (see phase doc).
    Ignored,
}

/// Decodes one websocket payload (Alpaca sends JSON in text frames on live and in binary
/// frames on paper; both arrive here as bytes).
///
/// Errors: `Stream` for an `unauthorized` status or an `{"action":"error",...}` message;
/// `Decode` for anything malformed or an unknown stream name.
#[allow(unused_variables, reason = "stub: Phase 1 task T3")]
pub(crate) fn parse_stream_message(payload: &[u8]) -> Result<StreamMessage, AdapterError> {
    todo!("Phase 1 task T3")
}

/// Runs the order stream until the receiver of `events` is dropped, then returns `Ok(())`.
///
/// Connects to `config.stream_url`, authenticates, subscribes to `trade_updates`, then
/// forwards every update. `events` is bounded and sends wait for room (back-pressure): an
/// order update is never dropped. On any connection or protocol failure it sends
/// `Disconnected`, waits (starting at `min_reconnect_delay`, doubling to
/// [`MAX_RECONNECT_DELAY`], reset after a successful connect) and reconnects. An
/// authentication failure is returned as `Err` immediately: retrying bad credentials is
/// pointless.
#[allow(unused_variables, reason = "stub: Phase 1 task T3")]
pub async fn run_order_stream(
    config: &AlpacaConfig,
    credentials: &Credentials,
    events: mpsc::Sender<StreamEvent>,
    min_reconnect_delay: Duration,
) -> Result<(), AdapterError> {
    todo!("Phase 1 task T3")
}
