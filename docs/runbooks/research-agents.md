# Research agents

How the owner turns the research loop on. Nothing here is done by an agent or by CI, and
nothing in this repository starts an agent on its own.

## What exists

- `research/`: the only directory an agent may write (configs, notes, labels).
- `research/agents/*.md`: the instructions for the three runs.
- `nisse-proposal-check`: the CI check every research pull request must pass.
- `nisse-agent-report`: writes the aggregates-only report agents read.
- `nisse-sign` and the engine's signature check: a merged config is signed; an engine whose
  config says `"required": true` trades nothing without a valid signature.

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

1. **Signing key.** Generate 32 random bytes as hex (`openssl rand -hex 32`). Put it in the
   repository's Actions secrets as `NISSE_SIGNING_KEY`, in an environment named `signing`
   that only the `main` branch may use, and in `/etc/nisse/nisse.env` on the engine host.
   It goes nowhere else, and never into a file in this repository.
2. **First signature.** After the config is on `main`, run the "Sign research configs"
   workflow (it also runs on every push to `main` that changes `research/configs/`). Copy
   the manifest it prints to `research/signed/<name>.manifest.json` on the host.
3. **Require it.** Set `"required": true` in the engine config's `signature` block on the
   host. From then on `tick` and `session-close` exit 2 without a valid manifest.
4. **Agent report.** Add to the host's crontab, after the scoring job:
   `deploy/run.sh report target/release/nisse-agent-report config/eval/gates.json research/configs/trend-v1.json data/bars data/agent-report.json`.
   How the file reaches the agents (a private gist, a bucket, a commit to a private
   repository) depends on decision 1; it must not be committed to a public repository.
5. **Token.** Create a fine-grained GitHub token with access to the research repository only:
   contents read and write, pull requests read and write, nothing else.
6. **Scheduled tasks.** Create three Claude Code scheduled tasks, each starting a fresh
   session, each given the matching file from `research/agents/` as its instructions and
   the token from step 5: pre-market (before 13:30 UTC on weekdays), post-close (after the
   report is written), overnight (once a night at most). Hourly at most, per PLAN.md.

## Every agent pull request

Treat it as untrusted input. Before merging:

- The "research proposal" check is green. It guarantees the pull request touches only
  `research/configs/`, `research/notes/` and `research/labels/`, changes at most one config,
  and respects the 30-day budget.
- Read the note. Was the rationale written before any result? Is it a reason, or a
  description of a backtest?
- Run `scripts/evaluate-proposal.sh research/configs/<name>.json` where the data is. It
  registers a trial; the count never resets. Read the gate report.
- A person merges. Paper or recent live results are never a reason to merge.

## Turning it off

Disable the three scheduled tasks and revoke the token. The engine keeps running on the last
signed config.
