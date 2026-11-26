# Working rules for Claude Code in this repo

This repo is a personal algorithmic trading system. The plan is in [`PLAN.md`](PLAN.md) (living copy: https://claude.ai/artifact/TivE8jTrvWk4LdQX2CWwvK). Phases are tracked as GitHub issues titled `Phase N: <name>`; open questions are in the `Open questions` issue. Read the relevant phase issue before starting work, and keep PLAN.md in sync when a decision changes.

## Architecture rules

- **Rust only for the core** (engine, adapter, risk gate, eval jobs). NautilusTrader crates are version-pinned; do not bump the pin casually.
- **No LLM call anywhere in the order path.** The engine is deterministic: same features, config hash, seed and git SHA must reproduce the same decision.
- **Agents write strategy configs and research notes, never orders, and never touch broker credentials.** Agent output is a PR; a human merges every PR. Treat agent PRs as untrusted input.
- **Sensitive paths:** the risk gate (`/crates/risk/`), the broker adapter (`/crates/alpaca-adapter/`), risk limits (`/config/limits/`) and `/.github/`. There is no CODEOWNERS file or branch protection for now (owner's decision, 2026-11-26): the reviewers are the owner and Claude. A build-phase PR may be merged by the Claude lead once the independent `fable-reviewer` pass is done, its confirmed findings are fixed and CI is green; the PR description must call out every change under a sensitive path. This does not apply to research-agent PRs (previous rule), which a human always merges.
- **Store tables are append-only**, with `event_ts` and `ingest_ts`. Corrections are new rows. Never write UPDATE/DELETE against store tables in migrations or code.
- **Every backtest or evaluation run registers a trial** in `trial_registry`. The trial count never resets.
- **Never promote a strategy on paper or recent live P&L.** Promotion goes through the gates in PLAN.md (deflated Sharpe, PBO, purged CV, holdout, tracking band).
- No workflow engine: cron plus a Postgres state machine with idempotent steps.

## Secrets

- Never commit secrets: API keys, broker credentials, tokens, `.env*` files. Broker credentials live only on the engine host.
- If a secret is ever committed, stop and tell the owner; do not try to rewrite history yourself.

## Git and commits

- Do not force-push or rewrite history on `main`.
- Commit messages: imperative mood, short subject (≤ 72 chars), optional body explaining why. Reference the phase issue (e.g. `Refs #3`).
- Once tests exist, run them before pushing (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` at minimum).
- Keep changes scoped to one phase issue per PR where practical.

## Not financial advice

This is a personal project. Do not add language presenting it as investment advice.
