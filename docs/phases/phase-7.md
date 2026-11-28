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
| Change budget | One new or changed config per PR, and none within 30 days of the newest config of the same `family` or the same strategy kind | PLAN.md: one change per strategy per month. A new family name must not reset the budget. |
| Signature | Ed25519 over a manifest (`ed25519-dalek`, approved by the owner 2026-11-28). The signing workflow holds the secret key (`NISSE_SIGNING_KEY`); the engine host holds only the public key (`NISSE_VERIFY_KEY`) | The host can check a manifest but cannot make one. This replaced the first version, a shared-key HMAC, whose key both signed and verified. |
| What is signed | The *deployment*: strategy parameters, universe and benchmark (`deployment_hash`), with the config's name, the commit and the stage | The engine computes the same hash from its own config and refuses a mismatch. |
| Stage | `paper` only; this build refuses to sign `live` | Live is Phase 9 and goes through the promotion gates. |
| When CI signs | Only when a person starts the signing workflow on `main` and approves its run (the `signing` environment has a required reviewer) | The job runs the code on `main` with the key in its environment. There is no branch protection, so a push to `main` proves nothing by itself. |
| Who judges a pull request | A workflow on `pull_request_target`: its file and the checker both come from the base branch, and the pull request's tree is only read, never built or run | A pull request must not be able to edit its own judge. |
| Where agents push | A fork; their token cannot write to this repository | Without branch protection, write access to this repository is write access to `main`. |
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

Run by the "Research proposal" workflow, for pull requests into `main` only. The workflow
first computes **the result of merging the pull request into the base** (`git merge-tree`;
a conflict fails) and judges that: the changed paths are the difference between the base and
the merged tree, NUL-separated so that no file name can disguise a path, and the checker
reads the merged tree, written out file by file (not as a `git archive`, which obeys
`export-ignore` attributes inside the tree and so could hide a file from its judge). The
base is the tip of `main` at the time the job runs. (A branch's own diff from a merge base can hide that the branch
undoes newer commits.) For an agent pull request the workflow also requires every added or
changed entry to be an ordinary file (mode 100644): no symbolic link, executable or
submodule anywhere.

A pull request from a fork, or from a branch named `research/...`, is an **agent pull
request** (`--agent`): it must change research files and nothing else, and all the rules
below apply. Any other pull request is a person's own: rules 1 and 2 do not bind it (it may
change code beside research files, and reorganise them), but if it touches `research/` it
is held to rules 3 to 5, so the change budget applies to people too. If it touches nothing
under `research/`, nothing is checked and the workflow says so.

1. Every changed path is a plain path (letters, digits, `.`, `_`, `-`, `/`; no component
   starting with a dot, so no `..` and no hidden files) under
   `research/configs/`, `research/notes/` or `research/labels/`. (An agent PR that also edits
   code, CI, limits, `research/agents/` or `research/signed/` fails here.)
2. Nothing under `research/` is deleted, renamed or copied.
3. At most one file under `research/configs/` is added or modified.
4. Every config in the head checkout is a regular file (not a symbolic link), parses and
   validates, and its note is a regular file with the three headings.
5. For the added or modified config: `created` is not in the future, not more than 7 days
   old (the budget is counted in these dates, so a proposal carries the date it is made),
   and not before the
   newest `created` at the base, among configs of its family or of its strategy kind, plus
   30 days; a `parent` names a config that exists at the base in the same family; a modified
   config keeps its `name` and `family`.

A green check is a judgement at one moment against `main` as it was then. It is not re-run
when `main` moves: two proposals can each pass against the same base, and a pull request
left open keeps its old result. So (a) the reviewer re-runs the check just before merging
(re-running the job judges against today's `main`; the job prints the commit it judged
against), and (b) the signer checks the budget again over the configs on `main` and refuses
a config dated fewer than 30 days after another of its family or kind: the later of a pair,
so the earlier one stays signable. Two with the same date are both refused until one is
re-dated by a later change.

Exit 0 pass, 1 fail (each failure on its own line), 2 bad arguments.

## Signing (`nisse-sign`) and verification

`deployment_hash` is the SHA-256 of the canonical JSON of
`{"benchmark": .., "strategy": .., "symbols": [sorted]}`.

The manifest (`research/signed/<name>.manifest.json`):

```json
{"name": "trend-v1", "deployment_hash": "<64 hex>", "git_sha": "<40 hex>",
 "stage": "paper", "signed_at": "2026-11-27T00:00:00Z", "signature": "<128 hex>"}
```

`signature` is the Ed25519 signature over the canonical JSON of the manifest without
`signature`. The secret key is 32 bytes, given as hex in `NISSE_SIGNING_KEY` to the signer
only; it is never printed. `nisse-sign --public-key` prints the matching public key, which
the engine reads from `NISSE_VERIFY_KEY`. Verification is strict (non-canonical signatures
and weak keys are refused).

The engine config gains `"signature": {"manifest": "<path>", "required": true|false}`.
The check is made by `tick` and `session-close` themselves, not only by the binary. A
refusal stops the command before it reconciles: see the runbook for what that means for
positions already held; whether an unsigned engine should still reconcile and reduce is the
owner's call.

Before `tick` or `session-close` does anything, the engine computes the deployment hash of
its own config and

- with `required: true`: the manifest must exist, its signature must verify, its hash must equal
  the engine's, and its stage must be `paper`; otherwise the command exits 2 and nothing
  is traded;
- with `required: false`: it prints that the deployment is unsigned and carries on. This is
  the setting in `config/engine/paper.json` until the owner has created a key.

`kill`, `release-kill` and `status` never depend on the signature.

## Agent report (`nisse-agent-report`)

Run nightly on the host by a job that can read the store; writes one JSON file. It holds
only:

- per strategy version: the number of decisions agents may see, by action; for each score
  metric, the count, mean and median over those decisions **in cohorts of 30**: the oldest
  30, 60, 90, ... of them, so the figures change only when 30 more have accrued, rounded to
  four significant digits (fewer than 30 is reported as `"suppressed"`). A report that
  changed with every decision would let the newest decision's outcome be worked out by
  subtracting yesterday's figures from today's;
- the gate report's status per gate (pass, fail, not evaluated) per version, **except the
  holdout gate**, whose result would be feedback on the holdout;
- data checks on the newest bar file: its name, the last session per symbol, symbols with
  no bar on the last session, zero-volume bars and one-day moves over 25% in the last 20
  sessions;
- the current regime of the benchmark;
- a label queue: at most 20 visible decisions with what was decided and how it was executed
  and **no outcome** (no forward return, no P&L), for the post-mortem labels.

Known limit of the cohort rule: a cohort is the oldest 30, 60, ... visible decisions *that
have the score*. If a score for an older decision arrives late, or a decision is re-scored
by a new evaluation version, a published figure can move without 30 new decisions, and the
change then reflects fewer than 30 values. This is likeliest for scores that exist only
once a position is closed, which arrive in closing order, not decision order. Rounding
blunts it but does not remove it; freezing a cohort once published would close it and needs a
place to keep what was published (a new table: the owner's call).

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
| A1 | Signature, deployment hash, manifest | `crates/core/src/signing.rs` | 1 |
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
