# Decision log

Decisions that are open, proposed, or settled but scheduled for a second look. Settled decisions without a revisit date live in the PRD's [Decisions](../PRD.md#risks-and-decisions) list.

| ID | Decision | Status | Revisit or decide by | Notes |
| --- | --- | --- | --- | --- |
| D-01 | Domain for docs and `llms.txt` | Open | Before 0.1 (W8-13) | Owner: maintainer |
| D-02 | Value semantics for records, arrays, `Map`, `Set` | Decided, revisit | End of Phase 1 | Revisit if W4-04 benchmarks or W8 suite data show copies hurt throughput or agent success. The alternative is TS-style reference semantics with `readonly` by default |
| D-03 | Strings: no `.length` or `s[i]`, byte offsets everywhere | Decided, revisit | End of Phase 1 | Revisit if the suite shows `.length` fixes dominate the fix loop. The alternative is `.length` = byte length with a warning on non-ASCII literals |
| D-04 | `int` lowers to Go `int`; 64-bit targets only | Decided | — | Revised in review from `int64` |
| D-05 | Class instances shared between tasks: race detector only in v1 | Decided, revisit | End of Phase 3 | The alternative is a checker rule that classes captured by `spawn` must be `readonly` or marked shareable |
| D-06 | `nisse fmt` | Proposed, needed by D-14 | Start of Phase 2 | Smaller fix diffs, one layout for agents. `nisse port` needs a printer that preserves comments (W14-04), so this is effectively required. See W5-14 |
| D-07 | `nisse init` | Proposed | Start of Phase 2 | Scaffolds `go.mod`, `main.nis`, `AGENTS.md`. See W5-08 |
| D-08 | `nisse:time`, `nisse:crypto` in the v1 stdlib | Proposed | Mid Phase 3 | Decide from how often suite tasks need them. See W7-13 |
| D-09 | Minimum supported Go version | Open | W0-01 | Pin in `go.mod`; support the two latest Go releases, as Go itself does |
| D-10 | Project configuration file | Decided, revised by D-22 | — | None in v1: a project is a directory with `go.mod`. The parity line adds `nisse.json` for the profile |
| D-11 | Parser strategy | Decided | — | Hand-written recursive descent with error recovery; best control over spans and diagnostics |
| D-12 | Semicolons and newlines | Open | W2-01 | Adopt TypeScript's automatic semicolon insertion rules exactly, or a stricter newline rule with a fix. Leaning towards TS rules for familiarity |
| D-13 | Bundler for npm phase | Decided | — | esbuild's Go API; only matters in wave 9 |
| D-14 | Parity model | Decided | — | Nisse stays strict and widens towards TypeScript with Go lowerings; `nisse port` translates existing apps. Rejected: a separate loose `.ts` dialect, and making Nisse fully TypeScript |
| D-15 | Reference apps | Open | W18-01, before Phase 7 | Candidates: a TypeScript RealWorld backend (tier 1), Umami (tier 2), Immich server or Outline (tier 3). Confirm license, size, test suite and activity |
| D-16 | TypeScript front end for `nisse port` | Open | W14-01, start of Phase 7 | TypeScript's Go-based compiler (typescript-go) through its API, or our own TS-shaped parser plus a type dump from `tsc`. Spike both |
| D-17 | Aliasing in ported code | Decided, revisit | End of Phase 7 | `Shared<T>` reference box inserted from alias analysis. Revisit if tier 1 ports are full of it |
| D-18 | `null` | Decided | — | A distinct unit type in the parity line, so `x === null` and `x === undefined` keep their TypeScript meaning. Reported in the `strict` profile |
| D-19 | `number` | Decided | — | Alias of `f64` with JavaScript semantics, including ToInt32 bitwise operators |
| D-20 | Circular imports | Decided | — | Each strongly connected group of modules becomes one Go package. Error in `strict`, allowed in `compat` |
| D-21 | Decorators | Decided | — | Compile-time lowering of TC39 and legacy decorators, with static metadata tables. No runtime reflection |
| D-22 | Strict and compat profiles | Decided | — | `nisse.json` profile; `strict` for new projects, `compat` for `nisse port` output; same compiler and Go output |
| D-23 | Frontend strategy | Open, research-gated | W17-01, start of Phase 9 | Client stays TS/React bundled by embedded esbuild; Go-rendered server JSX; React SSR in the embedded engine if fast enough. Last resort, to avoid: a Node sidecar, which breaks the one-binary goal |
| D-24 | npm track order | Decided | — | Go-native compat package, then port from TS source, then embedded engine, then flagged. Chosen per package and pinned in a lock file |
| D-25 | Regex engine | Open | W10-20 | RE2 fast path plus an ECMAScript-compatible backtracking engine. Pick an existing Go library or write one, based on test262 RegExp results |
| D-26 | Concurrency model for ported code | Decided | — | `compat` code runs on an event-loop executor with Node's task ordering; `strict` code keeps goroutine parallelism |
