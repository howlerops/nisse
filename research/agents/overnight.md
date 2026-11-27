# Overnight research run

When: at most once a night.
Reads: the newest agent report, everything under `research/`, and public research you find.
Writes: at most one pull request, containing at most one new or changed config under
`research/configs/` with its note under `research/notes/`. Most nights the right output is
no pull request.

Follow the rules in `research/agents/README.md`.

## Before proposing anything

A proposal is allowed only when all of these hold. If one does not, stop and write nothing.

1. It is at least 30 days since the newest config of the same family (`created`). CI
   enforces this.
2. The report shows at least 100 visible decisions for the strategy version the proposal
   changes, and no score you rely on is `suppressed`.
3. The lifetime trial budget is not spent (the report's gates say so). Every evaluation of a
   proposal is a trial, and the count never resets.

## Order of work

1. **Write the note first**, before running or reading any result for the new config:
   - `## Economic rationale`: why this should work, in terms of who is on the other side or
     what risk is being paid for. "It backtests well" is not a rationale.
   - `## Expected effect`: what you expect to change, in which direction, and roughly how
     much, compared with the parent config.
   - `## What would change my mind`: the results that would make you withdraw it.
2. Then write the config (`research/schema/strategy-config.schema.json` gives the format),
   with `parent` set to the config it changes.
3. Open the pull request. Say in its description what changed from the parent, in one line
   per parameter. Do not run a backtest yourself and do not quote one: the evaluation is run
   by the pipeline after review, and it registers the trial.

## What a proposal may not be

- A decision for a date or a symbol, or a rule that names dates.
- A parameter chosen because it would have done best recently.
- A change to the universe that adds a fund because it went up.
- Anything that needs a file outside `research/` to change. If you believe the engine needs
  a new kind of strategy, write that as a note only; a person decides.
