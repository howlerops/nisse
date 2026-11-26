# nisse

A personal, self-improving trading system for US equities and ETFs.

A deterministic Rust engine (built on NautilusTrader crates) trades through Alpaca. Claude research agents propose strategy changes as pull requests, never orders. Every decision is stored and scored so strategy versions can be ranked and promoted on statistical evidence.

## Status

**Phase 1 (Alpaca adapter).** The workspace, CI, store schema and the Alpaca adapter exist, tested against a local mock server. The adapter's paper round-trip tests are run by the owner (see [`docs/runbooks/paper-tests.md`](docs/runbooks/paper-tests.md)). There is no strategy yet. See [PLAN.md](PLAN.md) for the build plan and the GitHub issues for per-phase work.

## Layout

| Path | What |
|---|---|
| `crates/core` | Shared domain types; re-exports the NautilusTrader types the workspace uses |
| `crates/engine` | Deterministic engine (strategy, portfolio, order state) |
| `crates/risk` | Risk gate and kill switch |
| `crates/alpaca-adapter` | Alpaca data and execution adapter |
| `crates/store` | Append-only Postgres decision store |
| `crates/eval` | Nightly evaluation jobs and promotion gates |
| `migrations/` | Store schema as plain SQL, with tests in `migrations/tests/` |

## Development

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on first use.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

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
