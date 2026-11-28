# Phase 9: Live, small

Issue: #11. Exit criterion: three months inside the tracking band before any size increase.

**Nothing in this phase has been built, and nothing here enables live trading.** Going live
means funding an account and putting live broker credentials on the engine host: both are
the owner's, and so is the decision to do it. This document is what the build sessions can
responsibly contribute first: where the system stands, what the code refuses today and
must be changed deliberately, the order of work, and the decisions that are open.

## Where the system stands (2026-11-27)

Every earlier phase's code is merged. Phase 0's criterion (CI green, migrations apply) is
met. The others each have a real-world part that is not, because it needs credentials, data
or time that only the owner has:

| Phase | Built and tested | Not yet done |
|---|---|---|
| 1 Adapter | Against a mock server | The paper round-trip tests (`docs/runbooks/paper-tests.md`) |
| 2 Engine core | Replay on synthetic decisions | The nightly replay over real decisions: nothing has run on a host |
| 3 Strategy and backtest | On synthetic bars | A backtest on real data: no bars have been downloaded |
| 4 Risk gate | Fault injection on synthetic data | The out-of-band kill rehearsal at Alpaca |
| 5 Evals | On synthetic data | Gate reports on real trials; the holdout has never been looked at |
| 6 Paper run | The engine process, deployment files | The host, the two clean weeks, the write-up |
| 7 Research agents | The rails | The fork, the token, the signing key, the scheduled tasks |
| 8 UI | The app, headless | A device build; the kill from a phone against paper |

Live comes after all of the right-hand column, in that order. In particular no strategy has
passed a promotion gate, and PLAN.md is explicit that paper or recent live results never
promote one.

## What the code refuses today

These are deliberate, and each must be changed by a reviewed pull request that says why,
not worked around:

- The engine config parser accepts `"environment": "paper"` and nothing else
  (`crates/app/src/config.rs`).
- The engine binary and the kill commands build only Alpaca's paper endpoints
  (`AlpacaConfig::paper`).
- The signer signs only the stage `paper` (`nisse_core::signing::STAGE_PAPER`). The engine
  checks the stage only when `signature.required` is true; `config/engine/paper.json` has
  it false today, so the paper engine checks no signature at all.
- The only place that sends an order is the trader, after the risk gate's check
  (`crates/engine/src/trader.rs`). This is by convention: the gateway's submit functions
  are public, and nothing would stop a new binary calling them directly. A change that
  adds live should close that (make them crate-private or add a test that fails on a
  second caller).
- There is no LLM or HTTP client outside the broker adapter.

## Order of work for going live

1. **Finish the right-hand column above.** The paper run's write-up is the evidence that
   the plumbing is clean; the gate report on real trials is the evidence about the strategy.
2. **Choose the account to fund first.** Both a taxable account and an IRA are to be
   supported (decided 2026-11-28); nothing in the code is specific to either today. Which
   one is funded first is still a choice to make with a tax professional: PLAN.md notes
   short-term gains and wash-sale windows on one side, and that an IRA avoids both. If the
   two are ever run at once, each needs its own engine config, limits file and store rows
   keyed by account, which is unbuilt.
3. **The promotion decision.** `nisse-gate-report` must show every gate except `tracking`
   passing for the champion's strategy version, including the one deliberate look at the
   holdout. If it does not, there is no live phase yet. The tracking gate cannot pass
   before live: the report loads no live returns today (`live: None` in
   `crates/eval/src/gate_report.rs`), so it reports `not_evaluated` and the report exits 3,
   not 0. Two pieces of unbuilt work follow, both needed before step 5:
   - load live returns into the tracking gate, so steps 4 and 7 can be checked by the
     report and not by hand;
   - state the pre-live rule exactly ("every gate passes except `tracking`, which is
     `not_evaluated`") so the `live` signer checks that, and not a loosened exit code.
4. **Pre-declare the tracking band** before the first live order: the backtest replayed
   over the same period, and the band live results must stay inside
   (`tracking_error_max` and `tracking_gap_max` in `config/eval/gates.json`, both 0.05
   today). Written down and committed before any live result exists.
5. **A reviewed change that adds the live environment**, touching sensitive paths
   (the adapter's endpoints, the config parser, the signing stage), with:
   - a `live` stage that the signer refuses unless the gate report passes;
   - `"required": true` for the signature in the live engine config, with no way to run
     live unsigned;
   - limits for the live account in a file outside the repository;
   - the same kill switch, watcher and dead-man check as paper, rehearsed on paper first.
   - a `live` row in `trial_registry` for the live run: only the backtest job registers
     trials today, and the paper engine registers none.
   It is merged under the same rule as the other build phases (decided 2026-11-28): the
   Claude lead may merge it after the independent review and green CI, with every change
   under a sensitive path called out. It is never a change a research agent proposes.
6. **Fund with a small amount**; swing holding periods only; live credentials on the engine
   host and nowhere else.
7. **Monthly review** against the band; zero risk breaches; size doubles no faster than
   about every six months and only while in band.

## Challengers in shadow

The issue asks for "challengers in shadow (decisions logged, no orders)". Two ways to do it
were weighed; the owner chose B on 2026-11-28. Nothing of it is built yet.

- **A. A nightly backtest.** Each evening, run the challenger through the backtest job
  over the bar file up to today and store the last session's decisions. It needs no second
  broker. As the job stands it is refused: an ordinary run whose bars reach
  `holdout_start` (2023-01-01, which may not move later) fails with `HoldoutTouched`, and
  a holdout run is capped at `holdout_max_looks` (10) across all versions, so ten nights
  would use up the holdout for every strategy. It would also add a `backtest` trial a day
  against a `trial_budget` of 100. Making it work means a new kind of run with its own
  rules for both, which is a change to how the holdout and trials are protected.
- **B. A second engine with a simulated broker.** Run `nisse-engine` for the challenger
  with the simulated broker in place of Alpaca, persisting its simulated positions. Its
  decisions are made at the same moment as the champion's and it never reads the holdout
  through the backtest job. The costs: a second set of state to restore and reconcile; a
  simulated fill model that drifts from the champion's real fills; its simulated orders
  and fills must be stored apart from, or clearly marked against, the champion's real
  `order_event` rows (the issue says "no orders"); and it has a trial question too.
  `trial_registry.kind` already has `shadow`, and every row of every kind counts toward
  the lifetime budget, so the choice is one `shadow` trial per challenger version when its
  shadow run starts, or one per session. None is not allowed by CLAUDE.md.

Decided: B, registering one `shadow` trial per challenger version when its shadow run
starts. A's obstacles are the holdout and the budget themselves; B's are bookkeeping.

## The owner's

Everything in "Order of work" that needs an account, money, credentials or time. Nothing in
this repository can trade real money today; changing that takes the pull request described
in step 5.

Nothing here is investment advice.
