# Phase 6: Paper run

Issue: #8. Exit criterion: two clean weeks on Alpaca paper, no reconciliation breaks, slippage
within the model.

Paper is a plumbing test, not a promotion signal. This phase has two halves:

- **Built and tested here, without credentials:** the engine process, its scheduled steps, the
  state it restores from the store, the deployment files and the runbook. Tested against a
  fake market and the simulated broker, with a real Postgres.
- **The owner's:** provisioning the VM, putting the Alpaca paper credentials on it, the feed
  decision, starting the cron jobs, and the two-week run with its write-up. Nothing in this
  repository holds a credential, and no agent runs the engine against Alpaca.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Process model | One short-lived process per cron tick (`nisse-engine <command>`), no daemon | CLAUDE.md: cron plus a Postgres state machine with idempotent steps. Every run rebuilds its state from the store and the bar files, so a crash loses nothing that was recorded. |
| Order updates | By reconciliation on each tick, not the websocket stream | A daily strategy's orders are sent after the close and fill at the next open; a tick every few minutes during market hours sees them. The stream client stays in the adapter for later. |
| New crate | `nisse-app` (`crates/app`), binary `nisse-engine` | It is the only crate that depends on both the engine and the broker adapter. The engine crate stays free of any broker. |
| Market access | Trait `Market` (calendar and daily bars); the Alpaca implementation wraps the adapter | The session logic is tested with a fake market and the simulated broker. |
| Bars | Each session close fetches the whole history for the universe and the benchmark and writes one complete file `<bars_dir>/<YYYY-MM-DD>.parquet` | Files are complete snapshots (Phase 3): prices are adjusted as of one fetch, never spliced. About ten symbols by ten years is a few seconds. |
| Step log | New append-only table `engine_step` (`run_key`, `step`, `status`, `detail`) | The state machine: a re-run of the same tick skips steps already `done`. **New table: owner can veto.** |
| One engine at a time | A database advisory lock held for the life of the process | Two overlapping ticks must not both decide. |
| Environment | `paper` only; the config parser refuses anything else | Live endpoints are Phase 9 and the owner's. |
| Decision time | The session-close tick runs a few minutes after the close and decides then | Keeps the decision inside the 20-hour base-bar rule of outcome scoring (Phase 5). The engine never decides off-session; a missed tick is re-run the same evening or skipped, not run the next morning. |

## Commands

- `nisse-engine session-close <config.json>`: once per session, after the close.
- `nisse-engine tick <config.json>`: every few minutes while the market is open, and once after
  the open.
- `nisse-engine kill <config.json> --by <name>` and `nisse-engine release-kill <config.json>
  --by <name>`. `kill` does two things at the same time, neither waiting for the other: it asks
  the broker to cancel every open order, and it connects to the store and records the
  switch (ten seconds allowed for each store step). Then it waits up to a minute for a run
  in flight to end, cancels again, and checks that the broker lists no open order. It does
  not read the config file. With the store unreachable it still cancels and checks; with
  the broker keys missing it still records the switch. It prints which of the three (orders
  cancelled, switch recorded, engine idle) were done and exits 1 unless all were.
- `nisse-engine status <config.json>`: prints the risk state, open orders and positions the
  engine would start from. Reads only.

Every command reads `DATABASE_URL` and, when it talks to the broker, `APCA_API_KEY_ID` and
`APCA_API_SECRET_KEY` from the environment. None of them is ever printed.

## Engine config (`config/engine/paper.json`)

| Field | Meaning |
|---|---|
| `environment` | `"paper"` (anything else is refused) |
| `strategy` | the strategy parameters, stored as `strategy_version.params` |
| `symbols` | the universe |
| `benchmark` | fetched and stored with the universe, for the scoring job |
| `feed` | `"iex"` or `"sip"` |
| `history_start` | first date of bar history to fetch, `YYYY-MM-DD` |
| `bars_dir` | where bar files are written and read |
| `limits_path` | the risk limits file |
| `seed` | base seed for decisions |
| `source` | heartbeat source name, default `"engine"` |

Unknown fields are errors. Symbols are upper-case, unique and non-empty; the benchmark may be
one of them.

## What every command does first (`restore`)

1. Take the advisory lock; if another engine holds it, exit without doing anything.
2. Load the limits file; ensure the strategy version (`git_sha` of this build, the config
   hash of the strategy parameters).
3. Risk state: `RiskState::restore(store.load_risk_events())`. An unreadable event is fatal:
   the engine does not trade on an unknown risk state.
4. Orders: `tracker.restore_all(store.load_live_orders(..))`.
5. Positions: the newest `position_event` row per symbol.
6. Day trades: the last seven days of fills, through `Trader::record_past_fill`.
7. Build the trader; feed it every bar of the newest bar file, in time order.
8. `refresh_kill_switch(store)` (unreadable means engaged).
9. Record `limits_loaded` if the limits hash differs from the newest such event.

## `tick`

1. Take the lock; `restore`.
2. Reconcile with the broker and record what changed (fills, order states, positions).
   A pass that is not fully reconciled, or that finds orders the engine did not send, is
   logged as an error and the tick ends there (exit code 1): nothing is decided.
3. Observe the account equity (drawdown state).
4. If a rebalance is owed (below), or the risk scale is zero and something is still held
   (`needs_flatten`), and the market is open now (the calendar says so; a weekday holiday
   is not a session), and trading is not halted, and no tick has tried in this hour:
   rebalance. With the market closed a tick never decides.
5. Flush the store queue; write a heartbeat. A tick that failed writes no heartbeat, so a
   tick that keeps failing trips the dead-man check.

### An owed rebalance

A rebalance that should happen must survive the process dying before it does, the market
being closed, and orders being refused. So "a rebalance is owed" is itself a step:
`engine_step` run key `resize`, step `pending`, recorded `started` when

- the equity observation reports a drawdown step-down;
- a rebalance ran but some of its orders were blocked by the gate or refused by the broker
  (the book is not at its target);
- a session-close rebalance was due but trading was halted (below);

and `done` after a rebalance in which every order went out. While it is `started`, a
rebalance is due: at the next tick that may decide, or at the next session-close. (The mode
change and the `started` row are two statements, not one transaction; a crash exactly
between them loses the resize until the month end.)

Trading is *halted* when no order could go out anyway: the kill switch is engaged and the
limits do not let exits through it (`flatten_on_kill` false), or the broker has blocked the
account. A halted run decides nothing, since every decision's order would be refused, and
the rebalance stays owed. After `release-kill` the first tick that may decide makes it.

A tick tries an owed rebalance at most once an hour (its attempt is the step `rebalance`
under run key `tick/<UTC date>T<hour>`, with the risk scale it ran at; only a completed
attempt counts), so orders that stay blocked for another reason cost one set of decisions
an hour, not one every tick. A risk scale that has dropped since that attempt (a drawdown
step-down, a kill switch that flattens) is not held back: it is acted on at once.

Not covered: orders that were sent by a completed rebalance and then cancelled by `kill` are
not re-sent after `release-kill`; the book stays as it is until the next month end or owed
rebalance. Whether a release should owe a rebalance is the owner's call (open question).

A decision made by a tick uses the previous session's closing bars. After about noon those
are older than the 20 hours outcome scoring allows for a base bar (Phase 5), so such a
decision is scored as `base_too_old`. That is accepted: these are risk exits, not signals.

## `session-close`

The session is the newest one whose close is at or before now. If there is none, or its
close is more than `MAX_CLOSE_AGE` (6 hours) ago, the command logs that and exits 0 without
writing anything: the engine never decides off-session, so a close that was missed is not
made up for the next morning. Working from the close time keeps time zones out of it.

Run key `session-close/<session date>`. Steps, each recorded in `engine_step` as `started`
then `done` (with a detail) or `failed` (with the error text):

1. `ingest`: if `<bars_dir>/<date>.parquet` exists, skip; else fetch `history_start..=date`
   for the universe and the benchmark and write it. A symbol with no bar for the session is
   an error: nothing is decided on a universe with a hole in it.
2. `restore` (as above, now including the session's bars). Not a recorded step.
3. `reconcile`, as in `tick` step 2.
4. `observe`.
5. `rebalance`: when the session is the last of its month, or a rebalance is owed, or
   `needs_flatten`. If `engine_step` already has `rebalance` `done` for this run key, it is
   skipped, so a re-run of the command after a completed rebalance sends nothing. A re-run
   after a crash in the middle of a rebalance runs it again: orders already sent count as
   working (Phase 3), so nothing is ordered twice. When no rebalance is due the step is
   recorded `done` with `"due": false`; when one is due but trading is halted, `done` with
   `"halted": true`, and the rebalance is owed.
6. Flush; heartbeat.

The calendar is fetched from `history_start` to ten days past today, so "last session of its
month" is known.

Exit codes: 0 done or nothing to do; 1 a step failed (re-run the command); 2 bad arguments;
3 another engine still held the lock after 45 seconds.

## Deployment (files in `deploy/`)

- `deploy/crontab`: the schedule, in UTC (the host's time zone must be UTC; nothing depends
  on `CRON_TZ`). It covers both halves of the year by running a little too often: `tick`
  every 2 minutes from 13:30 to 21:00 on weekdays (the dead-man limit is 300 seconds),
  `session-close` at 20:11, 20:41, 21:11 and 21:41 (early and repeated runs do nothing), `nisse-score` at 23:00,
  `scripts/replay.sh` at 23:30.
- `deploy/engine.sh`: the same for a person at a terminal (`deploy/engine.sh kill --by NAME`).
- `deploy/run.sh`: what every cron line runs: reads the environment file, appends the
  command's output to `log/<name>.log`.
- `deploy/nisse.env.example`: the variable names the host needs, with no values.
- `docs/runbooks/paper-run.md`: provisioning, the first start, what to watch for two weeks,
  and the write-up template.

## Tasks

| Task | What | Files | Order |
|---|---|---|---|
| F | Foundation (lead): this doc, the crate skeleton, deployment files, runbook | many | 0 |
| P1 | `engine_step` table | `migrations/0013_engine_step.sql`, tests | 1 |
| P2 | Engine: standalone reconcile, position restore | `crates/engine/src/trader.rs`, `positions.rs`, tests | 1 |
| P3 | Engine config | `crates/app/src/config.rs` | 1 |
| P4 | Store: step log, newest positions, newest limits event | `crates/store/src/tables/*`, tests | 2 (P1) |
| P5 | `Market` trait, Alpaca implementation, ingest step | `crates/app/src/market.rs`, `ingest.rs` | 2 (P3) |
| P6 | `restore`, `tick`, `session-close` | `crates/app/src/restore.rs`, `session.rs`, tests | 3 |
| P7 | Binary, kill commands, status | `crates/app/src/bin/nisse-engine.rs`, `commands.rs` | 4 |
