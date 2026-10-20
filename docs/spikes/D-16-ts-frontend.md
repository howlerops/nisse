# D-16 spike: TypeScript front end for `nisse port`

Oct 19, 2026 · decision [D-16](../ledger/decisions.md) · code in [`spikes/D-16-ts-frontend/`](../../spikes/D-16-ts-frontend/README.md)

## Recommendation

**Vendor TypeScript's own Go compiler (option B) and use it as a library from `nisse port`.** Vendor from a TypeScript release branch (`release-7.x`), and keep every call into it behind one small adapter package, so upstream churn lands in one place. Run the official TypeScript API (option A) in CI as an oracle: the same project dumped both ways must show zero real differences.

**Fallback: option A**, the official TypeScript 7 API through its Node client. It gives the same answers and works today. It is 2.3–2.8× slower, needs Node and the `typescript` npm package at port time, and its API is still marked unstable.

**Rejected: option C** (the classic TypeScript 6 JS API plus our own parser). It is the slowest, and it is a dead end: TypeScript 7 no longer ships the classic API, so C would freeze `nisse port` at TypeScript 6.0.3.

Why B:

| | B · vendored Go compiler | A · TS 7 API over IPC | C · TS 6 JS API + own parser |
| --- | --- | --- | --- |
| Fidelity vs current TypeScript | Identical (0 real differences; only lib file paths differ) | Reference | 99.98% same symbols; ~0.95% of identifiers print a different type, mostly print form |
| Large project (93k lines), median wall time | **19.5 s** (15.9 s on 4 goroutines) | 43.9 s (7.1 nightly) · 49.2 s (7.0.2) | 109.1 s |
| Runtime needed at port time | None (inside the `nisse` binary) | Node + `typescript@7` | Node + `typescript@6` |
| AST access | Full Go AST, positions, comments, in process | AST fetched over IPC (175k–219k requests, 300–380 MB for the large project) | Full JS AST, but in another process; our own parser for everything else |
| Stability seen in the spike | 6 of ~25 entry points changed in 6 months, all at the 7.0 → 7.1 boundary; mechanical fixes | 2 breaking changes to a 120-line script in 3 months; a server panic in 7.0.2 | Frozen since Apr 2026; will not learn TypeScript 7.1+ |
| What we maintain | ~309k vendored lines (untouched), one adapter, a re-vendor script | One client script | A full TypeScript parser (TypeScript's own is ~38k lines) and position mapping |

## What was tested

**Context found during the spike.** TypeScript 7 (the Go port) is the current release: `typescript@7.0.2` on npm since Jul 8, 2026, and nightlies of 7.1. The Go code now lives in `microsoft/TypeScript` under `tsc/` (`microsoft/typescript-go` is closed and archived). Its Go packages are all under `internal/`, so other modules cannot import them. `typescript@7` exports no classic compiler API (its main export is just `version`); tools that need the old API pin TypeScript 6 (Immich pins `@typescript/typescript6`). TypeScript 7 ships a new API instead: a JS client (`typescript/unstable/sync` in 7.0.2, `typescript/sync` in 7.1 nightlies) that talks msgpack over IPC to the native compiler. Its README still lists the API as "not ready".

**Options.**

- **A:** `dump-ts7.mjs`, TypeScript 7's API from Node, run on `typescript@7.0.2` and `typescript@7.1.0-dev.20261019.1`. It batches per file (one request for all identifiers' types, one for symbols) and caches type strings.
- **A-go:** a Go client for the same IPC protocol. Not prototyped (see [below](#a-go-a-go-client-for-the-ts-7-ipc-api)).
- **B:** `vendored/vendor.sh` copies `tsc/internal/**` (without tests) into our module as `internal/tsgo/`, rewrites import paths, and copies upstream's dependency versions. `cmd/nissedump` uses `compiler.NewProgram` and the checker directly. With `-j 4` it dumps files on 4 goroutines, each through the checker the program assigned to the file.
- **C:** `dump-ts6.mjs`, the classic API of `typescript@6.0.3`. This tests the type-dump half of option C; the "own parser" half was sized, not built.

**Projects.**

| Project | Source | Size |
| --- | --- | --- |
| `rw` | [node-express-realworld-example-app](https://github.com/gothinkster/node-express-realworld-example-app) @ `30b68e1` (Express + Prisma) | 2.4k lines, 358 program files, 1,992 identifiers |
| `im` | [Immich](https://github.com/immich-app/immich) `server/` @ `fff2bd7` (NestJS, Kysely, zod, socket.io) | 93k lines, ~3,300 program files, 234,934 identifiers |

**Output.** Each option writes one JSON line per identifier in the project's own files. Each line holds the type at the location (`typeToString` with no truncation), the symbol name, the first declaration's location, and, for callees, the resolved signature as parameter and return types. `compare.py` joins dumps on (file, identifier text, occurrence number), so byte vs UTF-16 offsets cannot cause false mismatches. It sorts each difference into one of four classes:

- **exact**
- **cosmetic:** the same tokens in a different order, or a different path to the same default lib file
- **type-position identifier:** a type name such as `Request` in `req: Request`, with the same symbol and declaration; the APIs answer `getTypeAtLocation` differently there
- **real difference:** everything else

**Measurement.** `bench.sh` ran 5 rounds of every option on both projects; the tables show medians. Peak memory is the sum of resident memory over the whole process tree, sampled every 20 ms, so option A counts both Node and the native server. Other spikes were benchmarking on the same 4-core machine at the same time: absolute times are noisy, so read the ratios.

## Results

### Speed and memory (median of 5 runs)

| Project · option | Wall (s) | Load (s) | Check (s) | Dump (s) | Peak RSS, process tree (MB) | API errors | IPC requests | IPC MB received |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| im · A 7.0.2 | 49.2 | 1.6 | 9.3 | 39.1 | 3708 | **2** | 219,032 | 307 |
| im · A 7.1 nightly | 43.9 | 1.2 | 7.5 | 34.7 | 3318 | 0 | 174,650 | 384 |
| im · B, 1 goroutine | 19.5 | 1.0 | 8.2 | 10.4 | 3140 | – | – | – |
| im · B, 4 goroutines | **15.9** | 0.9 | 7.7 | 7.7 | 3482 | – | – | – |
| im · C (TS 6.0.3) | 109.1 | 4.9 | 36.0 | 67.5 | 2346 | – | – | – |
| rw · A 7.0.2 | 1.1 | 0.2 | 0.1 | 0.7 | 278 | 0 | 1,850 | 22 |
| rw · A 7.1 nightly | 1.0 | 0.2 | 0.1 | 0.6 | 249 | 0 | 1,726 | 22 |
| rw · B, 1 goroutine | 0.3 | 0.2 | 0.1 | 0.1 | 91 | – | – | – |
| rw · B, 4 goroutines | 0.3 | 0.2 | 0.1 | 0.1 | 92 | – | – | – |
| rw · C (TS 6.0.3) | 2.9 | 1.2 | 0.8 | 0.5 | 276 | – | – | – |

- **Checking is the same work in A and B** (7.5–9.3 s on Immich; it is the same Go compiler). The difference is in getting the answers out. A spends 35–39 s on 175k–219k IPC round trips and on rebuilding the AST in JavaScript (393k nodes fetched for `rw` alone). B reads the same data in process in 10.4 s, or 7.7 s with 4 goroutines.
- **C is 4.5× slower to check** than the Go compiler (36 s against 8 s) and slowest overall.
- **Memory** is similar for everything except A, which holds the program twice: once in the native server and partly in Node. All options fit a developer machine.
- **Option B's dumper binary is 25 MB**, including the embedded `lib.*.d.ts` files (4 MB of source).

### Fidelity

| Comparison | Identifiers | Exact | Cosmetic | Type-position (API) | Real difference | Symbols agree |
| --- | --- | --- | --- | --- | --- | --- |
| im · A 7.1 nightly vs B (same compiler revision) | 234,934 | 96.90% | 3.10% | 0% | **0%** | 100.00% |
| im · B, 1 vs 4 goroutines | 234,934 | 100.00% | 0% | 0% | 0% | 100.00% |
| im · A 7.0.2 vs A 7.1 nightly | 234,934 | 96.76% | 3.10% | 0% | 0.14% | 100.00% |
| im · C (TS 6) vs A 7.1 nightly | 234,934 | 79.89% | 15.54% | 3.62% | 0.95% | 99.98% |
| rw · A 7.1 nightly vs B | 1,992 | 96.79% | 3.21% | 0% | **0%** | 100.00% |
| rw · B, 1 vs 4 goroutines | 1,992 | 100.00% | 0% | 0% | 0% | 100.00% |
| rw · A 7.0.2 vs A 7.1 nightly | 1,992 | 96.64% | 3.16% | 0% | 0.20% | 100.00% |
| rw · C (TS 6) vs A 7.1 nightly | 1,992 | 85.34% | 9.59% | 4.62% | 0.45% | 99.75% |

What the differences are:

- **B vs A:** the only differences are lib file paths (`bundled/lib/lib.es5.d.ts` against the npm package's copy). Types, symbols, declarations and signatures are identical. Running B on 4 goroutines changes nothing.
- **TypeScript 6 vs 7:**
  - *Cosmetic (10–16%).* TypeScript 7 orders union members and object properties differently (`RejectOnNotFound | RejectPerOperation` vs the reverse).
  - *Type-position identifiers (3.6–4.6%).* For a type name like `Request` in `req: Request`, TS 6's `getTypeAtLocation` returns the declared type and TS 7's API returns `any`, with the same symbol. `nisse port` would read annotation types through `getTypeFromTypeNode` anyway.
  - *Remaining 0.95%.* Sampled by hand (category counts in the spike log, not exhaustive): mostly print form, such as quote style, parentheses around function types, `util.Mask` vs `Mask`, and `import("…")` qualifiers. Some are genuine checker differences: a different zod `refine` overload or declaration chosen, and Kysely intersection types simplified differently. Also `process.env.X`, where TS 7 returns the index-signature symbol (`__index`) and TS 6 returns none. *Not verified:* the exact split between print-only and semantic differences. A string comparison cannot tell them apart; a structured comparison of types would.
- **TypeScript 7.0.2 vs a 7.1 nightly:** 0.14–0.20% of identifiers differ, which is what three months of compiler changes look like.

### Stability and churn

**Option A, the TypeScript 7 API, from 7.0.2 (Jul 8) to the 7.1 nightly (Oct 19):**

- Export paths moved from `typescript/unstable/sync` to `typescript/sync`.
- `api.updateSnapshot(params)` became `api.createSnapshot(params)`; `updateSnapshot` is now private and takes different arguments.
- `api/sync/api.d.ts` changed on 1,607 lines (it was 528 lines), mostly from adding generator-based variants of every method. The method calls our script uses kept working.
- 7.0.2's server **panicked twice on Immich** inside `getTypeAtLocation` (`interface conversion: checker.TypeData is *checker.TypeReference, not *checker.TupleType`). The nightly fixes it. The panic comes back as an error and the session survives, so the dump retries node by node.

**Option B, the Go internals:**

| Package | Commits since 2026-04-19 | Exported API, Apr 17 → Oct 19 | Removed | Signature changed | Added |
| --- | --- | --- | --- | --- | --- |
| `checker` | 270 | 1,064 → 1,170 | 10 | 10 | 116 |
| `ast` | 60 | 3,770 → 3,839 | 43 | 32 | 112 |
| `compiler` | 64 | 143 → 171 | 12 | 43 | 40 |
| `tsoptions` | 47 | 163 → 183 | 21 | 41 | 41 |
| `parser` | 45 | 42 → 42 | 0 | 0 | 0 |
| `scanner` | 20 | 98 → 98 | 1 | 0 | 1 |

- **Our dumper uses about 25 entry points; 6 of them changed in six months.** `Symbol.Name` and `Symbol.Declarations` became methods, `SourceFile.FileName` now returns a typed path, and `NewCompilerHost`, `GetParsedCommandLineOfConfigFile` and `LibPath` changed parameters or results.
- **Building against older code shows when it happened.** Against the April 17 code the dumper fails with 8 compile errors, all mechanical. Against the `v7.0.2` tag it fails with the same 8 errors. Against today's `release-7.1` branch it builds unchanged, and its output is byte-identical to HEAD's. So the changes landed between 7.0 and 7.1, not continuously.
- **Expected upkeep: a few hours per TypeScript minor release** for the adapter, provided `nisse port` keeps a narrow surface. *Not verified:* `nisse port` will use much more of the checker than this dumper (type flags, union members, properties, index infos, node builder), so its exposure will be larger. The checker's exported API has removed or changed about 2% of its members per six months.
- **Go version coupling.** TypeScript's `tsc/go.mod` moved from Go 1.26 (7.0) to Go 1.27 (7.1). Vendoring it means Nisse's minimum Go version follows TypeScript's. This feeds D-09.
- **Vendoring is mechanical.** Copy, rewrite import paths (it also handles the older `microsoft/typescript-go` module path), copy upstream's `require` block, `go mod tidy`. It builds with no source patches: 491 files and 309k lines from `release-7.1`, 454 files and 280k lines from `v7.0.2`. Only `execute/tsctests`, which imports test helpers, is left out.

**Option C:** TypeScript 6.0.3 (Apr 16, 2026) is the last release with the classic JS compiler API. That makes it stable, but TypeScript features from 7.1 onwards will never reach it. Projects already drop TS 6-only settings: the RealWorld app's `baseUrl` and `moduleResolution: node` are deprecated in 6 and removed in 7.

### Licence

TypeScript, including its Go compiler, is Apache-2.0. Vendoring it into an MIT project is compatible, with these obligations:

- keep `LICENSE` and `NOTICE.txt` with the vendored code (`vendor.sh` copies both)
- mark the files as modified, because the import-path rewrite is a modification under Apache-2.0 §4(b)
- ship the NOTICE text with `nisse` binaries

*Not verified by counsel.*

### A-go: a Go client for the TS 7 IPC API

Not prototyped. The official JS client plus its AST decoder is about 40k lines of TypeScript, much of it generated from the Go server. A Go client would have to reimplement that, or vendor the server's own `internal/api/encoder` decoder, which is option B's vendoring with A's IPC overhead and unstable protocol on top. It is dominated by B.

### Option C's parser half

Not built. TypeScript's own parser, scanner and AST packages are 38.5k lines of Go (9.1k, 4.3k and 25.1k). Our own TypeScript parser would need to match them construct for construct, and keep matching new syntax. It would also need to map TS 6's UTF-16 offsets onto byte offsets. B gets this parser for free, together with comment ranges (`scanner.GetLeadingCommentRanges`), which `nisse port` needs to keep comments (W14-04). This does not affect D-11: Nisse's own `.nis` parser is still ours.

## Risks of the recommendation

1. **Upstream restructures the Go packages** (for example in TypeScript 8) beyond what the adapter can absorb.
   *Mitigation:* vendor from release branches only, keep the adapter as the single importer, and keep option A working in CI as the oracle and fallback.
2. **Exposure grows with `nisse port`'s use of the checker.**
   *Mitigation:* the adapter exposes the port's own concepts (types, symbols, signatures as Nisse port data), not checker types, and every adapter function has a test on the reference app corpus.
3. **Go version coupling** (D-09): Nisse's minimum Go version follows TypeScript's.
4. **Binary size:** `nisse` grows by roughly the 25 MB the dumper weighs (*not measured* inside a real `nisse` binary). If that matters, ship `nisse port` as a separate download.
5. **Security and bug fixes are ours to pull in**, as with any vendored code. Re-vendor on every TypeScript patch release.
6. **Evidence limits:**
   - two projects, neither with JSX (the frontend wave needs a TSX project checked)
   - fidelity compared as printed strings, not structured types
   - timings taken while other spikes were running

## Reproduce

```sh
cd spikes/D-16-ts-frontend
WORK=/tmp/nisse-d16 ./setup.sh          # clones and installs the two projects, TypeScript, typescript-go; vendors; builds
WORK=/tmp/nisse-d16 RUNS=5 ./bench.sh   # ~25 minutes on 4 cores; writes out/summary.md
```

Churn check:

```sh
cd spikes/D-16-ts-frontend/apidiff
GOTOOLCHAIN=auto go run . $WORK/old-apr17/internal $WORK/ts/tsc/internal checker ast compiler tsoptions scanner parser
# Build the dumper against another revision: point vendor.sh at an archive of that revision's tsc/ tree.
```

Needs Node 22, pnpm and Go with `GOTOOLCHAIN=auto` (the vendored compiler needs Go 1.27).

## Quiet-machine re-run (Oct 20, 2026)

The parent session re-ran `bench.sh` (5 rounds) alone on the machine, after all spikes finished; load average stayed under 2.6. `out/summary.md` now holds these numbers. Immich, medians:

| Option | Wall (s), parallel run | Wall (s), quiet run | Peak RSS (MB), quiet |
| --- | ---: | ---: | ---: |
| B vendored, 4 goroutines | 15.9 | **11.8** | 3421 |
| B vendored, 1 goroutine | 19.5 | 15.6 | 3264 |
| A, TS 7.1 nightly | 43.9 | 39.5 | 3311 |
| A, TS 7.0.2 | 49.2 | 47.8 | 3717 |
| C, TS 6.0.3 | 109.1 | 91.5 | 2471 |

B is 3.3× faster than A on the quiet machine (2.8× before) and 7.8× faster than C. Fidelity tables are identical to the parallel run. The recommendation stands.
