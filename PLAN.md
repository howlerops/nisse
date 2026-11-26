# Nisse build plan

> Living plan doc: https://claude.ai/artifact/TivE8jTrvWk4LdQX2CWwvK
> This file is the in-repo copy. Phases are tracked as GitHub issues (`Phase 0` … `Phase 9`, plus `Open questions`).

**Not financial advice.** This is a personal project.

## Summary

A personal, self-improving trading system for US equities and ETFs: a deterministic Rust engine trades through Alpaca, Claude research agents propose strategy changes as pull requests, and every decision is stored and scored so strategy versions can be ranked and promoted on evidence.

Realistic good outcome: net Sharpe 0.3 to 0.6 with long stretches behind SPY. At this account size the main return is learning.

### Decisions made

| Area | Decision |
|---|---|
| Broker | Alpaca for paper and live (real order API, free paper environment on the same API, fractional shares, $0 stock commissions). |
| Engine | NautilusTrader Rust crates, pinned at exactly `0.68.0` (released 2026-11-10; needs Rust 1.99.1, pinned in `rust-toolchain.toml`). Same engine for backtest and live. Rust API still changes between releases. NautilusTrader is LGPL-3.0-only; this repo's own code is MIT. |
| Language | Rust only for the core. |
| Adapter | Write our own Alpaca adapter (Alpaca is not an official Nautilus adapter; community ones are alpha). |
| Agents | Claude Code cloud scheduled tasks on the subscription, no API token for now; hourly at most; output is PRs. |
| Agent scope | Code and configs, never orders. |
| Store | Postgres, plus Parquet read with DuckDB for bars. No ClickHouse, Kafka or feature store. |
| UI | [Native Shell](https://nsh.dev), a Rust-first UI framework where a TSX app targets desktop, mobile, web and terminal and imports Rust modules. Currently v0.7.1. |
| First strategy | Monthly cross-asset ETF trend or dual momentum with vol targeting. |

## Architecture

Two loops that meet only at a human-approved merge.

- **Research loop:** Claude scheduled tasks → research repo (config-only PRs) → CI gates (backtest, then sign) → human merge → signed config loaded by the engine.
- **Trading loop:** Rust engine (strategy, risk gate) → Alpaca (orders); engine → Postgres store (decisions, fills); UI reads the store and can trigger the kill switch; agents receive aggregates only from the store.

### Components

| Component | Implementation | Responsibility |
|---|---|---|
| Engine | Rust, NautilusTrader crates | Strategy, portfolio, order state; backtest and live on one code path |
| Alpaca adapter | Rust | Market data, orders, fills, reconciliation |
| Risk gate | Rust, in the engine process | Limits, drawdown step-down, kill switch |
| Decision store | Postgres; Parquet + DuckDB for bars | Decisions, snapshots, orders, fills, scores |
| Eval jobs | Rust or SQL, nightly | Outcome windows, shortfall, rollups, gate reports |
| Research agents | Claude Code scheduled tasks | Data checks, post-mortem labels, strategy proposals as PRs |
| UI | Native Shell | Positions, eval rankings, PR approvals, kill switch |

### Boundaries

- Broker credentials only on the engine host.
- Engine is a single instance on one small VM, separate from anything agents touch.
- No workflow engine: cron plus a Postgres state machine with idempotent steps.
- No LLM call anywhere in the order path.

### UI notes (Phase 8)

- Take inspiration from [herdr-app](https://github.com/herdrdev/herdr-app), an app built with Native Shell, in particular its ability to open an agent that edits the app in real time, per user.
- If nisse gets such an in-app agent, the existing boundaries still hold: it edits UI code only, never the engine, risk gate, adapter or limits; it never sees broker credentials; and it cannot place orders or change what the kill switch does. Anything it changes outside a user's own UI session still lands as a PR that a human merges.

## Trade store and evals

Store every decision, not just every fill. Rank strategy versions, not individual trades (single-trade P&L is mostly luck). All tables are append-only with `event_ts` and `ingest_ts`; corrections are new rows. A trigger on every table rejects UPDATE, DELETE and TRUNCATE; a table owner or superuser can still disable it, so from Phase 2 the engine connects as a role with INSERT and SELECT only. The schema is in `migrations/`, applied by `scripts/migrate.sh`.

### Tables

| Table | Columns |
|---|---|
| `strategy_version` | id, git_sha, config_hash, params, parent_id |
| `trial_registry` | strategy_version_id, kind [backtest \| shadow \| live], data_range, result |
| `decision` | event_ts, strategy_version_id, symbol, action [enter \| exit \| hold \| skip], intended_qty, expected_edge_bps, expected_vol, features, feature_hash, data_snapshot_id, rng_seed, reason_code |
| `market_snapshot` | event_ts, symbol, bid, ask, mid, spread_bps, adv, vol, regime_tag |
| `order_event` | decision_id, client_order_id, broker_order_id, event_ts, state, type, limit_px, qty |
| `fill` | client_order_id, broker_fill_id (unique), event_ts, px, qty, fee |
| `position_event` | event_ts, symbol, qty_after, avg_cost |
| `outcome_window` | decision_id, horizon, fwd_ret, bench_ret, mfe, mae |
| `eval_score` | subject_id, subject_type, metric, value, eval_version |
| `risk_check` | decision_id, rule, limit_value, actual_value, passed |

Every table also has `id` and `ingest_ts`. `order_event.client_order_id` is written with the intent row before submission, when `broker_order_id` is still unknown; fills refer to their order by `client_order_id`. Column types and constraints are in [`docs/phases/phase-0.md`](docs/phases/phase-0.md).

**Nightly replay test:** rerunning a decision from its features, config hash and seed on the recorded git SHA must reproduce the same action.

### Trade-level scores

| Score | Definition | Use |
|---|---|---|
| Decision quality | Benchmark-adjusted forward return at horizons fixed before the trade, divided by expected vol; scored for skips too | Optimize in aggregate only |
| Execution | Implementation shortfall in bps vs arrival mid, plus fees and missed fills | Safe to optimize |
| Exit | MFE/MAE in R units; capture = realized / MFE | Diagnostic |
| Risk adherence | Pass/fail per rule | Hard gate |
| Outcome | Realized R, P&L | Diagnostic only |

### Strategy-level rollup (per version)

Expectancy, Sharpe and Sortino with bootstrap intervals, max drawdown, deflated Sharpe using the trial count, each split by regime, plus live-vs-backtest tracking error.

### Tuning rules

- No parameter change under about 100 decisions per strategy or 30 per regime bucket.
- A random 20 to 30% of live decisions held out and never shown to agents.
- One change per strategy per month, each registering a trial and stating expected effect first.
- Agents see aggregates and labels, never raw recent winners.
- An LLM judge may only label post-mortems (bad luck, bad decision, bad execution, data error) blind to outcome; never numeric scores or promotion decisions.

## Validation and promotion gates

Promote on statistical evidence across the whole trial history, never on paper or recent live P&L (a true Sharpe of 1 needs about four years to reach t = 2). Thresholds are reviewer judgment grounded in Harvey & Liu and Bailey & López de Prado.

| Gate | Threshold |
|---|---|
| Trial registry | Logs every config ever evaluated; count never resets |
| Lifetime trial budget | About 50 to 100 |
| Deflated Sharpe ratio | ≥ 0.95 using lifetime trial count |
| Probability of backtest overfitting | ≤ 0.2 (CSCV, 16 splits) |
| Cross-validation | Purged k-fold with embargo ≥ max holding period |
| Holdout | Queried once per candidate, logged as a trial, retired after about 10 looks |
| Backtest Sharpe haircut | ≥ 50% before sizing |
| Paper stage | Plumbing test only |
| Shadow stage | Challenger logs decisions without orders alongside champion |
| Live tracking | Live results stay inside a pre-declared band around the backtest replayed over the same period |
| Risk breaches | Zero |

### LLM rules

- Agents propose code and configs only, never per-date decisions.
- Any signal with a model in the loop is evaluated only on data after that model's knowledge cutoff.
- Each proposal states its economic rationale before seeing results.
- Synthetic data is for engine correctness, failure injection and stress paths only.

## Research agents

### Runs

| Run | Output |
|---|---|
| Pre-market | Data checks, regime note, flags |
| Post-close | Eval rollup, post-mortem labels, tracking-error report |
| Overnight research | At most one strategy proposal PR within the change budget |

### Constraints

- Scheduled tasks fire hourly at most and draw on normal subscription limits.
- Anthropic's Consumer Terms (section 3) bar relying on the service to buy or sell securities and bar automated access except by API key or where explicitly permitted. Subscription-run trading research sits close to that clause; accepted for now. Fallback is an API key (Max plans include $100/month API credit).
- Add a Fly.io Sprite only if research needs a persistent disk.

### Security

- Agents get a GitHub token scoped to the research repo only.
- Treat every agent PR as untrusted input (agents read untrusted web content).
- PRs contain schema-validated configs and notes.
- Risk gate, adapter and limits are sensitive paths. No CODEOWNERS or branch protection for now; reviewers are the owner and Claude (see CLAUDE.md).
- CI runs schema checks, backtest and gates, then signs the artifact; the engine verifies the signature before loading.
- Risk limits live outside the agent-writable repo.
- A human merges every PR.

## Risk controls and capital ramp

### Controls

- Portfolio vol target 8 to 10% annualized.
- At most quarter-Kelly on the haircut Sharpe.
- At most 0.5% of equity at risk per trade.
- Halve exposure at 10% drawdown; flat at 15 to 20%; re-enter only through the gates.
- Stale-data watchdog halts new entries.
- Kill switch fails closed (cancel all, flatten optional) and is reachable from the phone UI.
- An out-of-band way to disable trading at Alpaca.
- Dead-man heartbeat alert.

### Execution correctness

- Client order IDs persisted before submission in a write-ahead intent log.
- Reconcile positions and orders against Alpaca on startup and on a timer (broker is source of truth).
- Partial fills handled by cumulative quantity.
- Exchange calendar with half-days.

### Ramp

1. Paper on Alpaca until plumbing is clean.
2. Live with a small amount, swing holding periods only.
3. Double size no faster than about every six months, and only while tracking error stays in band.

Intraday is out of scope unless a gated strategy needs it. The day-trade limit stays a config value (brokers may differ on the post-PDT rules until 2027-10-20). Tax notes to check with a professional: short-term gains, wash-sale windows; an IRA avoids both.

## Milestones

| Phase | Name | Build | Exit criterion |
|---|---|---|---|
| 0 | Scaffold | Repo, Rust workspace, pinned NautilusTrader crates, CI, Postgres migrations for the store tables | CI green; migrations apply cleanly |
| 1 | Alpaca adapter | Data and execution clients for Alpaca paper (bars, quotes, orders, fills, reconciliation) | Order round-trip and restart reconciliation pass against paper |
| 2 | Decision store | Engine actor writing decisions (including skips), snapshots, orders, fills, risk checks | Nightly replay test reproduces every decision |
| 3 | First strategy and backtest | Monthly ETF trend or dual momentum with vol targeting; Parquet bars; trial registry | Backtest runs through the same engine as live; every run registers a trial |
| 4 | Risk gate and kill switch | Limits file, drawdown step-down, stale-data watchdog, heartbeat | Fault-injection tests pass on synthetic data |
| 5 | Evals | Outcome windows, implementation shortfall, strategy rollups, deflated Sharpe and PBO gates | Gate report generated per strategy version |
| 6 | Paper run | Engine on one small VM against Alpaca paper | Two clean weeks, no reconciliation breaks, slippage within model |
| 7 | Research agents | Scheduled tasks, research repo, config schema, CI signing | An agent PR passes CI and is loaded after human merge |
| 8 | UI | Native Shell app (positions, eval rankings, PR approvals, kill switch) | Kill switch works from the phone |
| 9 | Live, small | Fund the live account; champion only; challengers in shadow | Three months inside the tracking band before any size increase |

## Open questions

- [ ] Market data: free IEX feed or the $99/month SIP plan; decide before backtesting.
- [ ] Whether Alpaca fractional orders can be limit orders (Alpaca docs disagree); test in paper.
- [x] NautilusTrader pinned at `0.68.0` (Phase 0). Licence decided 2026-11-26: this repo's own code is MIT. NautilusTrader stays a separate `LGPL-3.0-only` dependency that this repo does not relicense; its terms apply to that library if a built binary is ever given to someone else.
- [ ] Confirm Native Shell can read Postgres through a Rust module before building screens.
- [ ] Whether Anthropic's Commercial Terms or Usage Policy carry a securities clause like the Consumer Terms; read before moving agents to an API key.
- [ ] Taxable account or IRA.
- [ ] Alpaca reliability (repeated incidents in 2026): keep our own ledger and reconcile from fills.
- [ ] This repo is public: decide whether strategy configs and research notes should live in a separate private repo.

## Sources

- https://docs.alpaca.markets/docs/trading-api
- https://docs.alpaca.markets/docs/paper-trading
- https://docs.alpaca.markets/docs/about-market-data-api
- https://status.alpaca.markets/
- https://nautilustrader.io/docs/latest/concepts/rust/
- https://github.com/nautechsystems/nautilus_trader/issues/3374
- https://nsh.dev
- https://code.claude.com/docs/en/web-scheduled-tasks
- https://www.anthropic.com/legal/consumer-terms
- https://www.finra.org/rules-guidance/notices/26-10
- Harvey & Liu, *Evaluating Trading Strategies*: https://people.duke.edu/~charvey/Research/Published_Papers/P116_Evaluating_trading_strategies.pdf
- https://papers.ssrn.com/abstract=2460551
- https://papers.ssrn.com/abstract=2326253
- https://arxiv.org/html/2510.07920v1
- https://arxiv.org/html/2505.07078v3
