# Kill switch and stopping trading out of band

Two independent ways to stop trading. Use the second whenever the first is in doubt: it does
not depend on the engine, the store or this repository working.

## 1. The engine's kill switch

The kill switch is a latched flag in the risk gate (`crates/risk`). While it is engaged:

- every open order is cancelled when it is engaged (`Broker::cancel_all`);
- every new order is blocked by the `kill_switch` rule, and each blocked order leaves
  `risk_check` rows saying so;
- with `"flatten_on_kill": true` in the limits file, the strategy's target becomes zero and
  only sells that reduce a position are let through, so the next rebalance exits everything
  through ordinary recorded decisions. With `false` (the default) positions are left alone.

The flag is stored as `kill_switch` rows in the append-only `risk_event` table; the newest
row wins. If the engine cannot read the flag, it treats it as engaged. Releasing it is a new
row written by a person. Nothing in the engine releases it.

On the engine host, as the user the engine runs as (Phase 6):

```
cd /opt/nisse
deploy/engine.sh kill --by <your name>
deploy/engine.sh release-kill --by <your name>
deploy/engine.sh status
```

`deploy/engine.sh` loads the host's environment file (`/etc/nisse/nisse.env`) and runs
`nisse-engine` with the output on your terminal. Running the binary directly from a login
shell does nothing useful: the database URL and the broker keys are not set there.

`kill` asks the broker to cancel every open order and writes the flag at the same time, then
waits up to a minute for any engine run in flight to end, cancels once more and checks that
the broker lists no open order. It needs neither the engine's
state nor its config file to be readable, and the cancel does not wait for the database. It
prints which of the three were done (orders cancelled, switch recorded, engine idle) and
exits 1 unless all were: then run it again, or go out of band. Every
engine run reads the flag when it starts. `status` shows the flag, the drawdown mode,
positions and the last heartbeat without changing anything.

From the phone (Phase 8, `docs/runbooks/phone-ui.md`) the flag can be engaged but not
released; a watcher on the host (`nisse-engine kill-watch`, every minute) then cancels what is
open. If neither the host nor the app can be reached, use the out-of-band steps below.

## 2. Out of band, at Alpaca

Do this from any browser; it needs only your Alpaca login.

1. Log in to the Alpaca dashboard and select the account the engine trades (paper or live).
2. Cancel all open orders.
3. If you want to be flat, close all positions.
4. Regenerate the account's API keys. The engine's old keys stop working at once, so it can
   no longer place, replace or cancel anything, whatever state it is in.
5. Stop the engine process on its host when you can reach it.

Before starting again: put the new keys on the engine host only (never in this repository),
let the engine reconcile against the broker, and check the `risk_event` table for the
drawdown mode and the kill-switch state it will start in.

Rehearse steps 1 to 4 once on the paper account, so the buttons are familiar and you have
seen the engine's requests being refused afterwards.

## Dead-man check

The engine writes a row to `heartbeat` periodically. `nisse-deadman` exits non-zero when the
newest row is older than `heartbeat_max_age_seconds` in the limits file (or there is none).
Run it from cron on a host other than the engine's, and send its failure wherever you will
see it. How that alert is delivered is not decided yet (see the "Open questions" issue).
