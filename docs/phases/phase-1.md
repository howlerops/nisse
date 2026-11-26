# Phase 1: Alpaca adapter

Issue: #3. Exit criterion: order round-trip and restart reconciliation pass against paper.

The exit criterion needs Alpaca paper credentials, which live only on the engine host and are
never given to an agent. This phase therefore ships the adapter, tested against a local mock
server, plus paper tests that are `#[ignore]`d and run by the owner (see T9).

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Where the logic lives | `crates/alpaca-adapter` only speaks Alpaca. Order tracking, reconciliation and the write-ahead submit path are broker-agnostic and live in `crates/engine`, reaching the adapter through `nisse_core::Broker`. | Phase 4 fault injection and backtests need the same logic with a fake broker. |
| Nautilus integration | Use Nautilus model types (`Price`, `Quantity`, `Bar`, `QuoteTick`, ids, order enums). Do not implement Nautilus `ExecutionClient` / `DataClient` yet. | Those traits belong with the engine wiring (Phase 2/3); the pinned Rust API for them is unstable. |
| HTTP / websocket | `reqwest` (rustls) and `tokio-tungstenite`, `wiremock` for tests. | Mainstream, small surface. |
| Decimals | Read from the JSON text into Nautilus fixed-point; never through `f64`. Signed quantities (positions) use `rust_decimal::Decimal`, already in the Nautilus tree. | Exact money. |
| Retries | Idempotent GETs retry 3 times; anything that changes state is sent once. | A resent order could execute twice. |
| Feed | `Feed::{Iex, Sip}` is configuration. | The IEX/SIP question (#12) stays open and does not block this phase. |
| Quote stream | Not built. Latest quotes come from REST. | No planned strategy is intraday; arrival mid at decision time is one REST call. |
| Order stream | Built, but treated as a latency optimisation: every (re)connect triggers a REST reconciliation. | Stream messages can be lost. |
| Fills | Derived from the change in the broker's cumulative filled quantity. | Duplicates, replays and out-of-order messages cannot double-count; missed messages are made up by the next one. |
| Order columns | Migration 0006 adds `symbol`, `side`, `time_in_force`, `cum_filled_qty` to `order_event` and fixes the `state` and `type` value lists. | The intent row must be enough on its own to recover after a crash. |

## Hot path

Phase 1 has no per-bar or per-quote path. The per-order path is `OrderGateway::submit`,
`OrderTracker::apply` and `parse_stream_message`. Orders are a handful per month, so there
is no criterion bench in this phase; the rule that still applies is **no `f64` for money and
no lock held across `.await`**. `OrderTracker::apply` must not allocate except when it has to
synthesise a trade id (recovery path only).

## Alpaca reference

Auth: headers `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY` (handled by `AlpacaHttp`).

### Order object (REST responses and stream `order` field)

```json
{"asset_class": "us_equity", "asset_id": "b0b6dd9d-8b9b-48a9-ba46-b9d54906e415", "canceled_at": null, "client_order_id": "5680c4bc-9ac1-4a12-a44c-df427ba53032", "created_at": "2023-12-12T22:31:24.668464435Z", "expired_at": null, "extended_hours": false, "failed_at": null, "filled_at": null, "filled_avg_price": null, "filled_qty": "0", "hwm": null, "id": "7b08df51-c1ac-453c-99f9-323a5f075f0d", "legs": null, "limit_price": "150", "notional": null, "order_class": "", "order_type": "limit", "qty": "2", "replaced_at": null, "replaced_by": null, "replaces": null, "side": "buy", "source": null, "status": "accepted", "stop_price": null, "submitted_at": "2023-12-12T22:31:24.577215743Z", "subtag": null, "symbol": "AAPL", "time_in_force": "gtc", "trail_percent": null, "trail_price": null, "type": "limit", "updated_at": "2023-12-12T22:31:24.668464435Z"}
```

Mapping to `BrokerOrder`: `id` → `venue_order_id`; `filled_qty` → `cum_filled_qty`;
`filled_avg_price` → `avg_fill_px` (`None` when `null`); `updated_at` → `ts`;
`side` `buy`/`sell` → `OrderSide::Buy`/`Sell`; `type` `market`/`limit` →
`OrderType::Market`/`Limit` (others `Unsupported`); `time_in_force` `day`/`gtc` →
`TimeInForce::Day`/`Gtc` (others `Unsupported`); `qty: null` → `Unsupported`.

### Status → `OrderState`

| Alpaca `status` | `OrderState` |
|---|---|
| `pending_new`, `accepted`, `accepted_for_bidding`, `pending_review`, `held` | `Submitted` |
| `new`, `done_for_day`, `stopped`, `suspended`, `calculated` | `Accepted` |
| `partially_filled` | `PartiallyFilled` |
| `pending_cancel`, `pending_replace` | `PendingCancel` |
| `filled` | `Filled` |
| `canceled` | `Canceled` |
| `replaced` | `Replaced` |
| `rejected` | `Rejected` |
| `expired` | `Expired` |
| anything else | `AdapterError::Decode` |

`calculated` and `done_for_day` are deliberately non-terminal: the cumulative quantity, not
the label, decides what was filled.

### Requests

- `POST /v2/orders` body: `{"symbol":"SPY","qty":"1.5","side":"buy","type":"limit","time_in_force":"day","limit_price":"500.10","client_order_id":"nisse-1-0"}`. `side` is `buy`/`sell`, `type` is `market`/`limit`, `time_in_force` is `day`/`gtc`; `limit_price` is omitted for market orders. Decimals are strings: `qty.to_string()` and `limit_px.to_string()`.
- `PATCH /v2/orders/{id}` body: `{"qty":"2","limit_price":"499.00","client_order_id":"nisse-1-1"}` (`qty` and `limit_price` omitted when `None`). Returns the new order object.
- `DELETE /v2/orders/{id}` → 204. `DELETE /v2/orders` → 207 with a list (ignored).
- `GET /v2/orders:by_client_order_id?client_order_id=...` → order object, or 404.
- `GET /v2/orders?status=open&limit=500` → array of order objects.
- `GET /v2/positions` → array: `{"symbol":"AAPL","qty":"5","side":"long","avg_entry_price":"100.0", ...}` (`side` is `long`/`short`; a short's `qty` is written with a leading `-`).
- `GET /v2/account` → `{"equity":"103820.56","cash":"-23140.2","buying_power":"262113.632","currency":"USD","trading_blocked":false,"account_blocked":false,"trade_suspended_by_user":false, ...}`.
- Errors: non-2xx with optional body `{"code": 42210000, "message": "..."}`. 403 = insufficient buying power or shares, 422 = bad parameters, 429 = rate limit.
- `GET /v2/calendar?start=YYYY-MM-DD&end=YYYY-MM-DD` → `[{"date":"2025-01-02","open":"09:30","close":"16:00","session_open":"0400","session_close":"2000","settlement_date":"2025-01-03"}]`. `open`/`close` are America/New_York local.
- `GET {data}/v2/stocks/bars?symbols=AAPL,TSLA&timeframe=1Day&start=YYYY-MM-DD&end=YYYY-MM-DD&adjustment=all&feed=iex&limit=10000&sort=asc[&page_token=...]` → `{"bars":{"AAPL":[{"c":178.21,"h":178.26,"l":178.21,"n":65,"o":178.26,"t":"2022-01-03T09:00:00Z","v":1118,"vw":178.235733}]},"next_page_token":"QUFQ..."}`. Prices are JSON numbers; `next_page_token` is `null` on the last page; `bars` may omit a symbol with no data.
- `GET {data}/v2/stocks/quotes/latest?symbols=AAPL,TSLA&feed=iex` → `{"quotes":{"AAPL":{"ap":172.7,"as":1,"ax":"Q","bp":172.62,"bs":2,"bx":"Q","c":["R"],"t":"2022-08-17T10:07:40.286587431Z","z":"C"}}}`. `ap`/`bp` of 0 mean no quote on that side.

### Order stream

Connect to `config.stream_url`. Paper sends JSON in **binary** frames, live in text frames;
treat both as bytes. Reply to ping with pong.

1. Send `{"action":"auth","key":"<key id>","secret":"<secret>"}`.
   Expect `{"stream":"authorization","data":{"status":"authorized","action":"authenticate"}}`.
   `"status":"unauthorized"` is a fatal `AdapterError::Stream`.
2. Send `{"action":"listen","data":{"streams":["trade_updates"]}}`.
   Expect `{"stream":"listening","data":{"streams":["trade_updates"]}}`.
3. Updates: `{"stream":"trade_updates","data":{"event":"fill","execution_id":"2f63ea93-423d-4169-b3f6-3fdafc10c418","order":{...order object...},"position_qty":"0","price":"105.8988475","qty":"1790.86","timestamp":"2022-04-19T17:45:05.024916716Z"}}`.
4. Error: `{"action":"error","data":{"error_message":"internal server error"}}`, then the server closes.

`event` handling: every event that carries an `order` becomes
`StreamMessage::Update(OrderUpdate { order, fill, ts })`. `fill` is `Some` only for `fill` and
`partial_fill` (from `execution_id`, `price`, `qty`; all three required for those events).
`ts` is `timestamp` when present, else the order's `updated_at`. `order_replace_rejected` and
`order_cancel_rejected` are `StreamMessage::Ignored` (the order itself did not change).

## Order tracker rules (`OrderTracker::apply`)

Let `delta = order.cum_filled_qty − tracked.cum_filled_qty`, compared as decimals.

1. `delta < 0`: the message is older than what we know. Change nothing; return
   `Applied::default()`.
2. `delta > 0`: there is a new fill.
   - Set `cum_filled_qty`, `avg_fill_px`, `venue_order_id`, `last_update_ts = ts` from the
     message and **adopt the broker's state**, even if the tracked state was terminal (a fill
     after a cancel is real).
   - Emit `Fill { client_order_id, trade_id, px, qty: delta, ts }`:
     - If `fill` is `Some` and `fill.qty == delta`: `px = fill.px`, `trade_id = fill.trade_id`.
     - Otherwise (a message was missed, or this is a REST snapshot): `px` is the average price
       of the missing quantity, `(avg_new·cum_new − avg_old·cum_old) / delta`, rounded
       half-even to 9 decimal places (`avg_old·cum_old` is 0 when nothing was filled before);
       `trade_id` is synthetic and deterministic: the first 18 characters of the venue order
       id with `-` removed, then `-`, then the raw fixed-point value of `cum_new` in lower
       hex (at most 36 characters). If `order.avg_fill_px` is `None` here, return
       `InconsistentBrokerData`.
3. `delta == 0`: no fill. If the tracked state is terminal, or `order.state.rank()` is lower
   than the tracked state's rank, change nothing. Otherwise adopt the state, set
   `venue_order_id` and `last_update_ts`.
4. `state_change` is `Some((before, after))` only when the state actually changed.

## Reconciliation rules (`reconcile`)

- For each broker open order: tracked and (`state` or `cum_filled_qty` differs) → `OrderDrift`;
  not tracked → `UnknownAtBroker`; tracked and equal → nothing.
- For each tracked order from `tracker.open_orders()` (this includes orders still in
  `Intent`) whose client order id is not among the broker's open orders → `NotOpenAtBroker`.
- Positions: signed broker quantity is `qty` for `Long`, `−qty` for `Short`, 0 for `Flat`.
  For every symbol on either side whose signed quantities differ → `PositionMismatch`.
- Order: order findings sorted by client order id (as strings), then position findings
  sorted by symbol.

## Tasks

Every task: no new dependencies, `unwrap`/`expect`/`panic` only in tests, tests use
placeholder credentials (`Credentials::new("test-key-id", "test-secret")`) against
`wiremock` or a local socket, never a real endpoint. Remove the task's
`#[allow(unused_variables, reason = "stub: ...")]` attributes when implementing.

| # | Task | Files | Round |
|---|---|---|---|
| F | Deps, core order/clock/calendar types and traits, adapter `error`/`config`/`parse`/`http`, all stubs | (foundation) | lead |
| T0 | Wire mapping | `crates/alpaca-adapter/src/wire.rs` | 1 |
| T2 | Market data client | `crates/alpaca-adapter/src/data.rs` | 1 |
| T4 | Calendar | `crates/core/src/calendar.rs`, `crates/alpaca-adapter/src/calendar.rs` | 1 |
| T5 | Order tracker | `crates/engine/src/order_tracker.rs` | 1 |
| T8 | Migration 0006 | `migrations/0006_order_columns.sql`, `migrations/tests/0007_order_columns.sql`, `migrations/tests/0004_orders.sql` | 1 |
| T1 | Trading client | `crates/alpaca-adapter/src/trading.rs` | 2 (needs T0) |
| T3 | Order stream | `crates/alpaca-adapter/src/stream.rs` | 2 (needs T0) |
| T6 | Reconcile | `crates/engine/src/reconcile.rs` | 2 (needs T5) |
| T7 | Order gateway | `crates/engine/src/gateway.rs`, `crates/engine/tests/support/mod.rs` | 2 (needs T5) |
| T10 | Broker sync | `crates/engine/src/sync.rs` | 3 (needs T5, T6, T7's test support) |
| T9 | Paper tests | `crates/alpaca-adapter/tests/paper.rs`, `docs/runbooks/paper-tests.md` | 3 |

### Acceptance tests

**T0 wire.** Unit tests in `wire.rs`:
- the order JSON above → `BrokerOrder` with every field asserted (state `Submitted`, qty 2,
  limit 150, cum 0, `avg_fill_px` `None`, `ts` 1702420284668464435);
- a filled market order (`filled_qty` "1790.86", `filled_avg_price` "105.8988475") →
  `Filled`, cum and avg asserted as strings;
- every row of the status table (one assertion per status), and `"bogus"` → `Decode`;
- `type` `stop` → `Unsupported`; `time_in_force` `ioc` → `Unsupported`; `qty` null →
  `Unsupported`; `side` `hold` → `Decode`; bad `updated_at` → `Decode`;
- position long `"5"` → `Long`, 5; short `"-3"` → `Short`, 3; side `flat`-ish garbage → `Decode`;
- account JSON above → equity `103820.56 USD`, cash `-23140.20 USD`, not blocked; each of
  the three blocking flags alone makes `trading_blocked` true;
- `OrderRequest::from_intent` for a limit and a market intent serialises to exactly the JSON
  in "Requests" (compare `serde_json::Value`s); limit intent without price, market intent
  with a price, and time in force `Ioc` → `Unsupported`;
- `ReplaceBody` omits `None` fields.

**T2 data.** `wiremock` tests: bars request carries `symbols`, `timeframe=1Day`, `start`,
`end`, `adjustment=all`, `feed=iex`, `limit=10000`, `sort=asc`; two pages are followed via
`page_token` and concatenated; result sorted by symbol then time; prices exact (`178.21`
stays `178.21`, test with `0.1` too); `bar_type` string is `AAPL.ALPACA-1-DAY-LAST-EXTERNAL`;
`ts_init == ts_event`; empty `symbols` → `Ok(vec![])` with no request; a symbol missing from
`bars` is simply absent; quotes map `bp`/`ap`/`bs`/`as`/`t`, drop a quote with `bp` or `ap`
of 0, are sorted by symbol; a 403 surfaces as `Status { status: 403, .. }`.

**T4 calendar.** `MarketCalendar` unit tests: `new` rejects empty, `open >= close`,
unsorted, duplicate dates, overlapping sessions; `session_on` hit and miss (weekend);
`session_at` at `open` (in), `close − 1` (in), `close` (out), between sessions (out), before
first and after last (out); `is_open` false outside coverage; `next_session` from before
the first, mid-session (returns the following one), and after the last (`None`);
`is_last_session_of_month` true/false/`None` for a non-session and for the final session;
`is_half_day` for a 13:00 close. Adapter tests: a winter day `09:30`–`16:00` ET is
14:30–21:00 UTC and a summer day is 13:30–20:00 UTC (DST); the day after Thanksgiving with
`close` `13:00` is a half-day; malformed date or time → `Decode`; `fetch_calendar` sends
`start` and `end` and builds a calendar (wiremock).

**T5 order tracker.** Unit tests, one per rule and edge: duplicate intent; unknown order;
accept then fill with matching `FillDetail`; two partial fills then final fill, sum of emitted
fill quantities equals the order quantity; the same fill message applied twice emits one
fill; an older snapshot (`delta < 0`) changes nothing; a missed partial fill is recovered
from the next message with the synthetic price (e.g. cum 0→3 at avg 10 already known, then
snapshot cum 5 avg 10.4 → fill of 2 at 11) and a synthetic trade id that is ≤ 36 chars and
identical when recomputed; `avg_fill_px` `None` with `delta > 0` and no detail →
`InconsistentBrokerData`; late `Accepted` after `Canceled` is ignored; fill after `Canceled`
is recorded and state follows the broker; `PendingCancel` then `PartiallyFilled` with a
fill keeps the fill; `mark_never_arrived` sets `Rejected`, is a no-op when terminal;
`open_orders` excludes terminal orders and iterates in id order; restart: a fresh tracker
given the intent and a snapshot with cum 2 emits one fill of 2.

**T8 migration.** `0006_order_columns.sql` on `order_event`: add
`symbol TEXT NOT NULL CHECK (symbol <> '')`,
`side TEXT NOT NULL CHECK (side IN ('BUY', 'SELL'))`,
`time_in_force TEXT NOT NULL CHECK (time_in_force IN ('DAY', 'GTC'))`,
`cum_filled_qty finite_numeric NOT NULL CHECK (cum_filled_qty >= 0)`; add
`CHECK (state IN ('intent', 'submitted', 'accepted', 'partially_filled', 'pending_cancel', 'filled', 'canceled', 'replaced', 'rejected', 'expired'))`,
`CHECK (type IN ('MARKET', 'LIMIT'))`, `CHECK (cum_filled_qty <= qty)`,
`CHECK ((type = 'LIMIT') = (limit_px IS NOT NULL))`. The table is empty everywhere, so plain
`ALTER TABLE ... ADD COLUMN ... NOT NULL` works. Update the `order_event` inserts in
`migrations/tests/0004_orders.sql` for the new columns and value lists. New test file covers
each new constraint (accept and reject) and that an intent row has `cum_filled_qty = 0`.
`scripts/test-migrations.sh` passes.

**T1 trading.** `wiremock` tests per method: submit sends the exact body and returns the
mapped order; 422 → `Rejected`; 403 → `Rejected`; 500 → `Unknown`; 429 → `Unknown`;
connection refused → `Unknown`; a 200 with an undecodable body → `Unknown`; the POST is
sent exactly once in every case (`expect(1)`); an unsupported intent → `Rejected` without any
request; cancel hits `DELETE /v2/orders/{id}`; cancel_all hits `DELETE /v2/orders`; replace
PATCHes with the body and maps the result, 422 → `Rejected`; order_by_client_id sends the
query and returns `Some`, 404 → `None`, 500 → `Err`; open_orders sends `status=open` and
`limit=500`; positions and account map.

**T3 stream.** Parser tests: the fill message above → `Update` with fill detail and `ts`;
a `new` event without `timestamp` uses `updated_at`; `partial_fill` missing `execution_id` →
`Decode`; authorized, listening, unauthorized (`Stream`), error action (`Stream`),
`order_cancel_rejected` (`Ignored`), garbage (`Decode`). Connection tests against a local
`tokio-tungstenite` server on `127.0.0.1:0` (use a `ws://` URL): auth then listen are sent
with the credentials; `Connected` then updates arrive in order, from both text and binary
frames; when the server drops the connection the task sends `Disconnected`, reconnects and
sends `Connected` again; `unauthorized` makes `run_order_stream` return `Err`;
back-pressure: with a channel of capacity 1 and a slow consumer, all N updates still arrive,
in order; dropping the receiver makes the task return `Ok(())`.

**T6 reconcile.** Unit tests: agreement → empty; drift by state; drift by cum; unknown at
broker; tracked `Intent` not at broker → `NotOpenAtBroker`; terminal tracked orders are not
reported; position mismatch long, short (negative), broker-only symbol, local-only symbol,
equal → none, `Flat` equals absent; ordering of a mixed result.

**T7 gateway.** `crates/engine/tests/support/mod.rs` provides `FakeBroker` (scripted
responses per method, records every call in order) and `MemoryIntentLog` (records intents,
can be set to fail), sharing one call log so tests can assert ordering. Tests (in
`gateway.rs` or `crates/engine/tests/gateway.rs`): the intent is recorded **before**
`submit` is called (assert the shared log order); intent log failure → `IntentNotRecorded`,
broker never called, tracker shows `Rejected`; duplicate client order id → `DuplicateOrder`,
nothing recorded or sent; `Rejected` → `OrderRejected`, tracker `Rejected`; `Unknown` →
`OrderOutcomeUnknown`, tracker still `Intent` and listed by `open_orders`; success → tracker
has the venue order id and broker state; submit is called exactly once per attempt; replace
records the replacement intent first, calls `broker.replace` with the original's venue id,
errors with `UnknownOrder` for an untracked or not-yet-acknowledged original.

**T10 sync.** Using T7's `FakeBroker`: clean state → empty report, no order lookups; restart
mid-order (tracker has only the intent; broker has it open, half filled) → drift applied, one
fill reported; order filled while disconnected (not in open orders, found by id) → fill
reported, state `Filled`; submit that never arrived (lookup `None`) → `Rejected` at `now`;
unknown broker order reported and untouched; positions overwritten, broker-flat removes the
entry; a broker error → `Broker(..)`.

**T9 paper tests.** `crates/alpaca-adapter/tests/paper.rs`, every test
`#[ignore = "needs Alpaca paper credentials on the engine host"]`, reading credentials with
`Credentials::from_env()` and using `AlpacaConfig::paper(Feed::Iex)` only: (1) round trip: a
day limit buy of 1 SPY far below the market is accepted, found by client order id, cancelled,
and ends `Canceled` in the tracker via `sync_with_broker`; (2) restart: submit, drop the
tracker, rebuild it from the intent, `sync_with_broker` adopts the open order, then cancel;
(3) fractional limit: a day limit buy of 0.5 SPY far below the market, test passes either
way and prints whether Alpaca accepted it (answers the open question); (4) calendar fetch
for the current year has a half-day. `docs/runbooks/paper-tests.md` says how the owner runs
them (`cargo test -p nisse-alpaca-adapter --test paper -- --ignored --test-threads=1`) and
that orders are placed far from the market and cancelled.

## Not done by this phase's PR

- Running the paper tests (needs the owner's credentials): the two "Integration test" boxes
  and the fractional-limit box in #3 stay open until the owner runs T9.
- Postgres `IntentLog` implementation: Phase 2, with the store writer.
