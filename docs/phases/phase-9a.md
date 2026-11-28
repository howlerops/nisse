# Phase 9a: what the tracking gate needs

Issue: #11. Three gaps found while writing `phase-9.md`, decided by the owner on 2026-11-28.
None of this enables live trading: the engine still runs against paper only.

## Decisions

| Question | Decision |
|---|---|
| Where live daily returns come from | A new append-only table, `account_equity`: session-close writes the broker's account equity once per session |
| The backtest the tracking gate compares with | Its own kind of run, a **tracking run**: allowed only for a strategy config the engine has traded, registers a trial (it counts toward the lifetime budget), is not a holdout look, and is never shown to research agents |
| Whether the paper engine registers a trial | Yes: one `live`-kind trial per strategy config and environment, the first time the engine runs a session close for it |
| Orders only through the risk gate | A test that reads the source and fails when a new non-test caller of the order-sending functions appears (`crates/app/tests/order_path.rs`) |

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

Session close gains a step, `equity`, after `observe`: if the config has no `live` trial for
this environment it registers one (`data_range` from now, open-ended; result
`{"environment": "paper"}`), then writes the session's `account_equity` row from the equity it
has just observed. A re-run of the same session skips the step once it is `done`.

### Tracking runs

`nisse-backtest <job.json> --tracking`:

- refused unless the job's strategy config has a `live` trial, and unless the bars reach the
  first session the engine ran;
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
environment's if it has any rows, else paper's) and the newest tracking run. The live return
of a session is `equity / previous session's equity - 1`. The two series are compared over the
sessions that have both, from the second session with an equity row to the earlier of the
last equity row and the last tracking session. A session inside that span that the backtest
has and the equity series lacks makes the gate `not_evaluated` (a missed session close must
be seen, not skipped). No equity rows, fewer than two paired sessions, or no tracking run:
`not_evaluated`, as today. The agents' report leaves the tracking gate out, like the holdout.

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
