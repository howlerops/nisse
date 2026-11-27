# Paper run

How to put the engine on one small host against Alpaca paper and run it for two weeks. Paper
is a plumbing test: nothing learned here is evidence about the strategy, and no result of it
promotes anything.

Everything on this page is done by the owner. No agent provisions the host, holds the keys or
runs the engine against Alpaca.

## Before the first day

1. **Decide the feed** (`"feed"` in `config/engine/paper.json`): `iex` is free, `sip` needs the
   paid data subscription.
2. **Provision one small Linux VM** with Postgres 16 on it or reachable from it. Create a
   database, a login role that is a member of `nisse_writer`, and run `scripts/migrate.sh`
   against it as the database owner.
3. **Check out and build** on the host: clone the repository to `/opt/nisse` at the commit
   you mean to run and `cargo build --release --locked`. The checkout must be clean: the
   build records its commit, and a dirty build cannot be replayed. `mkdir log data`.
   The nightly replay (`scripts/replay.sh`) builds its own binary from the same checkout;
   after you move the checkout to a newer commit it exits 3 to say that the older build's
   decisions were not replayed. That is expected and worth one line in the write-up.
4. **Environment**: copy `deploy/nisse.env.example` to `/etc/nisse/nisse.env`, fill it in with
   the Alpaca **paper** keys, `chmod 600`. The keys exist only in that file.
5. **Limits**: read `config/limits/limits.json` and confirm it is what you want the paper
   account held to. For production the file lives outside the checkout; point
   `limits_path` at it.
6. **Rehearse stopping** once: `docs/runbooks/kill-switch.md`, the out-of-band steps.
7. **Dry start**: `target/release/nisse-engine status config/engine/paper.json` (reads only,
   needs no Alpaca keys), then `target/release/nisse-engine tick config/engine/paper.json` by
   hand during market hours. The first
   `session-close` downloads the whole bar history.
8. **Schedule**: `crontab deploy/crontab`.
9. **Dead-man check** on another machine: `nisse-deadman config/limits/limits.json` from cron
   every few minutes during market hours; a non-zero exit is the alert.

## Stopping

`target/release/nisse-engine kill config/engine/paper.json --by <your name>` engages the kill
switch, cancels every open order, waits for any engine run in flight to end and cancels once
more. If it says a run was still in flight, run it again. `release-kill` with the same
arguments lowers the switch; nothing else does. If the host is unreachable, use the
out-of-band steps in `docs/runbooks/kill-switch.md`.

The engine's own log lines (`tracing`) are not printed anywhere yet: each command prints one
outcome line or one error line, and the detail is in the `engine_step`, `risk_event` and
`risk_check` tables.

## Each day, two minutes

- `log/session.log`: the session-close run ended with every step `done`.
- `log/tick.log`: no line saying the pass was not reconciled or that foreign orders exist.
- `log/replay.log`: the replay is clean (exit 0).
- Alpaca's dashboard: positions and open orders match `nisse-engine status`.

## What counts as a break

Write down, with the date, any of:

- a `tick` or `session-close` that ended with exit code 1 and did not succeed when re-run;
- a reconciliation finding of an order the engine did not send, or a position mismatch;
- a replay failure;
- a fill whose `shortfall_bps` (from `nisse-score`) is outside the cost model's slippage;
- a missed heartbeat during market hours.

The exit criterion is two consecutive weeks with none.

## Write-up (after two weeks)

- Dates covered, feed, build (`git_sha`).
- Sessions run, rebalances, orders sent, fills, orders blocked by the gate (and by which rule).
- Breaks, each with what happened and what was changed.
- Slippage: the distribution of `shortfall_bps` against the model's assumption.
- Anything that had to be done by hand.
- Open questions answered along the way (fractional limit orders; whether a cancel and an
  opposite-side order in one pass are accepted; whether a replacement order carries the filled
  quantity over).
