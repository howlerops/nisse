# nisse

A personal, self-improving trading system for US equities and ETFs.

A deterministic Rust engine (built on NautilusTrader crates) trades through Alpaca. Claude research agents propose strategy changes as pull requests, never orders. Every decision is stored and scored so strategy versions can be ranked and promoted on statistical evidence.

## Status

**Planning.** No application code yet. See [PLAN.md](PLAN.md) for the build plan and the GitHub issues for per-phase work.

The previous contents of this repo are preserved on the `archive/pre-trading` branch.

## Disclaimer

This is a personal project and not financial advice. Nothing here is a recommendation to buy or sell any security.

## Licence

MIT, see [LICENSE](LICENSE). NautilusTrader is LGPL-3.0; how that interacts with this repo is an open question in PLAN.md.
