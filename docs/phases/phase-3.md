# Phase 3: First strategy and backtest

Issue: #5. Exit criterion: backtest runs through the same engine as live; every run registers
a trial.

No real backtest is run in this phase: historical bars need the owner's Alpaca credentials
and the market data feed decision (#12). The code is tested on synthetic bars, which PLAN.md
allows for engine correctness only. Nothing here is evidence about the strategy.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Strategy | Time-series trend with volatility targeting (`trend_vol_target`) | The simpler of the two options in PLAN.md. Rationale, universe and parameters are in `docs/research/trend-v1-rationale.md`, written before any result. |
| Same engine | `Trader` is generic over `Broker`, `Clock` and the store traits. Backtest uses `SimBroker`, `ManualClock`, `MemoryStore`; live uses the Alpaca adapter, the system clock and Postgres. | The exit criterion. |
| Backtest decisions | Kept in memory, not written to `decision` | They are not real decisions. Only the trial is stored. |
| Trial results | New table `trial_result`, keyed by `trial_registry.id` | A trial is registered before it runs (so a crash still counts), and rows cannot be updated. Recommended in Phase 0; taken here because nothing else keeps the count honest. The owner can veto it in review. |
| Bars | Parquet files written and read through DuckDB (`crates/bars`), `DECIMAL(28, 9)` prices | PLAN.md. DuckDB is compiled from the source vendored in the crate: no download at build time, at the cost of a long first build. |
| Fill model | Orders decided at a session's close fill at the next session's open, with slippage and fees from `CostModel` | No look-ahead: the close that triggers a decision cannot also be the fill price. |
| Features | Exact `Decimal`: momentum, daily variance, close, position, equity | Stored and hashed without floats. Square root and the weight are computed in `f64` inside `decide`, which is deterministic for a given build. |
| Benchmarks | `criterion` benches for the per-bar path and `decide` | First phase with a per-bar path. |

## Hot path

- Per bar: `RollingStats::push` and `Trader::on_bar`. O(1), no allocation, exact decimals.
  Bench: `crates/engine/benches/hot_path.rs`, `rolling_push` and `trader_on_bar`.
- Per decision: `TrendVolTarget::decide`. Bench: `trend_decide`.

There is no `main` baseline yet for these; this phase's numbers become the baseline.

## Trend strategy rules (`TrendVolTarget::decide`)

Features read (all must be present, else `Skip` with reason `missing_feature`): `close`,
`history_days`, `position_qty`, `equity`, `n_assets`. `momentum` and `var_daily` may be
absent (not enough history).

1. `close <= 0`, `equity <= 0` or `n_assets < 1` → `Skip`, `bad_input`.
2. `momentum` or `var_daily` absent → `Skip`, `insufficient_history`.
3. Target shares:
   - `momentum <= 0` → target 0 (`trend_down`).
   - else `asset_vol = sqrt(var_daily × sessions_per_year)` (in `f64`); if it is zero or not
     finite → `Skip`, `zero_vol`; else
     `weight = min(max_weight, (target_vol / asset_vol) / n_assets)`, converted to a
     `Decimal` rounded half-even to 8 places, and
     `target = floor(weight × equity / close)` to 6 decimal places (`trend_up`).
4. `delta = target − position_qty`. If `|delta × close| < min_trade_notional` → `Hold`,
   quantity 0, reason `within_band`.
5. Otherwise `intended_qty = delta`; action `Enter` when `delta > 0`, `Exit` when
   `delta < 0`; reason `trend_up` or `trend_down` from step 3 (a reduction while the trend is
   still up is `Exit` with reason `trend_up`).
6. `expected_vol` is `Some(asset_vol)` whenever it was computed; `expected_edge_bps` is
   `None` (the strategy does not forecast an edge). `seed` is unused.

Long-only by construction: the target is never negative. If `position_qty` is negative
(which this strategy never creates), the delta simply buys it back.

## Simulated broker rules (`SimBroker`)

- `submit`: rejects (`SubmitError::Rejected`) a non-positive quantity, an order type other
  than market or limit, or a duplicate client order id; otherwise accepts, with venue order
  id `sim-<n>` and state `Accepted`, timestamp = the intent's.
- `on_session(bars)` settles a whole session: sells on every bar first, then buys, then
  marks; `on_bar(bar)` is a session of one bar. For each open order on a bar's symbol, in
  acceptance order:
  - market: fills in full at `costs.price(side, bar.open, qty)`;
  - limit: fills in full at the open (through the cost model) only when the open is at or
    below the limit for a buy, at or above for a sell; otherwise stays open (a `Day` order
    that did not fill is cancelled by this bar: state `Canceled`, one update);
  - a buy needs `qty × px + fee <= cash`, a sell needs `qty <= position`; otherwise the order
    is cancelled (state `Canceled`), not partly filled. The simulation never borrows or
    shorts.
  - a fill updates cash, the position (same average-cost rules as `PositionBook`), the
    order (state `Filled`, cumulative quantity, average price) and queues one `OrderUpdate`
    with a `FillDetail` whose trade id is `simfill-<n>` and whose time is the bar's
    `ts_event`.
  Then the symbol's mark becomes the bar's close.
- `account()`: equity = cash + Σ position × mark, in USD; `buying_power` = cash; never
  blocked.
- `positions()`: one line per non-flat symbol; `open_orders()`: the open list;
  `order_by_client_id`: any order ever accepted; `cancel` / `cancel_all`: mark `Canceled`
  and queue an update; `replace`: `SubmitError::Rejected` (not simulated).

## Rebalance steps (`Trader::rebalance`)

1. `reconcile_and_record`; if `!report.is_reconciled()` → `Err(NotReconciled(n))`. Nothing is
   decided on an unknown order state. If the broker has open orders the engine did not send
   (`UnknownAtBroker` findings, `unrepresentable_at_broker`) → `Err(ForeignOrders(n))`.
2. `equity` = `broker.account().equity` as a decimal.
3. For each symbol in universe order: build the features from its rolling window
   (`close`, `history_days`, `momentum` and `var_daily` when present), the position book
   plus the unfilled remainder of the engine's own unresolved orders for the symbol
   (`position_qty`; so a rebalance repeated before the fills orders nothing twice; an order
   with a provisional never-arrived verdict counts too until its recheck window passes,
   which can delay a re-sent exit by one rebalance but never doubles an order), `equity` and `n_assets`; a symbol with no bar yet, or whose last bar is older than the newest bar in the
   universe (a gap in its data), gets no price features and so a decision too
   (`Skip`, from the missing `close`). `decide_and_record` with seed `config.seed + n`.
4. If the decision's `intended_qty` is non-zero: build an `OrderIntent` (market, `Day`, side
   from the sign, quantity `|intended_qty|`, client order id `nisse-<decision id>-0`, time
   from the clock) and `submit_and_record`. `OrderRejected` and `OrderOutcomeUnknown` are
   counted in `orders_failed` and the loop goes on; any other error stops the rebalance.
5. Return the report.

Decisions for all symbols are made from the same equity figure, before any of the orders
fill, so the order of the universe does not change the sizing.

## Backtest loop (`run_backtest`)

Group the bars by `ts_event` (one group per session, in time order). For each session:

1. Set the clock to the session's close.
2. `broker.on_session(&bars_of_this_session)`: fills the orders placed at the previous
   close at this session's opens, all sells before any buy (a sale's proceeds can pay for a
   purchase made at the same open). Then apply every drained update with
   `trader.on_order_update`, then `trader.on_bar(bar)` for each bar.
3. Record the equity point (`broker.account().equity`).
4. If `calendar.is_last_session_of_month(date) == Some(true)` → `trader.rebalance()`.

The final session of the data never rebalances (the calendar cannot know it is a month end),
and orders placed at the last rebalance that never see another bar stay open: both are
correct, because there is no later price to fill them at.

## Tasks

| # | Task | Files | Round |
|---|---|---|---|
| F | Deps, crate `bars`, stubs, errors, memory store, rationale note | (foundation) | lead |
| B1 | Parquet write and DuckDB read | `crates/bars/src/writer.rs`, `crates/bars/src/reader.rs`, `crates/bars/tests/bars.rs` | 1 |
| B2 | Bar snapshot id | `crates/core/src/canonical.rs` (one function and its tests) | 1 |
| F1 | Rolling statistics and bench | `crates/engine/src/features.rs`, `crates/engine/benches/hot_path.rs` | 1 |
| C1 | Cost model | `crates/engine/src/cost.rs` | 1 |
| MT1 | Equity curve summary | `crates/engine/src/metrics.rs` | 1 |
| TR1 | `trial_result` table and store module | `migrations/0009_trial_result.sql`, `migrations/tests/0010_trial_result.sql`, `crates/store/src/tables/trial_result.rs` | 1 |
| ST1 | Trend strategy and bench | `crates/engine/src/strategies/trend.rs`, `crates/engine/benches/hot_path.rs` (add `trend_decide`) | 2 (after F1: shares the bench file) |
| SB1 | Simulated broker | `crates/engine/src/sim_broker.rs` | 2 (needs C1) |
| EN1 | Trader loop and bench | `crates/engine/src/trader.rs`, `crates/engine/tests/trader.rs`, `crates/engine/benches/hot_path.rs` (add `trader_on_bar`) | 3 (needs F1, ST1) |
| BT1 | Backtest loop | `crates/engine/src/backtest.rs`, `crates/engine/tests/backtest.rs` | 4 (needs EN1, SB1, MT1) |
| CLI | `nisse-backtest`, `nisse-ingest`, replay registry | `crates/eval/src/bin/nisse-backtest.rs`, `crates/bars/src/bin/nisse-ingest.rs`, `crates/eval/src/bin/nisse-replay.rs` | 5 (needs everything) |

### Acceptance tests

**B1.** Round trip: bars for three symbols over several days, written in scrambled order,
read back sorted by time then symbol with every price and volume exact (`0.1`, `178.235733`,
a 9-decimal price), `bar_type` `SYM.ALPACA-1-DAY-LAST-EXTERNAL`, `ts_init == ts_event`;
symbol filter; time filter inclusive at both ends; empty `symbols` → empty; empty directory →
empty; `open` on a missing directory → error; two files: only the file whose name
sorts last is read (each file is a complete snapshot; files are never merged); writing to an
existing path is an error and leaves the file unchanged; no temporary file is left after a
successful or a failed write; an empty slice writes no file; the written file is a real
Parquet file with `DECIMAL(28,9)` price columns (check with `DESCRIBE SELECT * FROM
read_parquet(..)` through a second DuckDB connection); 20,000 bars round-trip.

**B2.** `bars_snapshot_id`: 64 lowercase hex characters; equal for the same bars; differs when
any field of any bar changes or the order changes; `1.50` and `1.5` closes give the same id;
the empty slice gives the SHA-256 of the empty string
(`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`); a known two-bar case
asserted against the hash of its literal text (compute the expected value in the test with
`sha2` from the documented line format).

**F1.** `new` rejects `lookback <= skip`, `vol_window < 2`, `lookback == 0`; `stats()` is
`None` before the first close; `history_days` counts every push; `momentum` is `None` until
`history_days > lookback_days`, then equals `close[t−skip]/close[t−lookback] − 1` (check
against a hand-computed series, and again after the ring has wrapped several times);
`var_daily` is `None` until `vol_window` returns exist, then equals the population variance
of the last `vol_window` simple returns computed directly from the retained closes (compare
with a straightforward recomputation after every push over a 2,000-step deterministic
pseudo-random series: the incremental and the direct figures must be **equal**, not close);
a constant series has variance exactly 0; a non-positive close is `InvalidBar` and leaves the
statistics unchanged; `var_daily` is never negative. Bench `rolling_push`: push into a warm
window (lookback 252, vol window 60).

**C1.** Buy at 100 with 5 bps → 100.05; sell → 99.95; zero slippage → the reference price;
rounding is to 6 places and never in the order's favour (a buy rounds up, a sell rounds
down: test with a price whose 7th decimal is non-zero); fee = `qty × fee_per_share` rounded
up to the cent, 0 when the rate is 0; negative parameters are `InvalidConfig`; fractional
quantities.

**MT1.** Fewer than two points or non-positive start → `None`; a flat curve → all zeros;
a curve that doubles over exactly one year of sessions → `total_return` 1 and `cagr` 1 (to
1e-9); a known five-point curve with hand-computed `ann_vol`, `sharpe` and `max_drawdown`;
drawdown is measured from the running peak (a new high resets it) and reported positive;
`sharpe` is 0 when volatility is 0; `to_json` has exactly the six keys and writes a NaN as
`null`.

**TR1.** Migration: table `trial_result (id, trial_id fk(trial_registry) UNIQUE, result
JSONB NOT NULL, event_ts, ingest_ts)`, append-only guard, grants for `nisse_writer`
(insert all columns but `id` and `ingest_ts`, select) and `nisse_reader` (select), following
0007. SQL test: valid insert; second result for the same trial → 23505; unknown trial →
23503; NULL result → 23502; append-only; the table-list contract test
(`migrations/tests/0006_store_tables.sql`) is updated to include `trial_result`. Store
module `trial_result::{insert(client, trial_id, &Value, ts), get(client, trial_id)}` with
integration tests in `crates/store/tests/trial_result.rs` (insert, read back, duplicate is
`StoreError::Db`, missing is `None`).

**ST1.** One test per rule and per reason code above, with exact quantities (for example
equity 100,000, close 100, 8 assets, daily variance giving a 16% annual volatility, target
9%: weight `0.09/0.16/8 = 0.0703125`, target `70.3125` shares); the cap binds for a
low-volatility asset; `within_band` just below and `Enter` just at the minimum notional;
reduction with the trend still up; exit to zero when momentum turns; negative position is
bought back; `from_params(&to_params())` round-trips and the JSON has `"strategy":
"trend_vol_target"` with decimals as strings; each invalid parameter is `InvalidConfig`;
`strategies::build` returns it for the right name and `UnknownStrategy` otherwise; `decide`
is deterministic (1,000 calls, same verdict) and ignores the seed; replaying a decision made
by it gives `Match`. Bench `trend_decide`.

**SB1.** One test per rule above, with exact cash and position arithmetic: market buy fills
at the next bar's open plus slippage, cash falls by `qty × px + fee`; nothing fills on a bar
for another symbol; a sell raises cash and reduces the position; a buy that cash cannot
cover, and a sell beyond the position, are cancelled with one `Canceled` update and no cash
change; limit orders fill or are cancelled by the open; equity marks at the close; updates
are drained once and in order; duplicate client order id is rejected; fees accumulate;
`order_by_client_id` finds a filled order; two orders on one symbol fill in acceptance order.

**EN1.** With `SimBroker`, `MemoryStore` and `ManualClock`: `new` rejects an empty, unsorted
or duplicated universe; `on_bar` updates only its symbol's window and ignores a foreign
symbol; a rebalance before enough history records one `Skip` per symbol and sends no order;
after enough rising history it records `Enter` decisions and one market order per symbol,
with intents recorded before submission, client order ids `nisse-<id>-0`, and seeds
`seed + n`; all symbols are sized from the same equity; after the fills are applied the
position book matches the broker; a second rebalance with unchanged prices records `Hold`s
and sends nothing; a falling asset is exited; a rejected order is counted in
`orders_failed` and the rest still go out; an unresolved order (`FakeBroker` with a failing
lookup) gives `NotReconciled` and no decision is recorded. Bench `trader_on_bar`.

**BT1.** On synthetic bars: a steadily rising asset is entered after the warm-up and held;
a falling one is never held; fills happen at the session after the decision, at its open,
never at the deciding close (assert the fill time and price); equity is cash while flat;
`fees_paid` equals the sum over fills; the equity curve has one point per session; a run is
deterministic (two runs, equal `BacktestRun`); every decision replays as `Match` through
`nisse_engine::replay`; with zero slippage and fees, equity changes equal position × price
changes; bars for a symbol that starts late are handled.

**CLI.** `nisse-backtest <config.json>`: reads the config (strategy params, symbols, start,
end, starting cash, cost model, bars directory), reads bars, refuses to run when
`!build_is_replayable()` unless `--allow-dirty` is given (then the trial result is marked
`"dirty_build": true`), ensures the strategy version, **registers the trial before
running**, runs, writes the `trial_result`, prints the summary. A failed run still leaves
the trial registered. `nisse-ingest`: reads credentials from the environment, fetches daily
bars for the symbols and dates given on the command line with the configured feed, and
writes one Parquet file; never run by an agent. `nisse-replay`'s registry becomes
`nisse_engine::strategies::build`. An integration test runs the backtest function behind the
binary against a scratch database with synthetic bars and checks the trial count rose by one
and the result row exists.

## Not done by this phase's PR

- A real backtest. It needs the owner: credentials to run `nisse-ingest`, and the feed
  decision. The first real run is trial number one of the lifetime budget.
- Promotion gates (deflated Sharpe, PBO, purged CV): Phase 5.
- The risk gate between decision and order: Phase 4. `Trader::rebalance` sends what the
  strategy decides.
- A live trading binary: after Phase 4.
