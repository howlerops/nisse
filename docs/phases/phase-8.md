# Phase 8: UI

Issue: #10. Exit criterion: the kill switch works from the phone.

The UI is a [Native Shell](https://nsh.dev) app: TSX screens that import a Rust module, one
code base for phone and desktop. It shows the engine's state and can do exactly one thing to
it: engage the kill switch.

## What is verified, and what is not

- **Tested here:** the database role and function the phone uses, the engine's reaction to a
  kill switch engaged from outside its own process, the Rust data layer, and **the app
  itself, headless**: Native Shell's own test crate bundles `ui/App.tsx`, runs it on QuickJS
  with no window and no device, and drives it by pressing and typing. In those tests the app
  uses its real Rust module and a real Postgres, connected as the role a phone would have;
  the kill switch is engaged from the Kill screen and read back from the store. CI runs them.
- **Not verified:** anything on a device. The session that built this could not install the
  `nsh` command (its release download is outside the network the session is allowed), so
  there has been no `nsh check` (TypeScript type-checking: the headless bundler strips types
  without checking them), no `nsh run`, and no iOS or Android build. Whether every crate the
  module depends on (the NautilusTrader model crate comes in through `nisse-core`) builds
  for a phone target is unknown.
- **The owner's:** installing Native Shell, the device build, how the phone reaches the
  database, and the end-to-end test from a phone against paper.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| How the app reads the store | A Rust module in the app connects to Postgres directly | The plan's choice (open question: "confirm Native Shell can read Postgres through a Rust module"). Native Shell's docs say Rust code in an app may open TCP connections on iOS, Android and the desktops; a browser cannot, so there is no web build of this app. |
| How the phone reaches Postgres | Over a private network (a VPN such as WireGuard or Tailscale, or an SSH tunnel); the connection is not encrypted by the app | TLS to Postgres needs a new dependency. **Owner's decision**: a private network, or approve a TLS crate. The database must not be opened to the internet for this. |
| What the phone's credential can do | Read (`nisse_reader`), and call one function that engages the kill switch. Nothing else: it cannot release the switch, write any table, or reach the broker | A phone can be lost. The worst a stolen credential can do is stop trading and read the store. |
| How a phone kill takes effect | The function writes a `kill_switch` row. A watcher on the host (`nisse-engine kill-watch`, every minute, at any hour) cancels what is open at the broker; every engine run reads the switch first and again just before it would send orders | The phone never talks to the broker and holds no broker credential. Within about a minute: open orders cancelled, no new ones. `nisse-engine kill` on the host remains the immediate, complete version. |
| Fails closed | The app shows the switch as engaged only after reading it back from the store. If it cannot reach the store it says so and points to the out-of-band steps; it never shows "trading normally" from stale data | An app that cannot confirm must not reassure. |
| Releasing the switch | Not in the app | Releasing is deliberate and rare; it stays `nisse-engine release-kill` on the host. |
| Pull request approvals | The app lists open pull requests (GitHub's public API) and opens them in the browser. Approving and merging stay on GitHub | No GitHub token on the phone. |
| The data layer | A plain Rust crate, `nisse-ui-data`, in the main workspace, tested against Postgres in CI. The app's native module is a thin wrapper around it | The part that cannot be compiled here is kept as small as possible. |
| In-app agent | Not built | PLAN.md's note stands (inspiration: herdr-app). If added, it edits UI code only. |

## Database (migration 0014)

- Role `nisse_ui` (NOLOGIN, like the others; the owner creates a login role that is a
  member of it). It is a member of `nisse_reader`.
- Function `nisse_engage_kill_switch(by text) RETURNS bigint`, `SECURITY DEFINER`, owned by
  the migration role, `search_path` pinned: inserts one `risk_event` row
  `('kill_switch', {"engaged": true, "by": <by>, "via": "ui"}, clock_timestamp())` and returns its id.
  `by` must be 1 to 64 characters. `EXECUTE` is granted to `nisse_ui` only (revoked from
  `PUBLIC`). There is no function that releases.

No new table.

## Engine

A kill switch engaged by a row from outside the engine (the phone) is enforced by one small
function, `enforce_kill`, which needs only the store and the broker: no trader, no bar
files, no signature, no engine lock.

- It reads the switch (an unreadable switch counts as engaged). Off: nothing else happens,
  and the broker is not contacted.
- Engaged, with the limits' `flatten_on_kill` false (the switch halts): if the broker lists
  any open order, cancel everything.
- Engaged, with `flatten_on_kill` true (the switch means exit): cancel the open buys and
  leave the sells, which are the exits.
- A cancel is recorded as `engine_step` `kill` / `cancel` (`done`, or `failed` with the
  error), so it can be seen from the phone's Activity screen.

It runs in three places:

1. **`nisse-engine kill-watch`, from cron every minute, around the clock.** Orders sent after
   a close rest until the next open, and nothing else runs overnight; without the watcher a
   kill from the phone in the evening would not cancel them before they filled. With the
   switch off it costs one query.
2. First thing in `tick` and `session-close`, before the signature check, the lock and the
   restore, so it does not depend on any of them succeeding.
3. The switch is read again immediately before a rebalance sends orders, so a row written
   after a run restored still stops that run's rebalance (the rebalance is then owed).

What is left: a rebalance already past that last read sends its orders (a window of
moments); the watcher cancels them within a minute. `nisse-engine kill` on the host waits
for the run in flight and is still the complete version.

## Data layer (`crates/ui-data`)

One struct over a `tokio_postgres` client, read-only except for the kill call:

- `status()`: kill switch, drawdown mode, equity peak (the fold of `risk_event`, by the same
  code the engine uses), newest heartbeat and its age **by the database's clock** (a phone's
  clock may be wrong; a heartbeat in the future is an error), whether a rebalance is owed,
  the newest limits hash.
- `positions()`: newest row per symbol, flat ones left out.
- `open_orders()`: orders not in a final state.
- `versions()`: each strategy version with the newest stored status of each gate.
- `recent_steps(n)`: the newest engine steps (what the engine last did, and failures).
- `engage_kill(by)`: calls the function, then reads the switch back and returns it. An
  error, or a read-back that is not "engaged", is an error to the caller.

## App (`ui/`)

A Native Shell "mixed" app, its own Cargo workspace (so the main workspace and CI do not
build a UI framework):

```
ui/nsh.json              app id, name, the Rust crate
ui/App.tsx               the seven screens
ui/ext/nisse-native/     crate: #[native_module("nisse-native")], wraps nisse-ui-data
ui/ext/nisse-native/tests/app.rs   headless tests: the screens, the module and Postgres together
```

Screens: **Status** (switch, mode, heartbeat age, owed rebalance; "off" is green only while
the engine has a recent heartbeat, and no state is shown once the last read is older than two
refreshes), **Positions**, **Gates** (per version), **Activity** (the engine's newest steps,
including cancels made for the kill switch), **Proposals** (open pull requests),
**Kill** (type your name, confirm; the result is what the store says afterwards), and
**Settings** (the connection string, kept in the app's storage; Native Shell documents no
encrypted storage, so this is a plain file in the app's sandbox).

## Tasks

| Task | What | Files | Order |
|---|---|---|---|
| F | Foundation (lead): this doc, crate skeleton | many | 0 |
| U1 | Role and kill function | `migrations/0014_ui_role.sql`, tests | 1 |
| U3 | Engine cancels open orders when the switch is engaged | `crates/app/src/session.rs`, tests | 1 |
| U2 | Data layer | `crates/ui-data/src/lib.rs`, tests | 2 (U1) |
| U4 | The app and its headless tests (lead) | `ui/` | 3 |
| U5 | Runbook | `docs/runbooks/phone-ui.md` | 3 |

## The owner's

- Install Native Shell; `nsh check` (types), then run on a phone. The device build is the
  untested part: if a dependency does not build for the phone, the fix is to make
  `nisse-ui-data` independent of `nisse-core`'s NautilusTrader types.
- Private network or TLS for the phone's database connection.
- Create the login role for the phone (member of `nisse_ui`).
- The end-to-end test: engage from the phone against paper, watch the next tick cancel.
