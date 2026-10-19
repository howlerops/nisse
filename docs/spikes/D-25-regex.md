# D-25 spike: regex engine for JavaScript-compatible regex

Oct 19, 2026 · decides [D-25](../ledger/decisions.md) for [W10-20](../ledger/wave-10-language-parity.md) · harness in [`spikes/D-25-regex/`](../../spikes/D-25-regex/)

## Recommendation

**Use QuickJS's `libregexp` as the ECMAScript engine, with an RE2 fast path in front of it.**

1. **Backtracking engine: QuickJS `libregexp`**, reached cgo-free through `modernc.org/libquickjs` (QuickJS's C transpiled to Go), and called directly without the JS VM. It was the only candidate to match Node on every test:
   - all 2,056 real-world npm patterns (64,585 inputs);
   - all 77 feature probes;
   - all 2,325 test262 RegExp tests that Node 22 itself passes.

   It also implements the ES2025 features Node 22 lacks (duplicate named groups, modifiers), honours a timeout, and cross-compiles to all six v1 targets.
2. **Fast path: Go's RE2**, from a vendored copy of the standard `regexp` package with one ~5-line addition (match at a position with context). A translator decides per pattern whether RE2 gives identical results, plus two cheap per-input checks. It took 83.5% of corpus patterns, matched Node on 100% of the cases it handled, and makes those patterns immune to catastrophic backtracking.
3. **Do not use regexp2 or goja.**
   - regexp2's semantic gaps come from .NET and can't all be fixed by rewriting the pattern; the worst is capture groups inside loops not being reset between iterations.
   - goja's regex layer is regexp2 underneath, lacks the `d` and `v` flags, and crashes the process with a Go panic on duplicate group names.

**Patches we take on:**

- Vendor the standard `regexp` package to export a match-at-position function (done in the spike: [`re2at/at.go`](../../spikes/D-25-regex/go/re2at/at.go)).
- Transpile only `libregexp.c`, `libunicode.c` and `cutils.c` into a Nisse-owned package with Go-native allocation and timeout hooks. This drops the dependency on a JS context, and probably most of the 3.2 MB binary-size cost (not verified).
- Add prefilters to the wrapper. The anchored-start one is done and verified; a literal-prefix one is still to do.

## What was compared

| Engine | What it is | Version |
| --- | --- | --- |
| `node` | Ground truth: V8 irregexp in Node 22.22.0 (V8 12.4, ICU 77.1, Unicode 16.0) | — |
| `re2fast` | Go `regexp` (RE2), vendored with a match-at-position API, behind the Nisse translator; only the patterns it accepts | Go 1.26.0 stdlib |
| `regexp2v1` | `github.com/dlclark/regexp2`, `ECMAScript` option, wrapped by us (UTF-16 indices, group order, `lastIndex`) | v1.12.0 |
| `regexp2v2` | `github.com/dlclark/regexp2/v2`, `ECMAScript` + `OptionMaintainCaptureOrder` | v2.8.1 |
| `hybrid` | `re2fast`, else `regexp2v2` | — |
| `libregexp` | QuickJS `libregexp` called directly through `modernc.org/libquickjs` (no JS VM) | libquickjs v0.13.1 (QuickJS 2026-06-04) |
| `libregexpopt` | `libregexp` plus the anchored-start prefilter | — |
| `hybridlre` | `re2fast`, else `libregexpopt` (**the recommendation**) | — |
| `goja` | goja's `RegExp` (its own RE2 translation, else regexp2 v2) | 2026-10-19 commit `cf7b1419c9c0` |
| `quickjs` | `RegExp` inside the modernc.org/quickjs JS VM | v0.25.0 |
| `qjswasm` | `RegExp` inside QuickJS-ng compiled to WASM on wazero (`github.com/fastschema/qjs`) | v0.0.6 |

No other credible pure-Go ECMAScript regex engine turned up. The Go options are RE2 forks (`grafana/regexp`, `wasilibs/go-re2`), which have RE2 semantics, and regexp2, which goja and otto build on. *(Search not exhaustive.)*

## Method

1. **Corpus.**
   - Extracted every regex literal and every `new RegExp("…")` with a string-literal pattern from the JavaScript of 101 installed npm packages: 47 top-level installs plus their dependencies, listed with versions in [`corpus-packages.json`](../../spikes/D-25-regex/corpus-packages.json). That gave 6,498 unique valid patterns from 7,966 files.
   - Sampled 2,056, capped at 150 per package, because date-fns locale files and highlight.js grammars would otherwise make up most of the sample.
   - Added 77 hand-written probes covering test262's RegExp feature areas: named groups, lookbehind, `u`, `v` set notation, sticky, dotAll, `d` indices, backreferences, case folding, capture reset, Annex B, line terminators.
   - The pattern list is committed as [`corpus-patterns.json`](../../spikes/D-25-regex/corpus-patterns.json).
2. **Inputs.** Each pattern got 21 generic strings (including CRLF, U+2028, emoji, `ſ`/`K`/`İ`, NBSP and BOM), up to four strings generated to match it (randexp), mutations of those, and embedded copies. That makes 64,585 cases. Inputs with lone surrogates were dropped, because a Nisse string can't hold them.
3. **Ground truth.** Node runs one shared harness, [`harness.js`](../../spikes/D-25-regex/harness.js), which records:
   - the `exec` loop (up to 20 matches), with index, captures, groups, `d` indices and `lastIndex`, advancing past empty matches as `matchAll` does;
   - `replace(re, "<$&|$1>")`;
   - `split(re, 10)`.
4. **Comparison.**
   - The JS engines run the same harness file, so `exec`, `replace` and `split` are all compared.
   - The native engines are compared on the `exec` record. Their Go wrappers do what a Nisse runtime would: UTF-16 offsets, JS group numbering, `lastIndex`, sticky. `replace` and `split` sit on top of `exec` and weren't re-implemented for them.
   - Each case has a 1 s engine timeout. A driver process restarts any engine that hangs or crashes and records the case as failed.
5. **test262.**
   - Ran 2,557 files from test262 (`d53ebc8`, Oct 13, 2026): `built-ins/RegExp`, the regex-related `String.prototype` methods, `language/literals/regexp`.
   - Ran them in Node 22 (one `vm` context per test) and in each JS engine.
   - Skipped `module`, `async` and `raw` tests in every engine.
   - The headline column uses only the tests Node 22 passes, so Node's own gaps (ES2025 features, `$262`) aren't counted against the others.
6. **Benchmarks.**
   - **Compile:** all 2,056 corpus patterns, construct plus one `test("x")`, cold.
   - **Match:** 294 sampled patterns (with `g` added), in two workloads:
     - *per-line*: a non-global `exec` on each of the 243 lines of a 16 KB backend-style text, which is how validation code uses regexes;
     - *global*: `match` with `g` over the whole 16 KB.
   - Each figure is the median of 5 runs. The box had 4 cores shared with other spikes (load average 6.5 for the reported run). An earlier session at load average 2–4 gave figures within about 15% of these, but treat ratios as ±30%.
7. **Catastrophic backtracking.** Four classic exponential patterns, including the real-CVE email shape. Each runs in its own process with a 1 s engine timeout and a 15 s kill.

## Correctness against Node 22

Share of cases where the engine gives exactly Node's result. "covers" is the share of cases the RE2 fast path handled; the rest fall back.

| Feature (cases) | goja | quickjs VM | qjswasm | regexp2 v1 | regexp2 v2 | libregexp | re2fast | hybrid (RE2+regexp2) | **hybridlre** |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| npm, all (62,498) | 99.6% | 100% | 100% | 99.3% | 99.8% | 100% | 100% (covers 81%) | 99.8% | **100%** |
| npm, `u` flag (1,121) | 94.9% | 100% | 100% | 76.0% | 100% | 100% | 100% (39%) | 100% | **100%** |
| npm, `v` flag (84) | 0% | 100% | 70.2% | 0% | 67.9% | 100% | n/a | 67.9% | **100%** |
| npm, `\p{…}` (649) | 78.3% | 100% | 96.1% | 45.6% | 95.8% | 100% | n/a (excluded) | 95.8% | **100%** |
| npm, lookahead (3,565) | 99.6% | 100% | 100% | 98.9% | 99.8% | 100% | n/a | 99.8% | **100%** |
| probe: capture reset in loops (131) | 91.6% | 100% | 96.9% | 82.4% | 82.4% | 100% | 100% (24%) | 82.4% | **100%** |
| probe: `^`/`$` with `m` and `\r`, U+2028 (102) | 96.1% | 100% | 100% | 96.1% | 96.1% | 100% | 100% (92%) | 96.1% | **100%** |
| probe: `.` vs line terminators (97) | 96.9% | 100% | 100% | 96.9% | 96.9% | 100% | 100% (95%) | 97.9% | **100%** |
| probe: case folding, `ſ`/`K` (252) | 99.2% | 100% | 100% | 98.8% | 99.2% | 100% | 100% (86%) | 100% | **100%** |
| probe: Annex B (176) | 98.9% | 100% | 100% | 92.0% | 92.0% | 100% | 100% (56%) | 100% | **100%** |
| probe: `d` indices (57) | 0% | 100% | 100% | 100% | 100% | 100% | 100% (96%) | 100% | **100%** |
| probe: `v` set notation (105) | 0% | 100% | 32.4% | 27.6% | 10.5% | 100% | n/a | 10.5% | **100%** |
| **All cases (64,501)** | 99.3% | **100%** | 99.8% | 99.0% | 99.6% | **100%** | **100%** (80%) | 99.6% | **100%** |
| **npm patterns fully correct** | 1,997 / 2,056 | **2,056** | 2,052 | 2,009 | 2,013 | **2,056** | **1,717 / 1,717** | 2,029 | **2,056** |
| Compile errors where Node compiles | 246 | 0 | 21 | 459 | 74 | 0 | 0 | 74 | 0 |

The full per-feature table is generated by `d25 diff`. Its rows for duplicate named groups and modifiers are excluded from these totals, because Node 22 rejects both features. A spot check against the spec found that `libregexp` gets them right: groups `n="a"` then `n="b"` for `/(?<n>a)|(?<n>b)/g` on `"ab"`, and `/(?-i:a)b/i` matching `"aB"` but not `"AB"`.

**Root causes of the failures** (from the per-case diffs):

- **regexp2 (both versions), and goja through it:**
  - Captures inside quantified groups aren't reset each iteration: `((a)|b)+` on `"ab"` gives group 2 = `"a"`, where JS gives `undefined`.
  - `^`, `$` and `.` only treat `\n` as a line terminator, not `\r`, U+2028 or U+2029.
  - `\b` uses Unicode word characters.
  - Under `/i`, `ſ` matches `s`.
  - `\a` means BEL instead of `a`.
  - No `v` flag; Unicode properties are missing or come from older tables.
  - v1 also numbers named groups after unnamed ones.

  Everything except capture reset could be fixed by rewriting patterns before compiling. Capture reset is in the matcher itself.
- **goja additionally:**
  - No `d` or `v` flags.
  - `replace` ignores the `y` flag.
  - `split` mishandles U+2028.
  - Crashes with an uncatchable Go panic on `(?<n>a)|(?<n>b)`.
- **qjswasm (QuickJS-ng):** capture reset differs, the `v` flag and string properties (`\p{RGI_Emoji}`) are incomplete, and the timeout isn't honoured (see below).

## test262

| Group | Node 22 | quickjs VM (libregexp) | qjswasm | goja |
| --- | --- | --- | --- | --- |
| `regexp-unicode-property-escapes` | 595/679 | **679/679** | 531/679 | 516/679 |
| `regexp-v-flag` | 91/92 | **91/92** | 65/92 | 30/92 |
| `regexp-named-groups` | 98/98 | **98/98** | 98/98 | 86/98 |
| `regexp-lookbehind` | 17/17 | **17/17** | 17/17 | 17/17 |
| `regexp-match-indices` | 15/16 | **15/16** | 15/16 | 1/16 |
| `regexp-dotall` | 15/16 | **15/16** | 15/16 | 10/16 |
| `regexp-duplicate-named-groups` (ES2025) | 0/15 | **15/15** | 0/15 | 3/15 |
| `regexp-modifiers` (ES2025) | 160/230 | **230/230** | 157/230 | 160/230 |
| `RegExp/CharacterClassEscapes` | 12/12 | **12/12** | 12/12 | 0/12 |
| `language/literals/regexp` | 101/101 | **101/101** | 101/101 | 91/101 |
| String methods (`match`, `matchAll`, `replace`, `replaceAll`, `search`, `split`) | 309/332 | **332/332** | 331/332 | 332/332 |
| **All 2,557 files** | 2,325 (91%) | **2,543 (99%)** | 2,279 (89%) | 2,173 (85%) |
| **Tests Node 22 passes (2,325)** | 2,325 (100%) | **2,325 (100%)** | 2,229 (96%) | 2,094 (90%) |

test262 runs `libregexp` inside the QuickJS VM. The standalone `libregexp` wrapper is the same engine, and it agreed with the VM on all 64,585 corpus cases. Node's own misses are ES2025 features, `RegExp.escape`, and tests that need the `$262` host object.

## Performance

| Engine | Compile 2,056 patterns, cold | Per-line exec: median | vs Node, median | vs Node, p90 | Global match over 16 KB: median | vs Node, median | vs Node, p90 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| node (V8 JIT) | 90 ms | 0.04 µs | 1× | 1× | 0.35 µs | 1× | 1× |
| re2fast (83.5% of patterns) | 66 ms | 0.12 µs | 5.0× | 33× | 2.6 µs | 8.5× | 63× |
| **hybridlre** | 92 ms | 0.30 µs | 12× | 39× | 3.0 µs | 7.4× | 54× |
| libregexpopt | 31 ms | 0.34 µs | 8.8× | 57× | 6.6 µs | 5.9× | 129× |
| libregexp (no prefilter) | 29 ms | 2.0 µs | 64× | 115× | 344 µs | 977× | 2,304× |
| hybrid (RE2 + regexp2 v2) | 907 ms | 0.19 µs | 5.1× | 35× | 3.5 µs | 9.6× | 59× |
| regexp2 v2 | 768 ms | 0.43 µs | 12× | 24× | 5.1 µs | 11× | 37× |
| regexp2 v1 | 37 ms | 0.41 µs | 11× | 23× | 8.9 µs | 8.0× | 54× |
| quickjs VM | 32 ms | 2.8 µs | 86× | 161× | 406 µs | 1,180× | 2,725× |
| goja | 317 ms | 2.1 µs | 57× | 110× | 191 µs | 536× | 1,260× |
| qjswasm | 65 ms | 7.4 µs | 229× | 428× | 1,158 µs | 3,306× | 7,896× |

How to read this:

- **Ratios to V8 look large because V8's absolute times are tiny.** V8 compiles regexes to machine code and has prefilters, while every Go option is an interpreter. A median per-line `exec` costs 0.30 µs in the hybrid, against 0.04 µs in Node. A service doing 10 regex checks per request spends about 3 µs on them.
- **The prefilter mattered more than the engine.** `libregexp` has no start-anchor check, so a `/^…$/` pattern was tried at every position of the input. The wrapper now compiles patterns whose every alternative starts with `^` (without `m`) as sticky. That is provably equivalent, because such a pattern can only match where the search starts. It took the global-scan median from 344 µs to 6.6 µs and per-line from 2.0 to 0.34 µs, with all 64,585 cases still matching Node.
- **The p90 gap is literal and suffix scanning.** Patterns like `_(length|size)$` or `-?-?~?\}\}$` still try every position. A required-literal prefilter is the next wrapper optimisation and has not been measured.
- **Compile time: regexp2 v2 is about 20× slower than v1 or `libregexp`.** About 0.37 ms per pattern, which also rules it out for startup-heavy apps that build many regexes.

**Binary size** (stripped linux/amd64 hello-world importing the engine; base 1.60 MB):

| Engine | Size added |
| --- | --- |
| Go `regexp` | +0.30 MB |
| regexp2 v2 | +1.00 MB |
| `libquickjs` (for `libregexp`) | +3.21 MB |

The `libquickjs` cost is high because `libregexp` allocates and checks interrupts through a `JSContext`, which links much of QuickJS plus modernc's libc. Transpiling only the three regex files should remove most of it *(not verified)*.

**Cross-compilation:** `libregexp` via `libquickjs` builds cgo-free for linux, darwin and windows on amd64 and arm64. Only compilation was checked; it was run only on linux/amd64.

## Catastrophic backtracking

| Engine | `^(a+)+$` | `^(a\|aa)+$` | `(x+x+)+y` | email-style (CVE shape) |
| --- | --- | --- | --- | --- |
| Node 22, default | hung (killed at 15 s) | hung | hung | hung |
| Node 22, `--enable-experimental-regexp-engine-on-excessive-backtracks` | 1 ms | 2 ms | 2 ms | 3 ms |
| goja | 0 ms (RE2 path) | 0 ms | 0 ms | 0 ms |
| quickjs VM | aborted at 1 s | aborted | aborted | aborted |
| qjswasm | **hung; timeout not honoured** | hung | hung | hung |
| regexp2 v2 | aborted at 1.1 s | aborted | finished, 361 ms | aborted |
| libregexp | aborted at 1 s | aborted | aborted | aborted |
| **hybridlre** | **0 ms (RE2)** | **0 ms (RE2)** | **0 ms (RE2)** | aborted at 1 s (`libregexp`) |

The fast path makes most real patterns linear-time. That's a security property Node doesn't have by default: ReDoS is a recurring npm CVE class. Patterns RE2 can't take (here, the email pattern's capture inside a loop) fall back to `libregexp`, which is bounded by the interrupt-based timeout.

## The RE2 fast-path rule to adopt

A pattern goes to RE2 only if the translator ([`pattern.go`](../../spikes/D-25-regex/go/pattern.go)) accepts it. A call uses RE2 only if two input checks pass as well; otherwise that call falls back to `libregexp`.

**Reject the pattern** (send it to `libregexp`) if it has:

- the `v` flag, or `(?i:…)`-style modifiers;
- lookahead, lookbehind, or backreferences (`\1`, `\k<…>`);
- a capture group inside a quantified group, or a quantified capture group whose body can match empty. JS resets captures on every loop iteration and RE2 doesn't; that difference is what the `(a*)*b` probe exposed;
- with `i` but no `u`: a non-ASCII letter that has case variants. JS case-maps with `toUpperCase`, RE2 with simple folding, and the two differ for `ß`, `ſ`, `K` and Greek iota forms;
- with `iu`: `\w`, `\W` or `\b`. These include `ſ` and the Kelvin sign in JS's Unicode mode;
- surrogate escapes (`\uD83D` on its own); octal escapes and other Annex B escapes that aren't simple identity escapes; an astral literal followed by a quantifier, or an astral character in a class, without `u`;
- `\S` inside a class; duplicate group names, or names that aren't Go identifiers; repeat counts over 1,000;
- **`\p{…}` of any kind**. Go 1.26's Unicode tables are version 15.0 while Node 22's are 16.0, so property classes can disagree on newly assigned characters. The corpus didn't hit such a character, and an earlier version of the rule that translated `\p{…}` was also 100% correct. Excluding them anyway costs 9 patterns (83.9% → 83.5% coverage) and keeps one Unicode table in play.

**Translate everything else so RE2 matches JS exactly:**

| JS | RE2 |
| --- | --- |
| `.` | `[^\n\r  ]` (or any character with `s`) |
| `\s`, `\S` | the explicit JS whitespace class |
| `\d`, `\w` | ASCII classes |
| `^`, `$` without `m` | `\A`, `\z` |
| ASCII letters under `i` without `u` | explicit `[aA]`-style classes, case-closed ranges |
| `[^]` | any character |
| Annex B literal `{`, `}`, `]` and identity escapes | escaped literals |
| `iu` | Go's `(?i)` |

**Runtime checks (per call):**

- Without `u`, use RE2 only if the input has no astral characters. Non-`u` JS works on UTF-16 code units, RE2 on code points.
- With `m` and `^`/`$`, use RE2 only if the input has no `\r`, U+2028 or U+2029. JS treats those as line ends; RE2 only treats `\n` as one.

**Results:**

- RE2 handled 80% of corpus cases and matched Node on all of them (1,717 patterns; 50,440 inputs passed the runtime checks).
- Of inputs to accepted patterns, 96.7% passed the runtime checks.
- goja's own RE2 translation accepts more patterns (91%), but measured as a whole goja fails on line terminators, sticky and capture reset. Its rule is looser than safe.

**Execution:** RE2 runs from the vendored `regexp` with `FindStringSubmatchIndexAt(s, pos)`. The standard package can only start matching at the beginning of a string, and slicing the string first would break `\b`, `^` and lookbehind context at the slice point.

## Gaps to fix in the Nisse wrapper

- Transpile `libregexp.c`, `libunicode.c` and `cutils.c` on their own, with Go-implemented `lre_realloc` and `lre_check_timeout`. This removes the `JSContext` dependency and likely most of the 3.2 MB.
- Size the capture array with `lre_get_alloc_count`, not `capture_count * 2`. libregexp keeps loop registers there; the spike's first wrapper did the latter and segfaulted after about 4,000 calls.
- A required-literal or suffix prefilter, to close the p90 gap.
- Engine contexts aren't goroutine-safe, so keep one execution context per worker (per `P`). Compiled bytecode is read-only and could be shared *(not verified)*.
- The `groups` object for duplicate named groups: report the group that participated (the spike wrapper lists both).
- Fuzz `libregexp` through the wrapper. It is transpiled C that indexes raw memory, so an engine bug is memory corruption inside the Go process, not a Go panic.

## Risks

- **Supply chain.** `modernc.org/libquickjs` is a machine translation maintained by one person, tracking upstream QuickJS releases. Mitigations:
  - Pin it, and keep the option of re-running ccgo on upstream `libregexp.c` ourselves; the regex code is about 3k lines of C.
  - regexp2 v2 stays a documented fallback. Its gaps are known and mostly fixable by rewriting, except capture reset.
- **Memory safety.** See the fuzzing item above. The spike's own segfault was a wrapper bug, but it shows the failure mode.
- **Speed vs V8.** Per call the hybrid is a median 12× and a p90 39× slower than V8's JIT, in absolute terms 0.3 µs per call. Regex-heavy workloads (markdown, syntax highlighting, log parsing) will feel it. The literal prefilter, and possibly bytecode-to-Go code generation later, are the levers.
- **Unicode version skew.** Go's tables (15.0), V8's (16.0) and QuickJS's may disagree on new characters. Routing every `\p{…}` to `libregexp` keeps us to one table.
- **Ground truth lags the spec.** Node 22 lacks duplicate named groups, modifiers and `RegExp.escape`. Before relying on those features, re-check against a newer Node.

## Not verified

- Size and behaviour of a `libregexp`-only transpile, and sharing bytecode across contexts.
- Runtime behaviour on darwin, windows or arm64; only cross-compilation was checked.
- Benchmarks on a quiet or different machine. These ran beside other spikes at load average 2–4, as medians of 5 runs.
- Memory use under load.
- `replace` and `split` through the native wrappers (they were checked through the JS engines' harness only).
- Inputs with lone surrogates, which compat-mode UTF-16 views could produce in future.

## How to reproduce

```sh
spikes/D-25-regex/run.sh /path/to/workdir   # Node 22, Go (auto-fetches 1.26), network for npm/test262/Go modules
```

`run.sh` installs the npm corpus packages, sparse-clones test262 and builds the harness (`d25`, in `spikes/D-25-regex/go`). It then extracts the corpus, generates cases, computes Node ground truth, runs every engine, and writes `diff.md`, `re2cov.md`, `t262.md`, `bench.md` and `redos.tsv` to `<workdir>/out/`. A full run takes about 25 minutes on 4 cores. Individual steps can be re-run with the `d25` subcommands (`drive`, `diff`, `re2cov`, `t262`, `t262report`, `bench`, `redos`).

A second, from-scratch run in a fresh directory reproduced the conclusions:

- **Corpus:** npm installed slightly newer transitive versions, giving 2,064 patterns.
- **Correctness:** QuickJS, `libregexp` and `hybridlre` were 100% except one case, which was a harness artefact. A mutated input held a lone surrogate, which Node kept and Go's JSON decoding turned into U+FFFD. `gen.mjs` now filters those out.
- **RE2 coverage:** 84.0%.
- **test262:** identical.

`run.sh` now pins the top-level npm versions from `corpus-packages.json`; transitive versions can still drift.
