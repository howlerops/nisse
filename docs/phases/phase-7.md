# Phase 7: Research agents

Issue: #9. Exit criterion: an agent PR passes CI and is loaded after human merge.

Agents write strategy configs and research notes, never orders, and never touch broker
credentials. Their output is a pull request; a human merges every one; every agent PR is
untrusted input. This phase builds the rails that make that safe to run. It does **not**
start any agent: creating the scheduled tasks, the scoped token and the signing key are the
owner's, and so are three decisions listed at the end.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Where research lives | A `research/` directory in this repository, for now | The public-or-private question is the owner's (open question). Everything an agent may write is under one directory, so moving it to its own repository later is a copy. |
| What an agent may change | Only files under `research/configs/`, `research/notes/` and `research/labels/` | One rule that CI can check from the list of changed paths. |
| Config validation | In Rust (`nisse_eval::proposal`), with the engine's own strategy parser | The JSON Schema file in `research/schema/` documents the format for people and editors; the Rust check is the one CI runs. No schema-validation dependency. |
| Change budget | One new or changed config per PR, and none within 30 days of the newest config of the same `family` | PLAN.md: one change per strategy per month. |
| Signature | HMAC-SHA256 over a manifest, key in the environment (`NISSE_SIGNING_KEY`) | Uses the hash crate already in the workspace. **Limitation:** the key that signs also verifies, so the engine host could sign. A public-key signature (CI holds the private key, the host only the public one) is the better scheme and needs a new dependency: the owner's decision. The code keeps signing behind two functions so the scheme can be swapped. |
| What is signed | The *deployment*: strategy parameters, universe and benchmark (`deployment_hash`), with the config's name, the commit and the stage | The engine computes the same hash from its own config and refuses a mismatch. |
| Stage | `paper` only; this build refuses to sign `live` | Live is Phase 9 and goes through the promotion gates. |
| When CI signs | On push to `main` (after a human merged), never on a pull request | A pull request must not be able to reach the key. |
| Backtest and gates in CI | Not run by hosted CI in this phase | They need the bar files and the trial history, which exist only where the store is. `scripts/evaluate-proposal.sh` runs them there. Where that runs (the engine host by hand, or a self-hosted runner) is the owner's decision. |
| What agents read | A report file of aggregates written by a trusted job (`nisse-agent-report`), not the database | PLAN.md: aggregates and labels only, never raw recent winners or holdout decisions. |

## Research config (`research/configs/<name>.json`)

| Field | Meaning |
|---|---|
| `name` | Equal to the file name without `.json`; lower-case letters, digits and `-` |
| `family` | The strategy line this belongs to (`trend`); the change budget is per family |
| `created` | `YYYY-MM-DD`, the day the proposal was written |
| `parent` | The `name` of the config this changes, or `null` for the first of a family |
| `strategy` | The strategy parameters, exactly what `nisse_engine::strategies::build` takes |
| `symbols` | The universe: upper-case, unique, at least one |
| `benchmark` | One symbol |
| `rationale` | Path of the note, `research/notes/<name>.md` |
| `expected_effect` | One or two sentences, written before any result is seen |

Unknown fields are errors. The note must have the three headings `## Economic rationale`,
`## Expected effect` and `## What would change my mind`, each with text under it. That the
note was written *before* the results cannot be checked by a machine; the agent's
instructions require it and the reviewer reads for it.

## Proposal check (`nisse-proposal-check`)

Run by CI on every pull request, with the changed paths (`git diff --name-status`) on
standard input, the base checkout and the head checkout as directories. A pull request that
changes nothing under `research/` passes untouched. One that does must satisfy all of:

1. Every changed path is under `research/configs/`, `research/notes/` or `research/labels/`.
   (An agent PR that also edits code, CI or limits fails here.)
2. Nothing under `research/` is deleted or renamed.
3. At most one file under `research/configs/` is added or modified.
4. Every config in the head checkout parses and validates, and its note exists and has the
   three headings.
5. For the added or modified config: `created` is not in the future and not before the
   newest `created` of its family at the base plus 30 days; a `parent` names a config that
   exists at the base in the same family; a modified config keeps its `name` and `family`.

Exit 0 pass, 1 fail (each failure on its own line), 2 bad arguments.

## Signing (`nisse-sign`) and verification

`deployment_hash` is the SHA-256 of the canonical JSON of
`{"benchmark": .., "strategy": .., "symbols": [sorted]}`.

The manifest (`research/signed/<name>.manifest.json`):

```json
{"name": "trend-v1", "deployment_hash": "<64 hex>", "git_sha": "<40 hex>",
 "stage": "paper", "signed_at": "2026-11-27T00:00:00Z", "mac": "<64 hex>"}
```

`mac` is HMAC-SHA256 with the key over the canonical JSON of the manifest without `mac`.
The key is at least 32 bytes, given as hex in `NISSE_SIGNING_KEY`; it is never printed.

The engine config gains `"signature": {"manifest": "<path>", "required": true|false}`.
Before `tick` or `session-close` does anything, the engine computes the deployment hash of
its own config and

- with `required: true`: the manifest must exist, its MAC must verify, its hash must equal
  the engine's, and its stage must be `paper`; otherwise the command exits 2 and nothing
  is traded;
- with `required: false`: it prints that the deployment is unsigned and carries on. This is
  the setting in `config/engine/paper.json` until the owner has created a key.

`kill`, `release-kill` and `status` never depend on the signature.

## Agent report (`nisse-agent-report`)

Run nightly on the host by a job that can read the store; writes one JSON file. It holds
only:

- per strategy version: the number of decisions agents may see, by action; for each score
  metric, the count, mean and median over those decisions, **only when the count is at
  least 30** (smaller groups are reported as `"suppressed"`);
- the gate report's status per gate (pass, fail, not evaluated) per version;
- data checks on the newest bar file: its name, the last session per symbol, symbols with
  no bar on the last session, zero-volume bars and one-day moves over 25% in the last 20
  sessions;
- the current regime of the benchmark;
- a label queue: at most 20 visible decisions with what was decided and how it was executed
  and **no outcome** (no forward return, no P&L), for the post-mortem labels.

Decisions hidden by the holdout are never counted or listed. No date is finer than a month
except in the data checks.

## The three runs (`research/agents/*.md`)

The instructions each scheduled task is given. They are text in this repository so that a
change to them is a reviewed pull request.

| Run | Reads | Writes (as a PR) |
|---|---|---|
| Pre-market | the agent report's data checks and regime | `research/notes/daily/<date>-pre.md`: flags, regime note |
| Post-close | the agent report | `research/labels/<date>.json` (labels: bad luck, bad decision, bad execution, data error; blind to outcome) and `research/notes/daily/<date>-post.md` |
| Overnight research | the agent report, the notes | at most one config proposal with its note |

## Tasks

| Task | What | Files | Order |
|---|---|---|---|
| F | Foundation (lead): this doc, `research/` skeleton, stubs | many | 0 |
| A1 | HMAC, deployment hash, manifest | `crates/core/src/signing.rs` | 1 |
| A2 | Research config, note and proposal checks, `nisse-proposal-check` | `crates/eval/src/proposal.rs`, bin | 1 |
| A4 | Agent report | `crates/eval/src/agent_report.rs`, bin, tests | 1 |
| A3 | `nisse-sign`; engine verification and the `signature` config field | `crates/eval/src/bin/nisse-sign.rs`, `crates/app/src/config.rs`, `crates/app/src/deployment.rs`, binary | 2 (A1) |
| A5 | CI jobs, agent instructions, runbook (lead) | `.github/workflows/`, `research/agents/`, `docs/runbooks/research-agents.md` | 3 |

## The owner's

- Research in this repository or a private one (open question).
- Public-key signatures: needs one new dependency (an Ed25519 crate).
- Where the backtest and gates run for a proposal.
- Creating the signing key, the token scoped to the research repository, and the three
  scheduled tasks; re-reading Anthropic's terms before moving to an API key.
