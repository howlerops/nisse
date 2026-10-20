# Decision log

Decisions that are open, proposed, or settled but scheduled for a second look. Settled decisions without a revisit date live in the PRD's [Decisions](../PRD.md#risks-and-decisions) list.

| ID | Decision | Status | Revisit or decide by | Notes |
| --- | --- | --- | --- | --- |
| D-01 | Domain for docs and `llms.txt` | Decided, to register | Register now; live before 0.1 (W8-13) | `nisselang.dev`. `nisse.dev` is taken; RDAP lookup found `nisselang.dev` unregistered on Oct 20, 2026. Owner: maintainer |
| D-02 | Value semantics for records, arrays, `Map`, `Set` | Decided, revisit | End of Phase 1 | Revisit if W4-04 benchmarks or W8 suite data show copies hurt throughput or agent success. The alternative is TS-style reference semantics with `readonly` by default |
| D-03 | Strings: no `.length` or `s[i]`, byte offsets everywhere | Decided, revisit | End of Phase 1 | Revisit if the suite shows `.length` fixes dominate the fix loop. The alternative is `.length` = byte length with a warning on non-ASCII literals |
| D-04 | `int` lowers to Go `int`; 64-bit targets only | Decided | — | Revised in review from `int64` |
| D-05 | Class instances shared between tasks: race detector only in v1 | Decided, revisit | End of Phase 3 | The alternative is a checker rule that classes captured by `spawn` must be `readonly` or marked shareable |
| D-06 | `nisse fmt` | Decided: v1 | — | Smaller fix diffs, one layout for agents; `nisse port` reuses its comment-preserving printer (W14-04). See W5-14 |
| D-07 | `nisse init` | Decided: v1 | — | Scaffolds `go.mod`, `main.nis`, `AGENTS.md`. See W5-08 |
| D-08 | `nisse:time`, `nisse:crypto` in the v1 stdlib | Decided: both in v1 | — | Nearly every backend needs them, and `nisse:time` is the base for JavaScript `Date` parity (W10-22). See W7-13 |
| D-09 | Minimum supported Go version | Decided | — | Follows the vendored TypeScript compiler's minimum (Go 1.27 today, per D-16), and never older than the two latest Go releases |
| D-10 | Project configuration file | Decided, revised by D-22 | — | None in v1: a project is a directory with `go.mod`. The parity line adds `nisse.json` for the profile |
| D-11 | Parser strategy | Decided | — | Hand-written recursive descent with error recovery; best control over spans and diagnostics |
| D-12 | Semicolons and newlines | Decided | — | TypeScript's automatic semicolon insertion rules exactly, so agent-written code and `nisse port` output parse as in TypeScript. Rejected: a stricter newline rule, required semicolons |
| D-13 | Bundler for npm phase | Decided | — | esbuild's Go API; only matters in wave 9 |
| D-14 | Parity model | Decided | — | Nisse stays strict and widens towards TypeScript with Go lowerings; `nisse port` translates existing apps. Rejected: a separate loose `.ts` dialect, and making Nisse fully TypeScript |
| D-15 | Reference apps | Decided by [spike](../spikes/D-15-reference-apps.md) | Re-pin commits at W18-01 | Tier 1 gothinkster Express+Prisma RealWorld `30b68e1` (oracle: Jan 2024 API collection, spec `a0767b8`); 1b mutoe NestJS RealWorld `0c30a18`; tier 2 Umami `ec0ff50`; tier 3 Immich `fff2bd7`, fallback Hoppscotch `63273f8`. Outline out (BSL 1.1) |
| D-16 | TypeScript front end for `nisse port` | Decided by [spike](../spikes/D-16-ts-frontend.md) | Each TypeScript minor release | Vendor TypeScript 7's Go compiler (`tsc/internal/` of microsoft/TypeScript, Apache-2.0) at a release branch and call it as a library. Fallback and CI cross-check: the official TypeScript 7 API over IPC. Rejected: TS 6 JS API plus our own parser (7.8× slower, last release, ~1% type differences from TS 7) |
| D-17 | Aliasing in ported code | Decided, revisit | End of Phase 7 | `Shared<T>` reference box inserted from alias analysis. Revisit if tier 1 ports are full of it |
| D-18 | `null` | Decided | — | A distinct unit type in the parity line, so `x === null` and `x === undefined` keep their TypeScript meaning. Reported in the `strict` profile |
| D-19 | `number` | Decided | — | Alias of `f64` with JavaScript semantics, including ToInt32 bitwise operators |
| D-20 | Circular imports | Decided | — | Each strongly connected group of modules becomes one Go package. Error in `strict`, allowed in `compat` |
| D-21 | Decorators | Decided | — | Compile-time lowering of TC39 and legacy decorators, with static metadata tables. No runtime reflection |
| D-22 | Strict and compat profiles | Decided | — | `nisse.json` profile; `strict` for new projects, `compat` for `nisse port` output; same compiler and Go output |
| D-23 | Frontend strategy | Decided by [spike](../spikes/D-23-react-ssr.md) | After the tier 2 port | Render TSX in Go, including client components' first render, byte-identical to `react-dom/server` (7.8× Node in the spike). Fallback for untranslatable components: React in QuickJS on wazero (`fastschema/qjs`) with the join patch; production gate 25 pages/s per core, p99 ≤ 100 ms. Node sidecar: opt-in, off by default |
| D-24 | npm track order | Decided | — | Go-native compat package, then port from TS source, then embedded engine, then flagged. Chosen per package and pinned in a lock file |
| D-25 | Regex engine | Decided by [spike](../spikes/D-25-regex.md) | — | RE2 (vendored `regexp` with a 5-line patch) for eligible patterns, QuickJS `libregexp` via modernc transpilation for the rest. 100% match with Node 22 on 64,585 cases. Rejected: regexp2 (capture reset bug), goja (panics, missing flags), QuickJS-ng in WASM (timeouts ignored) |
| D-26 | Concurrency model for ported code | Decided | — | `compat` code runs on an event-loop executor with Node's task ordering; `strict` code keeps goroutine parallelism |
| D-27 | Positioning against Encore.ts | Decided | After the tier 1 port | Treat Encore.ts as the main alternative to porting: compare against it in the ported-performance metric (W8-06, W15-10) and benchmark the tier 1 API on it (W18-10) |
