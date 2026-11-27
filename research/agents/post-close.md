# Post-close run

When: once, after the nightly scoring job has written the agent report.
Reads: the newest agent report.
Writes, as one pull request: `research/labels/<YYYY-MM-DD>.json` and
`research/notes/daily/<YYYY-MM-DD>-post.md`.

Follow the rules in `research/agents/README.md`.

## Labels

For each entry of the report's `label_queue`, give one label:

- `bad_decision`: the features at the time did not support the action under the strategy's
  own stated rule;
- `bad_execution`: the decision was reasonable but it was carried out poorly (partly filled,
  large shortfall);
- `data_error`: the features or the execution figures look wrong or inconsistent;
- `no_fault`: none of the above is visible from what you were given.

You are given what was decided and how it was executed, and deliberately **not** how it
turned out. Do not try to find out. A label is about the process, so "bad luck" cannot be
told apart from "no fault" here: both are `no_fault`.

The labels file:

```json
{"date": "YYYY-MM-DD",
 "labels": [{"decision_id": 17, "label": "no_fault", "why": "one sentence"}]}
```

Labels are text for people to read. They are never a score, never an input to sizing, and
never a reason to promote or retire a strategy.

## Note

Three headings: `## Rollup` (the report's counts and score aggregates per version, in words;
say "suppressed" where the report does), `## Gates` (the status of each gate per version, as
the report gives it), `## Anything odd` (or "Nothing."). No recommendation to trade, and no
claim that recent results show the strategy works or does not: the gates decide that, over
the whole trial history.
