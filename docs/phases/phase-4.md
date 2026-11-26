# Phase 4: Risk gate and kill switch

Issue: #6. Exit criterion: fault-injection tests pass on synthetic data.

Everything under `/crates/risk/` and `/config/limits/` is a sensitive path (CLAUDE.md). The
gate fails closed: anything it cannot evaluate is a failed check, and a failed check means no
order.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Where the gate runs | In `Trader::rebalance`, between the recorded decision and the order, for backtest and live alike | One code path. A blocked order's decision still stands in the store, with the checks that blocked it. |
| Gate crate | `nisse-risk`: pure, no I/O, no clock, no async. Depends on `nisse-core` only. | It can be read in one sitting and tested exhaustively. |
| Limits file | JSON at `config/limits/limits.json`; the path is given to every binary explicitly. Decimals are strings. Unknown fields are errors. | No new dependency (`serde_json` is already used). In production the file lives outside the agent-writable checkout. |
| Hard ceilings | The code refuses a limits file that is looser than fixed ceilings (see "Limits") | A bad or tampered file cannot loosen the rules past what was reviewed in code. |
| "At risk" per trade | Order notional × `risk_sigmas` × daily volatility (default 2 daily standard deviations) | The strategy has no stop price; this is a stated, checkable definition. **The owner should confirm it.** |
| Quarter-Kelly | Gross exposure / equity ≤ `kelly_fraction × haircut_sharpe / portfolio_vol` | Kelly leverage is Sharpe / volatility. `haircut_sharpe` is a placeholder (0.4) until Phase 5 measures it: **owner to confirm.** |
| Drawdown step-down | Modes `normal` → `halved` (10%) → `flat` (15%), from the equity high-water mark. `halved` returns to `normal` below 5%. `flat` is latched until an explicit reset. | "Re-enter only through the gates": nothing in the engine lifts `flat` by itself. |
| How the step-down acts | The mode sets a `risk_scale` feature (1, 0.5, 0) that the strategy multiplies its target weight by, and the gate independently caps exposure by the same scale | The decision stays replayable (the scale is in its features), and the gate does not trust the strategy. |
| Kill switch | A latched flag. Engaging it cancels every open order and blocks every new one. With `flatten_on_kill` the scale is 0 and reducing sells are let through, so the normal decision path exits everything. If the flag cannot be read, it is engaged. | Fails closed; flatten goes through recorded decisions, not a side door. |
| Risk state | New append-only table `risk_event` (peak equity, mode changes, kill switch, limits loaded). State after a restart is the fold of its rows. | Rows are never updated; the state machine lives in Postgres. **New table: owner can veto.** |
| Heartbeat | New append-only table `heartbeat`; `nisse-deadman` exits non-zero when the newest row is too old | A cron job on another host turns that into an alert. **How the alert is delivered (mail, push) is the owner's choice and may need an external service.** |
| Day trades | A same-UTC-day round trip in one symbol counts as a day trade; the limit is `max_day_trades` in any 7 calendar days (a superset of 5 sessions) | Conservative, needs no calendar. The monthly strategy never comes near it. |

## Limits (`nisse_risk::Limits`)

| Field | Default file | Accepted range (else the file is refused) |
|---|---|---|
| `max_risk_per_trade` | `"0.005"` | > 0 and ≤ 0.005 |
| `risk_sigmas` | `"2"` | ≥ 1 and ≤ 10 |
| `kelly_fraction` | `"0.25"` | > 0 and ≤ 0.25 |
| `haircut_sharpe` | `"0.4"` | > 0 and ≤ 2, and `kelly_fraction × haircut_sharpe / portfolio_vol` (the exposure cap) ≤ 1.5 |
| `portfolio_vol` | `"0.09"` | ≥ 0.05 and ≤ 0.5 |
| `drawdown_halve` | `"0.10"` | > `drawdown_resume` and ≤ 0.10 |
| `drawdown_flat` | `"0.15"` | > `drawdown_halve` and ≤ 0.20 |
| `drawdown_resume` | `"0.05"` | > 0 |
| `max_bar_age_hours` | `96` | 1 to 240 |
| `max_day_trades` | `3` | 0 to 3 |
| `flatten_on_kill` | `false` | boolean |
| `heartbeat_max_age_seconds` | `300` | 30 to 3600 |

`Limits::hash()` is the SHA-256 of the canonical JSON of the parsed values
(`nisse_core::canonical_json`), stored in a `limits_loaded` risk event and in every backtest
result.

## Gate rules (`RiskGate::check`)

Every rule is evaluated for every order, in this order, and each produces one `risk_check`
row (`rule`, `limit_value`, `actual_value`, `passed`). The order goes out only if all pass.
A "reducing sell" is a sell with `0 < qty ≤ max(sellable_qty, 0)`, where `sellable_qty` is the filled position minus the engine's own unfilled sells. Shares that are only on order (an unfilled buy) are not sellable: a sell against them could leave the account short. `-1` as `actual_value` means
"could not be computed", which always fails.

| # | Rule | Passes when | `limit_value` | `actual_value` |
|---|---|---|---|---|
| 1 | `kill_switch` | not engaged, or (`flatten_on_kill` and a reducing sell) | 0 | 1 if engaged else 0 |
| 2 | `account_blocked` | the broker does not report the account blocked | 0 | 1 if blocked else 0 |
| 3 | `drawdown_flat` | a sell, or mode is not `flat` | `drawdown_flat` | current drawdown (0 when no peak yet) |
| 4 | `stale_data` | a sell, or the newest bar's age is known and ≤ `max_bar_age_hours` | the limit in seconds | age in whole seconds, or -1 |
| 5 | `no_short` | a buy, or a reducing sell | `max(sellable_qty, 0)` | sell quantity (0 for a buy) |
| 6 | `risk_per_trade` | a sell, or `qty × px × risk_sigmas × sqrt(var_daily) ≤ max_risk_per_trade × equity` with `px`, `var_daily` present, `px > 0`, `var_daily ≥ 0`, `equity > 0` | `max_risk_per_trade × equity` | the risk in dollars, or -1 (0 for a sell) |
| 7 | `kelly_exposure` | a sell, or `(gross_exposure + qty × px) / equity ≤ kelly_fraction × haircut_sharpe / portfolio_vol × scale` (scale 1, 0.5 or 0 by mode) | the cap | the ratio, or -1 (the ratio before the order for a sell) |
| 8 | `day_trade_limit` | the order would not complete a day trade, or `day_trades_in_window + 1 ≤ max_day_trades` | `max_day_trades` | the count after the order |

Amounts are rounded half-up to 6 decimal places before they are compared and stored. The
square root is `rust_decimal`'s decimal square root (no `f64`).

## Drawdown (`DrawdownState`)

`observe(equity)`:

1. `equity ≤ 0` → mode `flat` (a non-positive account is the worst drawdown).
2. No peak yet, or `equity > peak` → the peak becomes `equity` (reported, so it is stored).
3. `drawdown = 1 − equity / peak`.
4. Mode `flat` stays `flat`. Otherwise: `drawdown ≥ drawdown_flat` → `flat`; else
   `drawdown ≥ drawdown_halve` → `halved`; else mode `halved` and `drawdown < drawdown_resume`
   → `normal`; else unchanged.

`reset(equity)` (an explicit, recorded human action): peak = `equity`, mode `normal`.
`DrawdownState::restore(events)` folds `risk_event` rows in id order (`equity_peak`,
`drawdown_mode`); an event it cannot read is an error, and the caller must not trade.

## Trader changes

- `TraderConfig` gains `limits: Limits` and `risk: RiskState` (restored state; `Default` is a
  fresh account). The trader owns a `RiskGate`.
- `Trader::observe_equity(equity)`: feeds the drawdown state, emits a `StoreRecord::RiskEvent`
  for a new peak and for a mode change, and returns the mode change if there was one.
- `Trader::set_kill_switch(engaged)` / `Trader::engage_kill_switch()`: the latter sets the
  flag, emits the event and calls `Broker::cancel_all`; a cancel failure is returned as an
  error **after** the flag is set (the flag never depends on the broker answering).
- `rebalance`: after reconciliation, `observe_equity`; every decision's features carry
  `risk_scale`; before each order the gate checks it, every check is emitted as
  `StoreRecord::Risk`, and a blocked order is counted in `RebalanceReport::orders_blocked`
  and not sent. Data age is `clock.now() − newest bar time` (unknown when there is no bar).
  Gross exposure is `Σ |held + working| × last close` over the universe; a symbol with a
  position and no close makes it unknown, which fails `kelly_exposure` for buys.
- Fills reach the day-trade tracker from three places, each once: `Trader::on_order_update`
  (only a fill that moved the order's filled quantity; a redelivered message is not a new
  fill), a fill in the broker's answer to a submit, and fills found by reconciliation
  (counted even when the pass then ends in an error). After a restart the caller
  feeds the last seven days of fills back with `Trader::record_past_fill`
  (`Store::recent_fill_sides`).
- Durability: `RecordSink::flush` is awaited after a kill-switch event, after a drawdown mode
  change, and after an approved order's eight checks, before the order is submitted. A kill
  switch engaged in the process is never lowered by `refresh_kill_switch`; only
  `release_kill_switch` (a person's decision) lowers it.
- At zero risk scale (flat, or a flattening kill switch) a rebalance first asks the broker to
  cancel every waiting buy. A sell never asks for more than is sellable now: the decision may
  count shares still on order, the order sells what is held, so an order's quantity can be smaller than its decision's
  `intended_qty` (the `no_short` row records the quantity sold). Waiting buys are NOT
  cancelled on a step to `halved`: the next rebalance resizes from held plus working. A
  failed cancel is logged and reconciliation reports what became of the order.
- Flat means the strategy's target is zero, and exits go through ordinary decisions. A symbol
  the strategy cannot decide for (no current bar, not enough history) or whose buy is still
  unfilled stays held; `Trader::needs_flatten` reports that, and `run_backtest` rebalances
  every session while it is true.
- A position in a symbol outside the universe makes gross exposure unknown, which fails
  `kelly_exposure` for buys.
- `day_trade_limit` can block an exit on the day of a buy fill when the count is at its
  maximum. That is deliberate (a pattern-day-trade flag restricts the whole account) and is
  **the owner's to confirm**.
- `run_backtest` calls `observe_equity` every session and rebalances at once (same close)
  when the mode steps down, besides the month-end rebalance. `BacktestConfig` gains `limits`.

## Tasks

| Task | What | Files | Order |
|---|---|---|---|
| F | Foundation (lead): types, stubs, limits file, signature plumbing | many | 0 |
| R1 | Limits parsing, validation, hash | `crates/risk/src/limits.rs` | 1 |
| R2 | Drawdown state machine and restore | `crates/risk/src/drawdown.rs` | 1 |
| R3 | Day-trade tracker | `crates/risk/src/day_trades.rs` | 1 |
| S1 | `risk_event` and `heartbeat` tables | `migrations/0010_risk_event.sql`, `migrations/tests/0011_risk_event.sql`, `migrations/tests/0006_store_tables.sql` | 1 |
| E1 | `risk_scale` in the trend strategy | `crates/engine/src/strategies/trend.rs` | 1 |
| R4 | Gate rules | `crates/risk/src/gate.rs` | 2 (needs R1, R2) |
| S2 | Store modules for the two tables, writer arm, kill-switch source, risk state load | `crates/store/src/tables/{risk_event,heartbeat}.rs`, `crates/store/src/writer.rs`, `crates/store/tests/risk.rs` | 2 (needs S1) |
| E2 | Gate wired into the trader | `crates/engine/src/trader.rs`, `crates/engine/tests/trader.rs` | 3 (needs R4, E1) |
| E3 | Backtest loop: observe every session, step-down rebalance | `crates/engine/src/backtest.rs`, `crates/engine/tests/backtest.rs` | 4 |
| E4 | Fault-injection tests | `crates/engine/tests/fault_injection.rs` | 4 |
| V1 | Backtest job takes limits; `nisse-deadman` | `crates/eval/src/backtest_job.rs`, `crates/eval/src/bin/nisse-deadman.rs`, `crates/eval/tests/backtest_job.rs` | 4 (needs S2) |
| D | Kill-switch runbook, PLAN/README sync (lead) | `docs/runbooks/kill-switch.md` | 5 |

## Hot path

Per bar nothing changes (`Trader::on_bar`). Per order: `RiskGate::check`, eight rules, a
handful of decimal operations and one decimal square root, one small `Vec` of results. It runs
a few times a month; no bench. The gate itself allocates only the result vector (rule names
are `&'static str`); the trader copies each name into its `risk_check` row.

## Not in this phase

- The engine process that polls the kill switch and writes heartbeats on a timer (Phase 6).
- A phone-reachable kill switch (Phase 8); the store-backed flag it will set exists here.
- Measuring the haircut Sharpe (Phase 5).
