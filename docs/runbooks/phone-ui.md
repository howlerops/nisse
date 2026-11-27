# The phone app

How to get `ui/` onto a phone and prove the kill switch works from it. Everything here is
the owner's; none of it has been done.

## What the app can do

Read the store, and engage the kill switch. It cannot release the switch, write anything
else, or reach the broker. Engaging from the phone writes a row. On the host a watcher
(`nisse-engine kill-watch`, every minute, day and night) reads the switch and, when it is
engaged, cancels what is open at the broker; every engine run also reads the switch first,
and again just before it would send orders. So "engaged from the phone" means: open orders
cancelled within about a minute at any hour, and no new ones, except that a rebalance in
the very act of sending may still send, and is then cancelled by the watcher. The watcher
must be running: it is a line in `deploy/crontab`. For an immediate and complete stop,
`deploy/engine.sh kill --by <name>` on the host, or the out-of-band steps in
`docs/runbooks/kill-switch.md`, remain the stronger tools.

## Decide first

**How the phone reaches Postgres.** The app does not encrypt its connection. Use a private
network: a VPN between the phone and the host (WireGuard, Tailscale), or an SSH tunnel. Do
not open the database to the internet for this. If you would rather have TLS, say so: it
needs one new dependency in the app's Rust module.

## Set up

1. **Database.** Apply the migrations (`scripts/migrate.sh`; 0014 adds the role and the
   function). Create the phone's login, a member of `nisse_ui` only:
   `CREATE ROLE nisse_phone LOGIN PASSWORD '...' IN ROLE nisse_ui;`
   Let that login connect only from the private network (`pg_hba.conf`).
2. **Engine.** The host must be running the build that includes Phase 8, and its crontab
   must have the `kill-watch` line (re-install `deploy/crontab`). Check `log/killwatch.log`
   gets "kill-watch: switch off" every minute.
3. **Native Shell.** Install it on your computer (`https://nsh.dev/docs/installation`), the
   same version as the `nsh-*` crates in `ui/ext/nisse-native/Cargo.toml` (0.7.2).
4. **Check and run.** In `ui/`: `nsh check` (TypeScript types: this has never been run),
   then `nsh run . --target ios` or `--target android`. This is the first device build.
   If a Rust dependency does not build for the phone, note which one: the likely fix is to
   make `nisse-ui-data` independent of the NautilusTrader types that come in through
   `nisse-core`.
5. **Connect.** In the app's Settings, enter
   `postgres://nisse_phone:<password>@<host on the private network>/<database>`. It is kept
   in the app's storage on the device, which is not an encrypted keychain.

## The end-to-end test (the phase's exit criterion)

Against the paper account, in market hours, with one of the engine's own orders resting (for
example the morning after a month-end rebalance, before the open, or a limit order far from
the market). Do not place a test order by hand in Alpaca's dashboard: an order the engine did
not send makes its reconciliation stop.

1. On the phone, Status shows "Kill switch: off" and a recent heartbeat.
2. Kill: type your name, "Engage kill switch", "Yes, engage now". The app shows
   "Kill switch ENGAGED" only after reading it back.
3. Within about a minute, the app's Activity screen shows "kill  cancel: done",
   `log/killwatch.log` on the host has "switch ENGAGED; N open orders cancelled", and Alpaca
   shows no open orders. Repeat the test once in the evening, with the market closed and an
   order resting for the next open: that is the case the watcher exists for.
4. Status on the phone shows "Kill switch: ENGAGED".
5. Turn the phone's network off and open the app: it shows "CANNOT CONFIRM", never a state.
6. Release on the host: `deploy/engine.sh release-kill --by <name>`. Status shows "off".

Write down the times of steps 2 and 3: the gap is how long a phone kill takes to cancel.

## If the phone is lost

`ALTER ROLE nisse_phone NOLOGIN;` (or drop it). The credential could read the store and
engage the kill switch; it could not trade, release, or change anything else.
