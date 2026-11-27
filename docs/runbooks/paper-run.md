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
3. **Build** on the host or copy the release binaries to `/opt/nisse/bin`: `nisse-engine`,
   `nisse-score`, `nisse-replay`, `nisse-deadman`. Copy `config/`, `scripts/` and `deploy/`
   next to them. Build from a clean checkout: a dirty build cannot be replayed.
4. **Environment**: copy `deploy/nisse.env.example` to `/etc/nisse/nisse.env`, fill it in with
   the Alpaca **paper** keys, `chmod 600`. The keys exist only in that file.
5. **Limits**: read `config/limits/limits.json` and confirm it is what you want the paper
   account held to. For production the file lives outside the checkout; point
   `limits_path` at it.
6. **Rehearse stopping** once: `docs/runbooks/kill-switch.md`, the out-of-band steps.
7. **Dry start**: `bin/nisse-engine status config/engine/paper.json` (reads only), then
   `bin/nisse-engine tick config/engine/paper.json` by hand during market hours. The first
   `session-close` downloads the whole bar history.
8. **Schedule**: `crontab deploy/crontab`.
9. **Dead-man check** on another machine: `nisse-deadman config/limits/limits.json` from cron
   every few minutes during market hours; a non-zero exit is the alert.

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
