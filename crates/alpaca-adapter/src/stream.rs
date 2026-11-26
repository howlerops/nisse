//! Alpaca order update stream (`trade_updates` websocket).
//!
//! The stream is a latency optimisation, not the record: updates can be lost across a
//! reconnect, so every [`StreamEvent::Connected`] tells the consumer to reconcile against the
//! broker's REST state. Order state always comes from the embedded order's cumulative
//! figures, so a missed or duplicated message cannot corrupt it.

use std::convert::Infallible;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nisse_core::nautilus::TradeId;
use nisse_core::{FillDetail, OrderUpdate};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::parse::{parse_price, parse_qty, parse_ts};
use crate::wire::WireOrder;
use crate::{AdapterError, AlpacaConfig, Credentials};

/// First reconnect delay; doubles up to [`MAX_RECONNECT_DELAY`].
pub const MIN_RECONNECT_DELAY: Duration = Duration::from_secs(1);
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Longest execution id that `TradeId::new` accepts; longer ids would panic.
const MAX_TRADE_ID_LEN: usize = 36;
/// Bounds the connect and each handshake reply, so a silent server cannot stall the task.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Error text for a rejected login. The handshake matches on it to tell a fatal authorization
/// failure from a retryable stream error.
const UNAUTHORIZED: &str = "authorization rejected by server";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

/// Every field any stream message can carry, all optional: one pass over the payload, and the
/// caller picks the fields its message type needs.
#[derive(Deserialize)]
struct StreamEnvelope {
    stream: Option<String>,
    action: Option<String>,
    data: Option<StreamData>,
}

#[derive(Deserialize)]
struct StreamData {
    status: Option<String>,
    error_message: Option<String>,
    event: Option<String>,
    execution_id: Option<String>,
    price: Option<String>,
    qty: Option<String>,
    timestamp: Option<String>,
    order: Option<WireOrder>,
}

fn decode(message: &str) -> AdapterError {
    AdapterError::Decode(message.to_owned())
}

/// Decodes one websocket payload (Alpaca sends JSON in text frames on live and in binary
/// frames on paper; both arrive here as bytes).
///
/// Errors: `Stream` for an `unauthorized` status or an `{"action":"error",...}` message;
/// `Decode` for anything malformed or an unknown stream name.
pub(crate) fn parse_stream_message(payload: &[u8]) -> Result<StreamMessage, AdapterError> {
    let envelope: StreamEnvelope = serde_json::from_slice(payload)
        .map_err(|error| AdapterError::Decode(format!("malformed stream message: {error}")))?;
    let StreamEnvelope {
        stream,
        action,
        data,
    } = envelope;
    if let Some(action) = action {
        return match action.as_str() {
            "error" => {
                let message = data
                    .and_then(|data| data.error_message)
                    .ok_or_else(|| decode("error action without error_message"))?;
                Err(AdapterError::Stream(message))
            }
            other => Err(AdapterError::Decode(format!("unknown action: {other}"))),
        };
    }
    let stream = stream.ok_or_else(|| decode("message has neither action nor stream"))?;
    match stream.as_str() {
        "listening" => Ok(StreamMessage::Listening),
        "authorization" => authorization(data.ok_or_else(|| decode("missing data"))?),
        "trade_updates" => trade_update(data.ok_or_else(|| decode("missing data"))?),
        other => Err(AdapterError::Decode(format!("unknown stream: {other}"))),
    }
}

fn authorization(data: StreamData) -> Result<StreamMessage, AdapterError> {
    match data.status.as_deref() {
        Some("authorized") => Ok(StreamMessage::Authorized),
        Some("unauthorized") => Err(AdapterError::Stream(UNAUTHORIZED.to_owned())),
        _ => Err(decode("authorization without a known status")),
    }
}

fn trade_update(data: StreamData) -> Result<StreamMessage, AdapterError> {
    let StreamData {
        event,
        execution_id,
        price,
        qty,
        timestamp,
        order,
        ..
    } = data;
    let event = event.ok_or_else(|| decode("trade update without event"))?;
    if matches!(
        event.as_str(),
        "order_replace_rejected" | "order_cancel_rejected"
    ) {
        return Ok(StreamMessage::Ignored);
    }
    let order = order.ok_or_else(|| decode("trade update without order"))?;
    let fill = match event.as_str() {
        "fill" | "partial_fill" => Some(fill_detail(execution_id, price, qty)?),
        _ => None,
    };
    // An order this adapter cannot represent (a stop order placed by hand in the web UI, say)
    // is by construction not one the engine sent. Skipping it keeps one foreign order from
    // forcing an endless reconnect loop; its effect on positions still shows up in
    // reconciliation.
    let order = match order.into_broker_order() {
        Ok(order) => order,
        Err(AdapterError::Unsupported(_)) => return Ok(StreamMessage::Ignored),
        Err(error) => return Err(error),
    };
    let ts = match timestamp {
        Some(text) => parse_ts(&text)?,
        None => order.ts,
    };
    Ok(StreamMessage::Update(OrderUpdate { order, fill, ts }))
}

fn fill_detail(
    execution_id: Option<String>,
    price: Option<String>,
    qty: Option<String>,
) -> Result<FillDetail, AdapterError> {
    let execution_id = execution_id.ok_or_else(|| decode("fill without execution_id"))?;
    if execution_id.is_empty() || execution_id.len() > MAX_TRADE_ID_LEN {
        return Err(decode("execution_id must be 1 to 36 bytes"));
    }
    let price = price.ok_or_else(|| decode("fill without price"))?;
    let qty = qty.ok_or_else(|| decode("fill without qty"))?;
    Ok(FillDetail {
        trade_id: TradeId::new(&execution_id),
        px: parse_price(&price)?,
        qty: parse_qty(&qty)?,
    })
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
pub async fn run_order_stream(
    config: &AlpacaConfig,
    credentials: &Credentials,
    events: mpsc::Sender<StreamEvent>,
    min_reconnect_delay: Duration,
) -> Result<(), AdapterError> {
    let mut delay = min_reconnect_delay;
    loop {
        if events.is_closed() {
            return Ok(());
        }
        let mut connected = false;
        let attempt = match run_connection(config, credentials, &events, &mut connected).await {
            Ok(never) => match never {},
            Err(attempt) => attempt,
        };
        match attempt {
            Attempt::Closed => return Ok(()),
            Attempt::Unauthorized => {
                return Err(AdapterError::Stream(UNAUTHORIZED.to_owned()));
            }
            // The error detail is not surfaced: the consumer learns of the loss through
            // `Disconnected` and reconciles on the next `Connected`.
            Attempt::Failed(_) => {}
        }
        if connected {
            if events.send(StreamEvent::Disconnected).await.is_err() {
                return Ok(());
            }
            delay = min_reconnect_delay;
        }
        tokio::select! {
            () = sleep(delay) => {}
            () = events.closed() => return Ok(()),
        }
        delay = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
    }
}

/// Why one connection attempt ended.
enum Attempt {
    /// The receiver was dropped: the task stops without error.
    Closed,
    /// The server refused the credentials: retrying cannot help.
    Unauthorized,
    /// Connect, handshake, protocol or decode failure: reconnect.
    Failed(AdapterError),
}

fn ws_error(error: tokio_tungstenite::tungstenite::Error) -> Attempt {
    Attempt::Failed(AdapterError::Stream(format!("websocket error: {error}")))
}

fn stream_failure(text: &str) -> Attempt {
    Attempt::Failed(AdapterError::Stream(text.to_owned()))
}

/// Waits for the next text or binary frame. Pings are answered and pongs skipped. A close
/// frame, end of stream or transport error is a failure. Returns `Closed` if the receiver is
/// dropped while waiting.
async fn next_data(
    ws: &mut Socket,
    events: &mpsc::Sender<StreamEvent>,
) -> Result<Message, Attempt> {
    loop {
        let frame = tokio::select! {
            frame = ws.next() => frame,
            () = events.closed() => return Err(Attempt::Closed),
        };
        match frame {
            Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => return Ok(message),
            Some(Ok(Message::Ping(payload))) => {
                ws.send(Message::Pong(payload)).await.map_err(ws_error)?;
            }
            Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Close(_))) => return Err(stream_failure("closed by server")),
            Some(Err(error)) => return Err(ws_error(error)),
            None => return Err(stream_failure("stream ended")),
        }
    }
}

/// The payload of a text or binary message, as bytes.
fn payload(message: &Message) -> &[u8] {
    match message {
        Message::Text(text) => text.as_bytes(),
        Message::Binary(bytes) => &bytes[..],
        _ => &[],
    }
}

/// Reads frames until the one that completes a handshake step, which is `expected`. Other
/// control-style messages are skipped. An order update before the stream is listening is a
/// failure rather than a skip, so no update is dropped without a reconcile.
async fn await_reply(
    ws: &mut Socket,
    events: &mpsc::Sender<StreamEvent>,
    expected: StreamMessage,
) -> Result<(), Attempt> {
    loop {
        let message = match timeout(HANDSHAKE_TIMEOUT, next_data(ws, events)).await {
            Ok(message) => message?,
            Err(_) => return Err(stream_failure("handshake timed out")),
        };
        match parse_stream_message(payload(&message)) {
            Ok(StreamMessage::Update(_)) => {
                return Err(Attempt::Failed(decode(
                    "order update before the stream was listening",
                )));
            }
            Ok(got) if got == expected => return Ok(()),
            Ok(_) => {}
            Err(AdapterError::Stream(text)) if text == UNAUTHORIZED => {
                return Err(Attempt::Unauthorized);
            }
            Err(error) => return Err(Attempt::Failed(error)),
        }
    }
}

/// One connection, from connect to the first failure. Never returns `Ok`: the read loop only
/// ends with an error. `connected` is set once `Connected` has been sent.
async fn run_connection(
    config: &AlpacaConfig,
    credentials: &Credentials,
    events: &mpsc::Sender<StreamEvent>,
    connected: &mut bool,
) -> Result<Infallible, Attempt> {
    let mut ws = tokio::select! {
        result = timeout(HANDSHAKE_TIMEOUT, connect_async(config.stream_url.as_str())) => {
            match result {
                Ok(Ok((ws, _response))) => ws,
                Ok(Err(error)) => return Err(ws_error(error)),
                Err(_) => return Err(stream_failure("connect timed out")),
            }
        }
        () = events.closed() => return Err(Attempt::Closed),
    };

    // The secret is escaped by `json!`; it is never formatted into an error message.
    let auth = json!({
        "action": "auth",
        "key": credentials.key_id(),
        "secret": credentials.secret_key(),
    });
    ws.send(Message::text(auth.to_string()))
        .await
        .map_err(ws_error)?;
    await_reply(&mut ws, events, StreamMessage::Authorized).await?;

    let listen = json!({"action": "listen", "data": {"streams": ["trade_updates"]}});
    ws.send(Message::text(listen.to_string()))
        .await
        .map_err(ws_error)?;
    await_reply(&mut ws, events, StreamMessage::Listening).await?;

    events
        .send(StreamEvent::Connected)
        .await
        .map_err(|_| Attempt::Closed)?;
    *connected = true;

    loop {
        let message = next_data(&mut ws, events).await?;
        match parse_stream_message(payload(&message)) {
            Ok(StreamMessage::Update(update)) => {
                events
                    .send(StreamEvent::Update(update))
                    .await
                    .map_err(|_| Attempt::Closed)?;
            }
            Ok(StreamMessage::Authorized | StreamMessage::Listening | StreamMessage::Ignored) => {}
            Err(error) => return Err(Attempt::Failed(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use nisse_core::OrderState;
    use serde_json::{Value, json};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use tokio::time::{sleep, timeout};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;
    use crate::Feed;

    /// The order object from the "Alpaca reference" section of the phase doc.
    const ORDER_JSON: &str = r#"{"asset_class": "us_equity", "asset_id": "b0b6dd9d-8b9b-48a9-ba46-b9d54906e415", "canceled_at": null, "client_order_id": "5680c4bc-9ac1-4a12-a44c-df427ba53032", "created_at": "2023-12-12T22:31:24.668464435Z", "expired_at": null, "extended_hours": false, "failed_at": null, "filled_at": null, "filled_avg_price": null, "filled_qty": "0", "hwm": null, "id": "7b08df51-c1ac-453c-99f9-323a5f075f0d", "legs": null, "limit_price": "150", "notional": null, "order_class": "", "order_type": "limit", "qty": "2", "replaced_at": null, "replaced_by": null, "replaces": null, "side": "buy", "source": null, "status": "accepted", "stop_price": null, "submitted_at": "2023-12-12T22:31:24.577215743Z", "subtag": null, "symbol": "AAPL", "time_in_force": "gtc", "trail_percent": null, "trail_price": null, "type": "limit", "updated_at": "2023-12-12T22:31:24.668464435Z"}"#;

    /// The fill example from the phase doc, with a filled SOLUSD order object.
    const FILL_JSON: &str = r#"{"stream":"trade_updates","data":{"event":"fill","execution_id":"2f63ea93-423d-4169-b3f6-3fdafc10c418","order":{"id":"7b08df51-c1ac-453c-99f9-323a5f075f0d","client_order_id":"5680c4bc-9ac1-4a12-a44c-df427ba53032","asset_class":"crypto","symbol":"SOLUSD","side":"buy","type":"market","order_type":"market","time_in_force":"gtc","qty":"1790.86","limit_price":null,"filled_qty":"1790.86","filled_avg_price":"105.8988475","status":"filled","updated_at":"2022-04-19T17:45:05.024916716Z"},"position_qty":"0","price":"105.8988475","qty":"1790.86","timestamp":"2022-04-19T17:45:05.024916716Z"}}"#;

    fn parse(text: &str) -> Result<StreamMessage, AdapterError> {
        parse_stream_message(text.as_bytes())
    }

    fn order_value(client_order_id: &str) -> Value {
        let mut order: Value = serde_json::from_str(ORDER_JSON).unwrap();
        order["client_order_id"] = json!(client_order_id);
        order
    }

    /// A `new` event for a distinct client order id.
    fn update_text(client_order_id: &str) -> String {
        json!({"stream": "trade_updates", "data": {"event": "new", "order": order_value(client_order_id)}})
            .to_string()
    }

    fn update_of(event: StreamEvent) -> OrderUpdate {
        match event {
            StreamEvent::Update(update) => update,
            other => panic!("expected an update, got {other:?}"),
        }
    }

    fn client_id_of(update: &OrderUpdate) -> String {
        update.order.client_order_id.to_string()
    }

    // ---- Parser tests ----

    #[test]
    fn fill_message_yields_update_with_fill_detail_and_ts() {
        let StreamMessage::Update(update) = parse(FILL_JSON).unwrap() else {
            panic!("fill must be an update");
        };
        let fill = update.fill.expect("fill event carries fill detail");
        assert_eq!(
            fill.trade_id.to_string(),
            "2f63ea93-423d-4169-b3f6-3fdafc10c418"
        );
        assert_eq!(fill.px.to_string(), "105.8988475");
        assert_eq!(fill.qty.to_string(), "1790.86");
        assert_eq!(update.ts.as_u64(), 1_650_390_305_024_916_716);
        assert_eq!(update.order.state, OrderState::Filled);
        assert_eq!(update.order.symbol.to_string(), "SOLUSD");
        assert_eq!(update.order.cum_filled_qty.to_string(), "1790.86");
    }

    #[test]
    fn new_event_without_timestamp_uses_order_updated_at() {
        let text = json!({"stream": "trade_updates", "data": {"event": "new", "order": order_value("nisse-1-0")}}).to_string();
        let StreamMessage::Update(update) = parse(&text).unwrap() else {
            panic!("new must be an update");
        };
        assert_eq!(update.fill, None);
        assert_eq!(update.ts.as_u64(), 1_702_420_284_668_464_435);
        assert_eq!(update.ts, update.order.ts);
        assert_eq!(update.order.state, OrderState::Submitted);
    }

    #[test]
    fn partial_fill_carries_fill_and_missing_execution_id_is_decode() {
        let with_id = FILL_JSON.replace("\"fill\"", "\"partial_fill\"");
        let StreamMessage::Update(update) = parse(&with_id).unwrap() else {
            panic!("partial_fill must be an update");
        };
        assert!(update.fill.is_some());

        let without_id = with_id.replace(
            "\"execution_id\":\"2f63ea93-423d-4169-b3f6-3fdafc10c418\",",
            "",
        );
        assert!(matches!(parse(&without_id), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn fill_events_require_price_and_qty() {
        let no_price = FILL_JSON.replace("\"price\":\"105.8988475\",", "");
        assert!(matches!(parse(&no_price), Err(AdapterError::Decode(_))));
        let no_qty = FILL_JSON.replace("\"qty\":\"1790.86\",", "");
        assert!(matches!(parse(&no_qty), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn execution_id_length_is_checked_before_trade_id() {
        let empty = FILL_JSON.replace("2f63ea93-423d-4169-b3f6-3fdafc10c418", "");
        assert!(matches!(parse(&empty), Err(AdapterError::Decode(_))));

        let too_long = FILL_JSON.replace("2f63ea93-423d-4169-b3f6-3fdafc10c418", &"x".repeat(37));
        assert!(matches!(parse(&too_long), Err(AdapterError::Decode(_))));

        let max_len = FILL_JSON.replace("2f63ea93-423d-4169-b3f6-3fdafc10c418", &"x".repeat(36));
        let StreamMessage::Update(update) = parse(&max_len).unwrap() else {
            panic!("36 characters is allowed");
        };
        assert_eq!(update.fill.unwrap().trade_id.to_string(), "x".repeat(36));
    }

    #[test]
    fn canceled_event_has_no_fill_detail() {
        let text = json!({"stream": "trade_updates", "data": {"event": "canceled", "order": order_value("nisse-1-0"), "timestamp": "2022-04-19T17:45:05.024916716Z"}}).to_string();
        let StreamMessage::Update(update) = parse(&text).unwrap() else {
            panic!("canceled must be an update");
        };
        assert_eq!(update.fill, None);
        assert_eq!(update.ts.as_u64(), 1_650_390_305_024_916_716);
    }

    #[test]
    fn trade_update_without_order_is_decode() {
        let text = r#"{"stream":"trade_updates","data":{"event":"new"}}"#;
        assert!(matches!(parse(text), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn unknown_order_status_in_update_is_decode() {
        let mut order = order_value("nisse-1-0");
        order["status"] = json!("bogus");
        let text = json!({"stream": "trade_updates", "data": {"event": "new", "order": order}})
            .to_string();
        assert!(matches!(parse(&text), Err(AdapterError::Decode(_))));
    }

    #[test]
    fn update_for_an_order_kind_we_never_send_is_ignored() {
        let mut order = order_value("placed-by-hand");
        order["type"] = json!("stop");
        let text = json!({"stream": "trade_updates", "data": {"event": "new", "order": order}})
            .to_string();
        assert_eq!(parse(&text).unwrap(), StreamMessage::Ignored);
    }

    #[test]
    fn authorized_and_listening_messages() {
        assert_eq!(
            parse(r#"{"stream":"authorization","data":{"status":"authorized","action":"authenticate"}}"#).unwrap(),
            StreamMessage::Authorized
        );
        assert_eq!(
            parse(r#"{"stream":"listening","data":{"streams":["trade_updates"]}}"#).unwrap(),
            StreamMessage::Listening
        );
    }

    #[test]
    fn unauthorized_is_stream_error() {
        let text = r#"{"stream":"authorization","data":{"status":"unauthorized","action":"authenticate"}}"#;
        assert!(matches!(parse(text), Err(AdapterError::Stream(_))));
    }

    #[test]
    fn error_action_is_stream_error_with_server_message() {
        let text = r#"{"action":"error","data":{"error_message":"internal server error"}}"#;
        match parse(text) {
            Err(AdapterError::Stream(message)) => assert_eq!(message, "internal server error"),
            other => panic!("expected a stream error, got {other:?}"),
        }
        assert!(matches!(
            parse(r#"{"action":"error","data":{}}"#),
            Err(AdapterError::Decode(_))
        ));
    }

    #[test]
    fn order_rejections_are_ignored() {
        for event in ["order_cancel_rejected", "order_replace_rejected"] {
            let text = json!({"stream": "trade_updates", "data": {"event": event, "order": order_value("nisse-1-0")}}).to_string();
            assert_eq!(parse(&text).unwrap(), StreamMessage::Ignored, "{event}");
        }
    }

    #[test]
    fn garbage_and_unknown_shapes_are_decode() {
        for text in [
            "not json",
            "",
            "{}",
            r#"{"stream":"quotes","data":{}}"#,
            r#"{"action":"auth","data":{}}"#,
            r#"{"stream":"authorization","data":{"status":"maybe"}}"#,
            r#"{"stream":"trade_updates"}"#,
        ] {
            assert!(
                matches!(parse(text), Err(AdapterError::Decode(_))),
                "{text} must be a decode error"
            );
        }
    }

    // ---- Local websocket server ----

    /// What the server does with one accepted connection, after the handshake (or instead of it).
    enum Scripted {
        /// Authorize, listen, send these frames, then keep the connection open.
        Serve(Vec<WsMessage>),
        /// Authorize, listen, send these frames, then close the connection.
        ServeThenClose(Vec<WsMessage>),
        /// Reply `unauthorized` to the auth message.
        Reject,
    }

    async fn read_json(ws: &mut WebSocketStream<TcpStream>) -> Value {
        let message = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("server: timed out reading")
            .expect("server: stream ended")
            .expect("server: read error");
        match message {
            WsMessage::Text(text) => serde_json::from_str(&text).expect("server: bad json"),
            other => panic!("server expected a text frame, got {other:?}"),
        }
    }

    async fn serve(
        tcp: TcpStream,
        script: Scripted,
        reports: mpsc::UnboundedSender<(Value, Value)>,
    ) {
        let Ok(mut ws) = accept_async(tcp).await else {
            return;
        };
        let auth = read_json(&mut ws).await;
        if matches!(script, Scripted::Reject) {
            let _ = reports.send((auth, Value::Null));
            let _ = ws
                .send(WsMessage::text(
                    json!({"stream": "authorization", "data": {"status": "unauthorized", "action": "authenticate"}}).to_string(),
                ))
                .await;
            return;
        }
        let _ = ws
            .send(WsMessage::text(
                json!({"stream": "authorization", "data": {"status": "authorized", "action": "authenticate"}}).to_string(),
            ))
            .await;
        let listen = read_json(&mut ws).await;
        let _ = reports.send((auth, listen));
        let _ = ws
            .send(WsMessage::text(
                json!({"stream": "listening", "data": {"streams": ["trade_updates"]}}).to_string(),
            ))
            .await;
        match script {
            Scripted::Serve(frames) => {
                for frame in frames {
                    let _ = ws.send(frame).await;
                }
                // Hold the connection until the test runtime shuts down.
                std::future::pending::<()>().await;
            }
            Scripted::ServeThenClose(frames) => {
                for frame in frames {
                    let _ = ws.send(frame).await;
                }
                let _ = ws.close(None).await;
            }
            Scripted::Reject => {}
        }
    }

    /// Starts a local server with one entry per accepted connection, in order. Returns its
    /// `ws://` URL and a channel of `(auth, listen)` messages, one pair per handshake.
    async fn spawn_server(
        scripts: Vec<Scripted>,
    ) -> (String, mpsc::UnboundedReceiver<(Value, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (reports, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for script in scripts {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(tcp, script, reports.clone()));
            }
        });
        (format!("ws://{addr}"), received)
    }

    fn test_config(url: &str) -> AlpacaConfig {
        AlpacaConfig {
            trading_base_url: "https://placeholder.invalid".to_owned(),
            data_base_url: "https://placeholder.invalid".to_owned(),
            stream_url: url.to_owned(),
            feed: Feed::Iex,
        }
    }

    /// Spawns `run_order_stream` against `url` with placeholder credentials.
    fn start(
        url: &str,
        capacity: usize,
        delay: Duration,
    ) -> (
        mpsc::Receiver<StreamEvent>,
        JoinHandle<Result<(), AdapterError>>,
    ) {
        let config = test_config(url);
        let credentials = Credentials::new("test-key-id", "test-secret");
        let (tx, rx) = mpsc::channel(capacity);
        let handle =
            tokio::spawn(async move { run_order_stream(&config, &credentials, tx, delay).await });
        (rx, handle)
    }

    async fn next_event(rx: &mut mpsc::Receiver<StreamEvent>) -> StreamEvent {
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a stream event")
            .expect("stream ended")
    }

    async fn next_handshake(
        reports: &mut mpsc::UnboundedReceiver<(Value, Value)>,
    ) -> (Value, Value) {
        timeout(Duration::from_secs(5), reports.recv())
            .await
            .expect("timed out waiting for a handshake")
            .expect("server gone")
    }

    fn assert_credentials_and_listen(auth: &Value, listen: &Value) {
        assert_eq!(auth["action"], "auth");
        assert_eq!(auth["key"], "test-key-id");
        assert_eq!(auth["secret"], "test-secret");
        assert_eq!(
            *listen,
            json!({"action": "listen", "data": {"streams": ["trade_updates"]}})
        );
    }

    // ---- Connection tests ----

    #[tokio::test]
    async fn text_and_binary_updates_arrive_in_order_after_connected() {
        let frames = vec![
            WsMessage::text(update_text("order-1")),
            WsMessage::binary(update_text("order-2").into_bytes()),
        ];
        let (url, mut reports) = spawn_server(vec![Scripted::Serve(frames)]).await;
        let (mut rx, _task) = start(&url, 8, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-1"
        );
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-2"
        );

        let (auth, listen) = next_handshake(&mut reports).await;
        assert_credentials_and_listen(&auth, &listen);
    }

    #[tokio::test]
    async fn reconnects_after_server_drop_and_reports_disconnect() {
        let (url, _reports) = spawn_server(vec![
            Scripted::ServeThenClose(vec![WsMessage::text(update_text("order-1"))]),
            Scripted::Serve(vec![WsMessage::text(update_text("order-2"))]),
        ])
        .await;
        let (mut rx, _task) = start(&url, 8, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-1"
        );
        assert_eq!(next_event(&mut rx).await, StreamEvent::Disconnected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-2"
        );
    }

    #[tokio::test]
    async fn unauthorized_returns_stream_error_without_reconnecting() {
        let (url, _reports) = spawn_server(vec![Scripted::Reject]).await;
        let config = test_config(&url);
        let credentials = Credentials::new("test-key-id", "test-secret");
        let (tx, _rx) = mpsc::channel(1);
        let result = timeout(
            Duration::from_secs(5),
            run_order_stream(&config, &credentials, tx, Duration::from_millis(10)),
        )
        .await
        .expect("must return without reconnecting");
        assert!(matches!(result, Err(AdapterError::Stream(_))), "{result:?}");
    }

    #[tokio::test]
    async fn back_pressure_delivers_every_update_in_order() {
        const COUNT: usize = 20;
        let frames = (0..COUNT)
            .map(|i| WsMessage::text(update_text(&format!("order-{i}"))))
            .collect();
        let (url, _reports) = spawn_server(vec![Scripted::Serve(frames)]).await;
        let (mut rx, _task) = start(&url, 1, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        for i in 0..COUNT {
            if i < 5 {
                sleep(Duration::from_millis(50)).await;
            }
            let update = update_of(next_event(&mut rx).await);
            assert_eq!(client_id_of(&update), format!("order-{i}"));
        }
    }

    #[tokio::test]
    async fn dropping_receiver_stops_task_with_ok() {
        let (url, _reports) = spawn_server(vec![Scripted::Serve(vec![WsMessage::text(
            update_text("order-1"),
        )])])
        .await;
        let (mut rx, task) = start(&url, 4, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-1"
        );
        drop(rx);

        let result = timeout(Duration::from_secs(5), task)
            .await
            .expect("task must stop after the receiver is dropped")
            .expect("task must not panic");
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn dropping_receiver_while_reconnect_is_pending_stops_task() {
        let (url, _reports) = spawn_server(vec![Scripted::ServeThenClose(vec![])]).await;
        // A delay far longer than the test: only the closed receiver can end the sleep.
        let (mut rx, task) = start(&url, 4, Duration::from_secs(3600));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Disconnected);
        drop(rx);

        let result = timeout(Duration::from_secs(5), task)
            .await
            .expect("task must stop while sleeping")
            .expect("task must not panic");
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn undecodable_frame_after_connected_reconnects() {
        let (url, _reports) = spawn_server(vec![
            Scripted::Serve(vec![WsMessage::text("{not valid json")]),
            Scripted::Serve(vec![WsMessage::text(update_text("order-2"))]),
        ])
        .await;
        let (mut rx, _task) = start(&url, 8, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Disconnected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-2"
        );
    }

    #[tokio::test]
    async fn error_action_after_connected_reconnects() {
        let error_frame = WsMessage::text(
            json!({"action": "error", "data": {"error_message": "internal server error"}})
                .to_string(),
        );
        let (url, _reports) = spawn_server(vec![
            Scripted::ServeThenClose(vec![error_frame]),
            Scripted::Serve(vec![WsMessage::text(update_text("order-3"))]),
        ])
        .await;
        let (mut rx, _task) = start(&url, 8, Duration::from_millis(10));

        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Disconnected);
        assert_eq!(next_event(&mut rx).await, StreamEvent::Connected);
        assert_eq!(
            client_id_of(&update_of(next_event(&mut rx).await)),
            "order-3"
        );
    }
}
