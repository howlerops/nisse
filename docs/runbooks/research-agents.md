# Research agents

How the owner turns the research loop on. Nothing here is done by an agent or by CI, and
nothing in this repository starts an agent on its own.

## What exists

- `research/`: the only directory an agent may write (configs, notes, labels).
- `research/agents/*.md`: the instructions for the three runs.
- `nisse-proposal-check`: the CI check every research pull request must pass.
- `nisse-agent-report`: writes the aggregates-only report agents read.
- `nisse-sign` and the engine's signature check: a merged config is signed (by a workflow you
  start and approve); an engine whose config says `"required": true` trades nothing without
  a valid signature.

## Decide first

1. **Where research lives.** It is in this (public) repository for now. To move it, create
   the private repository, copy `research/` and the proposal-check job, and point the
   agents' token at it.
2. **Signature scheme.** The built one is a shared-key MAC: whoever can verify can also
   sign. A public-key scheme needs one new dependency; say if you want it.
3. **Where a proposal is evaluated.** The backtest and gates need the bar files and the
   store, so hosted CI cannot run them. `scripts/evaluate-proposal.sh` runs them on the host.
4. **Terms.** PLAN.md records that subscription-run research sits close to a clause in
   Anthropic's Consumer Terms. Re-read them, and the Commercial Terms, before moving to an
   API key.

## Turn it on

1. **Agents work from a fork, never in this repository.** This repository has no branch
   protection (CLAUDE.md), so any token that can write to it can push to `main`. Create a
   fork under a separate account for the agents. Their token (step 5) can write to the
   fork only. A pull request from a fork is treated as an agent pull request by CI, with the
   strictest rules. If research moves to its own repository, the same holds there.
2. **Signing key.** Generate 32 random bytes as hex (`openssl rand -hex 32`). Put it in this
   repository's Actions secrets as `NISSE_SIGNING_KEY` **in an environment named `signing`**,
   and configure that environment with (a) deployment branches: `main` only, and (b) a
   required reviewer: you. Both matter: the signing job runs the code on `main` with the key
   in its environment, so every run must wait for you. Put the same key in
   `/etc/nisse/nisse.env` on the engine host. It goes nowhere else, and never into a file in
   this repository.
3. **Sign.** After you have merged a config, start the "Sign research configs" workflow by
   hand on `main` and approve its run. It never starts by itself. It refuses a config that
   is within 30 days of another of the same family or strategy kind. Copy the manifest it
   prints to `research/signed/<name>.manifest.json` on the host.
4. **Require it.** Set `"required": true` in the engine config's `signature` block on the
   host. From then on `tick` and `session-close` exit 2 without a valid manifest.
5. **Token.** A fine-grained GitHub token for the agents' account with: contents read and
   write on **the fork only**; pull requests read and write on this repository (to open the
   pull request); nothing else. It must not have contents write on this repository.
6. **Agent report.** Add to the host's crontab, after the scoring job:
   `deploy/run.sh report target/release/nisse-agent-report config/eval/gates.json research/configs/trend-v1.json data/bars data/agent-report.json`.
   How the file reaches the agents (a private gist, a bucket, a commit to a private
   repository) depends on decision 1 under "Decide first"; it must not be committed to a
   public repository.
7. **Scheduled tasks.** Create three Claude Code scheduled tasks, each starting a fresh
   session, each given the matching file from `research/agents/` as its instructions and
   the token from step 5: pre-market (before 13:30 UTC on weekdays), post-close (after the
   report is written), overnight (once a night at most). Hourly at most, per PLAN.md.

## Every agent pull request

Treat it as untrusted input. Before merging:

- It comes from the agents' fork, and the "Research proposal" workflow's check is green on
  it. For a pull request from a fork (or from a branch named `research/...`) that check
  fails unless the pull request changes only plain files under `research/configs/`,
  `research/notes/` and `research/labels/`, changes at most one config, and respects the
  30-day budget. For any other pull request that touches nothing under `research/` the
  workflow checks nothing and says so: a green result there says nothing about what the
  pull request changes. That workflow's file comes from `main`, not from the pull request,
  so a pull request cannot weaken it. It judges the result of merging the pull request into
  `main`, not the branch's own diff, so a branch built on old history cannot quietly undo
  newer commits. It runs only for pull requests into `main`.
- Read the note. Was the rationale written before any result? Is it a reason, or a
  description of a backtest?
- Evaluate it where the data is, **without checking the pull request out there**. The host
  must only ever build and run `main`. Take the one JSON file and nothing else:
  `git fetch origin pull/<N>/head`, then
  `git show FETCH_HEAD:research/configs/<name>.json > /tmp/<name>.json`, then
  `scripts/evaluate-proposal.sh /tmp/<name>.json` from a clean checkout of `main` (the
  script refuses anything else). It registers a trial; the count never resets. Read the
  gate report.
- Just before merging, re-run the "Research proposal" job (it judges against `main` as it is
  now; an old green result does not account for anything merged since, or for the proposal's
  date having gone stale). If `.github/workflows/proposal.yml` itself has changed on `main`
  since the pull request's last run, close and reopen the pull request instead: a re-run
  keeps the workflow file of the original run.
- Read the result in the "Research proposal" workflow's own run, not from the list of check
  names: the run says "Judging against main at <commit>" and ends with `proposal check: ok`.
  A pull request from a fork also runs the fork's copy of the ordinary CI workflow, which
  could contain a job with a reassuring name.
- A person merges. Paper or recent live results are never a reason to merge.

## Turning it off

Disable the three scheduled tasks and revoke the token. The engine keeps running on the last
signed config.

Once `"required": true` is set, a missing or invalid manifest stops `tick` and
`session-close` before they do anything: exit code 2, and nothing is reconciled either, so
orders resting at the broker stay there and fills go unrecorded until it is fixed. Treat exit
code 2 in `log/tick.log` as an alert. Before you rotate the key or replace a manifest, engage
the kill switch (`deploy/engine.sh kill --by <name>`), make the change, check with
`deploy/engine.sh tick`, then release.

A manifest does not expire, and the engine checks only that it signs the deployment the
engine's config describes. To retire a signed config, remove its manifest from the host (and
point the engine config's `signature.manifest` at the one you want); to retire every
manifest at once, change the key.
