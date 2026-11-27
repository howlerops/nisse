# nisse

A personal, self-improving trading system for US equities and ETFs.

A deterministic Rust engine (built on NautilusTrader crates) trades through Alpaca. Claude research agents propose strategy changes as pull requests, never orders. Every decision is stored and scored so strategy versions can be ranked and promoted on statistical evidence.

## Status

**Phase 7 (research agents), the rails.** The workspace, CI, store schema, Alpaca adapter, decision store, replay job, a first strategy (monthly ETF trend with volatility targeting; rationale in [`docs/research/trend-v1-rationale.md`](docs/research/trend-v1-rationale.md)), a backtest that runs through the same trading loop as live, the risk gate, and the evaluation jobs exist. Every order passes eight recorded checks under the limits in `config/limits/limits.json` (see [`docs/phases/phase-4.md`](docs/phases/phase-4.md)). Every backtest registers a trial and stores its daily returns. `nisse-score` scores decisions nightly (outcome windows, execution, exits) and `nisse-gate-report` states, per strategy version, whether each promotion gate passes, fails or could not be evaluated, against the thresholds in `config/eval/gates.json` (see [`docs/phases/phase-5.md`](docs/phases/phase-5.md)); it promotes nothing. The adapter is tested against a local mock server and the store against a real Postgres; the adapter's paper round-trip tests and the bar download (`nisse-ingest`) need Alpaca credentials and are run by the owner (see [`docs/runbooks/paper-tests.md`](docs/runbooks/paper-tests.md)). How to stop trading is in [`docs/runbooks/kill-switch.md`](docs/runbooks/kill-switch.md). The engine process is `nisse-engine`: short commands run from cron (`tick`, `session-close`, `kill`, `release-kill`, `status`) that rebuild their state from the store and the bar files each time and only talk to Alpaca's paper endpoints (see [`docs/phases/phase-6.md`](docs/phases/phase-6.md)). Putting it on a host with paper credentials and running it for two weeks is the owner's part: [`docs/runbooks/paper-run.md`](docs/runbooks/paper-run.md). Research agents have rails but are not running: everything an agent may write is under [`research/`](research/README.md), a pull request that touches it is checked by `nisse-proposal-check` (nothing else changed, one config, a 30-day budget per strategy family), a merged config is signed by `nisse-sign`, the engine refuses an unsigned deployment once its config requires a signature, and agents read an aggregates-only report (`nisse-agent-report`), never the database (see [`docs/phases/phase-7.md`](docs/phases/phase-7.md)). Turning the agents on is the owner's: [`docs/runbooks/research-agents.md`](docs/runbooks/research-agents.md). No backtest on real data has been run yet and the engine has not been run against Alpaca. No agent has been started. See [PLAN.md](PLAN.md) for the build plan and the GitHub issues for per-phase work.

## Layout

| Path | What |
|---|---|
| `crates/core` | Shared domain types; re-exports the NautilusTrader types the workspace uses |
| `crates/engine` | Deterministic engine (strategy, portfolio, order state) |
| `crates/risk` | Risk gate and kill switch |
| `crates/alpaca-adapter` | Alpaca data and execution adapter |
| `crates/store` | Append-only Postgres decision store |
| `crates/bars` | Daily bars in Parquet files, written and read through DuckDB |
| `crates/eval` | Replay, backtest and nightly scoring jobs; the statistics and the promotion gate report |
| `config/limits/` | The reviewed risk limits file |
| `config/eval/` | The reviewed promotion gate thresholds |
| `migrations/` | Store schema as plain SQL, with tests in `migrations/tests/` |

## Development

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on first use.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Store tests need a Postgres server. In a dev container with Postgres 16 installed:
eval "$(scripts/dev-postgres.sh)"   # starts it if needed, exports DATABASE_URL and TEST_DATABASE_URL
cargo test --workspace --all-features

# Apply the schema to a scratch database and run the schema tests (needs psql and CREATEDB):
DATABASE_URL=postgres://localhost/postgres scripts/test-migrations.sh
# Apply the schema to a database:
DATABASE_URL=postgres://localhost/nisse scripts/migrate.sh
```

The previous contents of this repo are preserved on the `archive/pre-trading` branch.

## Disclaimer

This is a personal project and not financial advice. Nothing here is a recommendation to buy or sell any security.

## Licence

MIT, see [LICENSE](LICENSE). The NautilusTrader crates this project depends on are a separate LGPL-3.0-only library and keep their own licence.
