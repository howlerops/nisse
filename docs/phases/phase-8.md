# Phase 8: UI

Issue: #10. Exit criterion: the kill switch works from the phone.

The UI is a [Native Shell](https://nsh.dev) app: TSX screens that import a Rust module, one
code base for phone and desktop. It shows the engine's state and can do exactly one thing to
it: engage the kill switch.

## What could and could not be done in this phase's build session

- **Built and tested here:** the database role and function the phone uses, the engine's
  reaction to a kill switch engaged from outside its own process, and the Rust data layer
  (queries and the kill call), all against a real Postgres.
- **Written but not compiled or run here:** the Native Shell app itself (`ui/`). The session
  that wrote it could not install the `nsh` command (its release download is outside the
  network the session is allowed, and building it from source was not permitted). The app
  follows the framework's documentation and its own `rust-exports` example, and keeps
  everything that matters in the tested data layer, but its first compile, `nsh check`,
  `nsh test` and run on a phone are the owner's, and it should be expected to need fixes.
- **The owner's:** installing Native Shell, building for the phone, how the phone reaches
  the database, and the end-to-end test against paper.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| How the app reads the store | A Rust module in the app connects to Postgres directly | The plan's choice (open question: "confirm Native Shell can read Postgres through a Rust module"). Native Shell's docs say Rust code in an app may open TCP connections on iOS, Android and the desktops; a browser cannot, so there is no web build of this app. |
| How the phone reaches Postgres | Over a private network (a VPN such as WireGuard or Tailscale, or an SSH tunnel); the connection is not encrypted by the app | TLS to Postgres needs a new dependency. **Owner's decision**: a private network, or approve a TLS crate. The database must not be opened to the internet for this. |
| What the phone's credential can do | Read (`nisse_reader`), and call one function that engages the kill switch. Nothing else: it cannot release the switch, write any table, or reach the broker | A phone can be lost. The worst a stolen credential can do is stop trading and read the store. |
| How a phone kill takes effect | The function writes a `kill_switch` row. Every engine run reads the switch when it starts; a run that finds it engaged with orders open cancels them. Ticks run every 2 minutes in market hours | The phone never talks to the broker and holds no broker credential. Within one tick: no new orders, open orders cancelled. `nisse-engine kill` on the host remains the immediate, complete version. |
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
  `('kill_switch', {"engaged": true, "by": <by>, "via": "ui"}, now())` and returns its id.
  `by` must be 1 to 64 characters. `EXECUTE` is granted to `nisse_ui` only (revoked from
  `PUBLIC`). There is no function that releases.

No new table.

## Engine

`tick` and `session-close`, after restoring: if the kill switch is engaged and the engine
has orders it believes are open, ask the broker to cancel everything before reconciling.
(Until now only `nisse-engine kill` cancelled; a switch engaged by a row from elsewhere
stopped new orders but left resting ones.) A failed cancel fails the run: no heartbeat, so
the dead-man check notices.

## Data layer (`crates/ui-data`)

One struct over a `tokio_postgres` client, read-only except for the kill call:

- `status()`: kill switch, drawdown mode, equity peak (the fold of `risk_event`, by the same
  code the engine uses), newest heartbeat and its age, whether a rebalance is owed, the
  newest limits hash.
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
ui/nsh.json        app id, name, the Rust crate
ui/App.tsx         navigation and the five screens
ui/App.test.tsx    headless tests with the native module faked
ui/native/         crate nisse-ui-native: #[native_module("nisse/native")], wraps nisse-ui-data
```

Screens: **Status** (switch, mode, heartbeat age, owed rebalance; red when anything cannot be
confirmed), **Positions**, **Gates** (per version), **Proposals** (open pull requests),
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
| U4 | The app (lead; not compiled here) | `ui/` | 3 |
| U5 | Runbook | `docs/runbooks/phone-ui.md` | 3 |

## The owner's

- Install Native Shell; `nsh check`, `nsh test`, then run on a phone. Expect to fix the app.
- Private network or TLS for the phone's database connection.
- Create the login role for the phone (member of `nisse_ui`).
- The end-to-end test: engage from the phone against paper, watch the next tick cancel.
