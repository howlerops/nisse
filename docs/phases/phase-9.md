# Phase 9: Live, small

Issue: #11. Exit criterion: three months inside the tracking band before any size increase.

**Nothing in this phase has been built, and nothing here enables live trading.** Going live
means funding an account and putting live broker credentials on the engine host: both are
the owner's, and so is the decision to do it. This document is what the build sessions can
responsibly contribute first: where the system stands, what the code refuses today and
must be changed deliberately, the order of work, and the decisions that are open.

## Where the system stands (2026-11-27)

Every earlier phase's code is merged. None of their real-world exit criteria has been met,
because each needs credentials, data or time that only the owner has:

| Phase | Built and tested | Not yet done |
|---|---|---|
| 1 Adapter | Against a mock server | The paper round-trip tests (`docs/runbooks/paper-tests.md`) |
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

These are deliberate, each tested, and each must be changed by a reviewed pull request that
says why, not worked around:

- The engine config parser accepts `"environment": "paper"` and nothing else
  (`crates/app/src/config.rs`).
- The engine binary and the kill commands build only Alpaca's paper endpoints
  (`AlpacaConfig::paper`).
- The signer signs, and the engine accepts, only the stage `paper`
  (`nisse_core::signing::STAGE_PAPER`).
- There is no way to place an order except through the risk gate, and no LLM anywhere in
  the order path.

## Order of work for going live

1. **Finish the right-hand column above.** The paper run's write-up is the evidence that
   the plumbing is clean; the gate report on real trials is the evidence about the strategy.
2. **Decide the account**: taxable or IRA (check with a tax professional; PLAN.md notes
   short-term gains and wash-sale windows on one side, and that an IRA avoids both).
3. **The promotion decision.** `nisse-gate-report` must show every gate passing for the
   champion's strategy version, including the one deliberate look at the holdout. If it
   does not, there is no live phase yet.
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
   A human merges it. It is not a change an agent proposes.
6. **Fund with a small amount**; swing holding periods only; live credentials on the engine
   host and nowhere else.
7. **Monthly review** against the band; zero risk breaches; size doubles no faster than
   about every six months and only while in band.

## Open design question: challengers in shadow

The issue asks for "challengers in shadow (decisions logged, no orders)". Two ways to do it,
with a real trade-off, so it is the owner's call before anything is built:

- **A. Replay.** Each evening, run the challenger through the backtest engine over the bar
  file up to today and store the last session's decisions under the challenger's strategy
  version. It reuses the path the backtest and live already share and needs no second
  broker. But CLAUDE.md says every backtest or evaluation run registers a trial and the
  count never resets: a nightly replay of one unchanged config would either add a trial a
  day (wrecking the deflated Sharpe, which counts trials) or need a rule that a re-run of
  an already-registered config over an extended period is the *same* trial. That rule
  would be a change to how trials are counted.
- **B. A second engine with a simulated broker.** Run `nisse-engine` for the challenger
  with the simulated broker in place of Alpaca, persisting its simulated positions. No
  trial question, and its decisions are made at the same moment as the champion's; but it
  is a second set of state to restore and reconcile, and a simulated fill model that
  drifts from the champion's real fills.

Recommendation: B, because it leaves the trial count alone, which the promotion gates
depend on.

## The owner's

Everything in "Order of work", and the shadow decision. Nothing in this repository will
start trading real money without a pull request that a person wrote the reasons for and
merged.

Nothing here is investment advice.
