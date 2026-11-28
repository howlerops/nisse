# Phase 9a: what the tracking gate needs

Issue: #11. Three gaps found while writing `phase-9.md`, decided by the owner on 2026-11-28.
None of this enables live trading: the engine still runs against paper only.

## Decisions

| Question | Decision |
|---|---|
| Where live daily returns come from | A new append-only table, `account_equity`: session-close writes the broker's account equity once per session |
| The backtest the tracking gate compares with | Its own kind of run, a **tracking run**: allowed only for a strategy config the engine has traded, registers a trial (it counts toward the lifetime budget), is not a holdout look, and is never shown to research agents |
| Whether the paper engine registers a trial | Yes: one `live`-kind trial per strategy config, environment and universe, before the engine first acts on it |
| What counts toward the trial budget and the deflated Sharpe | Only runs that can select a strategy: ordinary and holdout backtests, and shadow runs. Tracking runs and the engine's `live` trials are registered and kept (the record never resets) but are not counted: they judge a config that is already running and choose nothing. Otherwise routine tracking checks would use up the budget of 100 and weaken the Sharpe gate |
| Orders only through the risk gate | A test that reads the source and fails when a new non-test use of the order-sending functions appears (`crates/app/tests/order_path.rs`). It is a tripwire for honest mistakes, not a proof: the functions stay public for the engine's integration tests, and a determined change could still route around a text search. Review of every change to a sensitive path remains the control. |

"Strategy config" means `strategy_version.config_hash`. A strategy version is a config at one
git SHA, so a new build of the same config is a new version; the trial, the equity series and
the tracking run all follow the config, not the build. Otherwise every deploy would spend a
trial and restart the series.

## Design

### `account_equity` (migration 0015)

`strategy_version_id, environment ['paper' | 'live'], session_date, equity, event_ts` (plus
`id`, `ingest_ts`). The newest row per environment, config and session is that session's
equity; a correction is a new row. **Known limit:** deposits and withdrawals are not recorded,
so a session with one has a wrong return. Paper has none; recording flows is work for the
change that adds live.

### The engine's trial and equity row

`tick` and `session-close` register the trial right after the engine's state is restored
and before anything else is done with it: if the config has no `live` trial for the engine's
environment, one is registered (`data_range` from now, open-ended; result
`{"environment": "paper", "symbols": [...]}`). The environment is the engine config's, not a
constant. The strategy config's hash does not cover the universe, so the trial records the
symbols: when the engine's symbols differ from those of its newest trial, a new trial is
registered. The newest `live` trial is "the engine's current run": everything below (what a
tracking run may be, where the compared series starts) refers to it.

Session close gains a step, `equity`, after `observe`: it writes the session's
`account_equity` row from the equity it has just observed. A re-run of the same session
skips the step once it is `done`. The step is bookkeeping and never stops the run: if the
write fails the step is recorded `failed`, the error is logged, and the rebalance and the
heartbeat go ahead; the next run of the same session tries again. A session close that
never got as far as this step (the bar ingest failed all evening, say) leaves no row, which
the tracking gate reports as a gap. **Known limit:** the value is the broker's equity when the
step runs, up to six hours after the close, not at the close itself; an after-hours move
before a late run adds noise to the comparison.

### Tracking runs

`nisse-backtest <job.json> --tracking`:

- refused unless the job's strategy config has a `live` trial, unless the job's symbols are
  exactly those of the engine's current run, and unless the bars reach the day that run
  started (compared by UTC date; a first run late in the US evening falls on the next UTC
  date, so the session of that evening is not part of the comparison);
- **shows nothing about the days before the engine ran.** A config can be put on paper
  before it has passed the holdout gate, so a tracking run must not become a free look at
  the holdout. Its stored result holds returns and session dates only for sessions on or
  after the day the engine first ran, and no summary, final equity or other figure computed
  over the whole span; the binary prints nothing more than that. The result is built from a
  fixed list of keys that may be kept, so a figure added to backtest results later is left
  out of a tracking run unless someone adds it to that list on purpose;
- may read past `holdout_start` (it must: the engine runs today), is not marked `holdout`, and
  does not count toward `holdout_max_looks`. It is not a look in the sense the holdout
  protects: the config is already chosen and running, and the run only says how far the
  engine's results are from the model's over the same days;
- registers a `backtest` trial whose result has `"tracking": true`, and stores the session
  date of every return (`"session_dates"`), so the report can line the two series up by date;
- is invisible to everything that selects or judges a strategy: the store's list of backtest
  results leaves tracking runs out, so the deflated Sharpe, the overfitting matrix, the
  holdout gate and the agents' report never see one. It still counts in the lifetime trial
  count.

`--tracking` and `--holdout` together are refused.

### The tracking gate

`nisse-gate-report` loads, for the version's config: the daily equity (the `live`
environment's if it has any rows, else paper's), from the day the engine's current run
started, and the newest **usable** tracking run: made from a clean build, with the symbols
of the engine's current run and the same inputs as the candidate backtest. A newer run that
is not usable does not replace an older one that is, so a failed comparison cannot be set
aside by making a run that will not be judged. The live return
of a session is `equity / previous session's equity - 1`. The two series are compared
session by session up to the earlier of the last equity row and the last tracking session.

- **Before the engine first traded.** A fresh deployment holds cash until its first
  rebalance (a month end), while the backtest is already invested. The comparison starts
  after the first session in which the account's equity changed: the leading sessions with
  an unchanged equity, and the session of the first change (positions were being built that
  day), are left out, and the note says how many. **Known limit:** the engine starts with a
  fresh risk state, so if the backtest is in a drawdown step-down at that moment the two
  hold different sizes until it ends.
- **A failure outranks everything else.** Every stretch long enough to judge is judged
  first. If any is out of band the gate is `fail`, even when the tracking run is stale or
  the newest stretch is still short. `not_evaluated` is only ever reported when nothing
  judged has failed.
- **A gap is reported, not hidden, and cannot hide a failure.** A session the backtest has
  and the equity series lacks (a missed session close) cannot be made up by the engine. A
  gap splits the comparison into stretches. The newest stretch is the one that must reach
  the floor; every earlier stretch that reaches the floor is judged too, and the gate fails
  if any judged stretch is out of band. Earlier stretches shorter than the floor are named
  as not judged. To join two stretches, the owner inserts the missing day's equity by hand
  (`docs/runbooks/paper-run.md`).
- **A floor.** Fewer than 20 paired sessions in the newest stretch: `not_evaluated`. Two
  flat series a few days long would otherwise pass.
- **A clean build.** A tracking run made from a build with uncommitted changes
  (`--allow-dirty`) is never used.
- **Known limit: the risk limits are not tied.** The engine's limits file can change while
  it runs; the gate ties the tracking run to the candidate backtest's limits, not to what
  the engine had loaded on each day.
- **Not stale.** More than 5 equity rows after the tracking run's last session:
  `not_evaluated`, with a note to make a new tracking run.
- **The same experiment.** The tracking run must have the same inputs (costs, symbols,
  limits, starting cash, seed) as the version's candidate backtest, by the rule the other
  gates already use; otherwise `not_evaluated`. A tracking run cannot be re-made with other
  costs until it fits.
- The gate's note says what was judged: the environment, the first and last session and
  the number of sessions.

No equity rows or no tracking run: `not_evaluated`, as before. The agents' report leaves the
tracking gate out, like the holdout.

Before live, the rule for promotion is: every gate passes except `tracking`, which is
`not_evaluated` or, after a paper run, judged on paper.

## Tasks

| Id | What | Files |
|---|---|---|
| S1 | `account_equity` store module | `crates/store/src/tables/account_equity.rs`, `mod.rs`, tests |
| S2 | Live trials; tracking runs left out of backtest results; newest tracking result | `crates/store/src/tables/trial_registry.rs`, `scoring.rs`, tests |
| A1 | The `equity` step of session close | `crates/app/src/session.rs`, tests |
| B1 | `--tracking` in the backtest job and binary | `crates/eval/src/backtest_job.rs`, `bin/nisse-backtest.rs`, tests |
| G1 | The gate report loads and pairs the two series; agents' report omits the gate | `crates/eval/src/gate_report.rs`, `agent_report.rs`, tests |

## Sensitive paths

None of `/crates/risk/`, `/crates/alpaca-adapter/`, `/config/limits/`, `/.github/`.
