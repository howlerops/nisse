# Pre-market run

When: once, before the market opens, on trading days.
Reads: the newest agent report (`data` and `regime` sections only).
Writes: `research/notes/daily/<YYYY-MM-DD>-pre.md`, as one pull request.

Follow the rules in `research/agents/README.md`.

1. Read the report's `data` section. List, as flags:
   - symbols with no bar on the last session;
   - zero-volume bars;
   - one-day moves over 25% (say whether a split, a distribution or bad data is the likelier
     cause, and that you have not verified it);
   - a `last_session` that is not the previous trading day.
   If `data` is `null`, the one flag is that there is no bar file.
2. Write one paragraph on the regime field: what it is, and that it is a description of the
   benchmark against its own average, not a forecast.
3. Do not suggest a trade, a position size or a change of parameters. This run reports on
   data; it proposes nothing.

The note has two headings, `## Flags` and `## Regime`. No flags is a valid result: write
"None." Keep it under 200 words.
