# Decision log

Decisions that are open, proposed, or settled but scheduled for a second look. Settled decisions without a revisit date live in the PRD's [Decisions](../PRD.md#risks-and-decisions) list.

| ID | Decision | Status | Revisit or decide by | Notes |
| --- | --- | --- | --- | --- |
| D-01 | Domain for docs and `llms.txt` | Open | Before 0.1 (W8-13) | Owner: maintainer |
| D-02 | Value semantics for records, arrays, `Map`, `Set` | Decided, revisit | End of Phase 1 | Revisit if W4-04 benchmarks or W8 suite data show copies hurt throughput or agent success. The alternative is TS-style reference semantics with `readonly` by default |
| D-03 | Strings: no `.length` or `s[i]`, byte offsets everywhere | Decided, revisit | End of Phase 1 | Revisit if the suite shows `.length` fixes dominate the fix loop. The alternative is `.length` = byte length with a warning on non-ASCII literals |
| D-04 | `int` lowers to Go `int`; 64-bit targets only | Decided | — | Revised in review from `int64` |
| D-05 | Class instances shared between tasks: race detector only in v1 | Decided, revisit | End of Phase 3 | The alternative is a checker rule that classes captured by `spawn` must be `readonly` or marked shareable |
| D-06 | `nisse fmt` | Proposed | Start of Phase 2 | Smaller fix diffs, one layout for agents. See W5-14 |
| D-07 | `nisse init` | Proposed | Start of Phase 2 | Scaffolds `go.mod`, `main.nis`, `AGENTS.md`. See W5-08 |
| D-08 | `nisse:time`, `nisse:crypto` in the v1 stdlib | Proposed | Mid Phase 3 | Decide from how often suite tasks need them. See W7-13 |
| D-09 | Minimum supported Go version | Open | W0-01 | Pin in `go.mod`; support the two latest Go releases, as Go itself does |
| D-10 | Project configuration file | Decided | — | None in v1: a project is a directory with `go.mod`; `.nis` files are found from there |
| D-11 | Parser strategy | Decided | — | Hand-written recursive descent with error recovery; best control over spans and diagnostics |
| D-12 | Semicolons and newlines | Open | W2-01 | Adopt TypeScript's automatic semicolon insertion rules exactly, or a stricter newline rule with a fix. Leaning towards TS rules for familiarity |
| D-13 | Bundler for npm phase | Decided | — | esbuild's Go API; only matters in wave 9 |
