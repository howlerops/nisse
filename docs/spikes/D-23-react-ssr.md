# D-23 spike: React SSR inside a Go binary

Oct 19, 2026 · decision [D-23](../ledger/decisions.md) · ledger [W17-01](../ledger/wave-17-frontend-ssr.md)

**Question.** Can React's server renderer run fast enough inside an embedded, cgo-free JavaScript engine in a Go binary to back W17-07 (React SSR with hydration), and how much does rendering the same JSX natively in Go gain (W17-03, W17-09)?

## Answer

**No embedded, cgo-free engine is fast enough for React SSR as the main path.** The best one (modernc QuickJS) renders a typical 28 KB page at 28 pages per second per core, about **2% of Node**; the others reach about 1%. Rendering the same JSX natively in Go is **7.5–11.6× faster than Node**. The spike also shows Go can emit HTML **byte-identical** to `react-dom/server`, which is what hydration needs.

**Recommendation for D-23:**

1. **Make Go-native rendering the SSR path.** W17-03 lowers TSX components to Go, including the *initial render of client components*, not only stateless server components. W17-09 builds on it. W17-07 (React in an embedded engine) drops to a fallback.
2. **Fallback engine: QuickJS-NG on wazero (`fastschema/qjs`)**, with the join patch (three edits to React's `renderToString`, see Method). It is not the fastest per core: modernc is. But modernc serialises every instance on a process-wide `malloc` lock, so it stops scaling at about 1.3× however many cores it gets. QuickJS on wazero scales with cores and isolates each instance's memory. Use the fallback only for components that cannot be lowered, typically JS-only npm component libraries.
3. **Thresholds** (on the 28 KB reference page, per core, against Node 22 on the same machine):
   - Go-native SSR path: **≥ 1.5× Node throughput and p99 ≤ Node's**, the PRD's ported-performance target. The hand-written renderer reaches 7.5×, which leaves room for generated code to be up to about 5× slower than hand-written code.
   - Engine fallback: **≥ 25 renders/s and p99 ≤ 100 ms** before a route may use it in production. Today modernc passes (27.6/s, p99 58 ms) and QuickJS on wazero does not (16/s, p99 148 ms). Until it does, the fallback is for low-traffic routes and development.
4. **Node sidecar: justified in one narrow case only.** An app whose SSR depends on JS-only component libraries that cannot be lowered (for example CSS-in-JS runtimes or component kits shipped only as compiled JS), *and* that needs more than the fallback's ~16–28 pages/s/core. Keep it as a documented opt-in deployment mode, off by default, built only when a reference port needs it. It gives Node's throughput but breaks the one-binary goal.

## Results

Medians of 3 runs, 10 s per phase. Ranges are across runs where they matter. "Join" is the bundle with the join patch.

### Small page (100 rows, 28,123 bytes)

| Engine | Bundle | Renders/s (1 core) | p50 ms | p99 ms | Startup ms | MB / instance | Pool of 4, renders/s | vs Node (1 core) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Go-native | — | 10,884 | 0.06 | 0.48 | 0.06 | 0.07 | 17,575 | **7.5×** |
| Node 22 | stock | 1,457 | 0.64 | 1.27 | 1.4 | 1.8 ¹ | 3,674 | 1× |
| Node 22 | join | 1,134 | 0.80 | 1.75 | 1.7 | 1.8 ¹ | 3,241 | 0.78× |
| modernc QuickJS | stock | 27.6 | 34.4 | 58.3 | 43 | 1.7 | 32.5 | 1/53 |
| modernc QuickJS | join | 27.8 | 34.0 | 57.2 | 43 | 1.7 | 32.9 | 1/52 |
| QuickJS on wazero | stock | 15.8 | 56.9 | 181 | 76 ² | 5.3 | 27.9 | 1/92 |
| QuickJS on wazero | join | 16.0 | 51.5 | 148 | 76 ² | 5.6 | 33.4 | 1/91 |
| goja | stock | 6.9 | 144 | 205 | 8.6 | 7.3 | 8.0 | 1/211 |
| goja | join | 16.5 | 58.4 | 89.2 | 11 | 2.8 | 33.4 | 1/88 |

### Large page (800 rows, 223,319 bytes)

| Engine | Bundle | Renders/s (1 core) | p50 ms | p99 ms | Startup ms | MB / instance | Pool of 4, renders/s | vs Node (1 core) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Go-native | — | 1,595 (1,531–2,615) | 0.36 | 4.9 | 0.54 | 0.39 | 2,291 | **11.6×** |
| Node 22 | stock | 138 | 6.6 | 14.6 | 2.9 | 6.8 ¹ | 239 | 1× |
| Node 22 | join | 96 | 9.7 | 18.8 | 2.9 | 3.5 ¹ | 140 | 0.70× |
| modernc QuickJS | stock | 3.5 | 282 | 348 | 48 | 2.9 | 4.2 | 1/39 |
| modernc QuickJS | join | 3.6 | 272 | 395 | 47 | 2.9 | 4.2 | 1/38 |
| QuickJS on wazero | stock | 1.3 | 765 | 1,494 | 91 ² | 9.5 | 2.9 | 1/106 |
| QuickJS on wazero | join | 2.4 | 414 | 573 | 74 ² | 12.2 | 3.7 | 1/57 |
| goja | stock | 0.2 | 5,752 | 5,752 | 18 | 13.1 | 0.4 | 1/690 |
| goja | join | 1.8 | 566 | 636 | 14 | 7.5 | 2.5 | 1/76 |

¹ Node's figure is RSS growth per `vm` context inside one isolate, so it is not directly comparable with the per-engine-instance numbers.
² After the first instance in a process. The first one also compiles `qjs.wasm` with wazero's compiler: 0.9–1.2 s in the quick test.

### Multicore scaling (small page, total renders/s, `scale.sh`)

| Engine | 1 instance | 2 instances | 4 instances | 2-instance speed-up |
| --- | ---: | ---: | ---: | ---: |
| Go-native | 9,768 / 10,189 | 12,595 / 15,155 | 13,012 / 12,475 | 1.3–1.5× |
| goja (join) | 15.5 / 13.9 | 31.2 / 18.4 | 38.5 / 27.4 | 1.3–2.0× |
| modernc QuickJS | 27.1 / 27.1 | 35.2 / 33.5 | 32.9 / 32.9 | **1.2–1.3×, flat after** |
| QuickJS on wazero (join) | 17.9 / 16.6 | 30.5 / 32.9 | 30.6 / 52.5 | 1.7–2.0× |

Two rounds, shown as round 1 / round 2. The load average was 6.6–7.4 during this check, so 4-instance numbers are capped by other processes. The 2-instance column is the telling one: modernc plateaus while the others roughly double. The cause is visible in `modernc.org/libc` v1.77.1 `mem.go`: `Xmalloc`, `Xcalloc`, `Xrealloc` and `Xfree` all take one global `allocMu` mutex.

### Correctness

- **HTML:** all 54 renders checked (9 configurations × 3 runs × 2 page sizes) are byte-identical to Node's `renderToString` output, including the hand-written Go renderer.
- **Streaming:** `renderToReadableStream` completes in every engine (110 chunks for the large page, 14 for the small one), and the joined output is byte-identical to `renderToString`. It needs a `ReadableStream` polyfill, host-driven timers and a streaming-aware `TextDecoder`. A first `TextDecoder` shim that ignored `{ stream: true }` corrupted multi-byte characters split across chunks, which is the kind of bug the fallback's shims must be tested for. Streaming throughput was not benchmarked; in the quick test a full streamed render took 2–3× as long as `renderToString` in the QuickJS engines.

### Binary size

Stripped (`-s -w`), `CGO_ENABLED=0`, a program that evaluates `1+1`; Go-only hello world is 1.6 MB.

| Engine | Binary | Added over hello world |
| --- | ---: | ---: |
| modernc QuickJS | 5.3 MB | +3.7 MB |
| QuickJS on wazero | 6.2 MB | +4.6 MB (includes the 1.0 MB `qjs.wasm`) |
| goja | 10.5 MB | +8.9 MB |

### Findings worth carrying into the ledger

- **React's `renderToString` is quadratic in engines without rope strings.** It builds its result with `result += chunk`. On the large page it makes goja about 10× slower (p50 5.8 s vs 0.57 s) and QuickJS on wazero 1.8× slower. modernc's QuickJS is unaffected, and on Node the patch is *slower* (V8 has ropes). The join patch is three small build-time edits (`js/build.mjs`) and should ship with any engine fallback.
- **Byte-identical markup from Go is achievable.** The Go renderer has to reproduce these `react-dom/server` rules:
  - `<!-- -->` separators between adjacent text nodes
  - the `useId` format (`_R_2_` in React 19.3)
  - attribute renames (`className`, `htmlFor`)
  - `defaultValue`/`defaultChecked` becoming `value`/`checked`
  - boolean attributes rendered as `=""`
  - style objects serialised as `border-color:#f60`
  - escaping `& < > " '` as `&amp; &lt; &gt; &quot; &#x27;`

  These rules are what a W17-03 conformance suite must pin, per React version.
- **goja is quick to start and easy to embed, but the slowest at steady state** even with the patch. Its CPU profile is flat interpreter dispatch, allocation and GC, with no remaining hotspot to fix.


## Method

**App.** `js/app.jsx` is a React 19.3.1 page that exercises what real SSR pages do: two context providers, `useContext`, `useState`, `useMemo`, `useId`, conditional rendering, nested lists, escaping of user text, inline styles, boolean attributes, and a customer table. Two sizes:

- **Large page:** 800 rows, 223,319 bytes of HTML (UTF-8). A stress case: a big admin table.
- **Small page:** 100 rows, about 28 KB of HTML. Closer to a typical product or content page.

Data is generated deterministically (a float64 LCG), so every engine renders the same page.

**Bundles.** esbuild 0.28.2 bundles the app with `react-dom/server.browser` into one IIFE (`js/build.mjs`), minified, with `process.env.NODE_ENV = "production"`. A small prelude (`js/prelude.js`) adds what non-Node engines lack, only where missing: `TextEncoder`/`TextDecoder`, `setTimeout` as a host-drained queue, `MessageChannel`, `queueMicrotask`, `performance.now`, `process.env`. Three bundles:

| Bundle | Contents |
| --- | --- |
| `ssr.js` | Stock `renderToString` |
| `ssr-join.js` | Same, with one patch: React's `renderToString` accumulates output with `result += chunk`; the patch collects chunks in an array and joins once. It changes no output. |
| `ssr-stream.js` | `renderToReadableStream`, with `web-streams-polyfill` 4.3.0 supplying `ReadableStream` where missing |

**Engines.** All Go engines were built with `CGO_ENABLED=0`, which proves they are cgo-free.

| Engine | Version | What it is |
| --- | --- | --- |
| Node | 22.22.0 | V8 with JIT; the baseline. Runs the same bundle in `vm` contexts |
| goja | `dop251/goja` @ cf7b141 (Oct 2026) | ES2020+ interpreter written in Go |
| QuickJS on wazero | `fastschema/qjs` v0.0.6 | QuickJS-NG compiled to WASM (`qjs.wasm`, 1.0 MB, embedded), run by wazero v1.9.0's compiler. Verified: its `go.mod` depends only on wazero, and it builds with cgo off |
| modernc QuickJS | `modernc.org/quickjs` v0.25.0, `libquickjs` v0.13.1 (QuickJS 2026-06-04) | Bellard's QuickJS transpiled from C to Go by ccgo |
| Go-native | `go/native.go` | Hand-written `strings.Builder` renderer producing the same HTML: the ceiling for "server JSX lowered to Go" |

v8go (cgo, V8) was tried as a reference point and **not built**: its bundled libc++ needs a newer clang than this container has. Node runs V8 too, so it stands in as the V8 reference.

**Correctness checks.** For every engine and bundle, the HTML of one render is compared byte for byte with Node's (`run.sh` writes `html-check.txt`). The streaming check runs `renderToReadableStream` to completion in each engine, driving timers and microtasks from the host, and compares the result with that engine's `renderToString` output.

**Measurements** (`go/main.go`, `node/bench.mjs`):

- **Single-thread throughput and latency:** one instance, warm-up for a fifth of the window, then renders back to back for 10 s; renders per second, p50 and p99.
- **Startup:** creating an instance and evaluating the bundle; median of 5.
- **Memory per instance:** process RSS growth over 4 live instances that have each rendered once, divided by 4. RSS includes the WASM linear memory for QuickJS on wazero. For Node it is the growth per `vm` context, not per isolate.
- **Pool:** 4 instances on 4 goroutines (4 worker threads for Node), 10 s, total renders per second.

Each configuration ran 3 times per page size; tables show the median, with the range across runs where it matters.

**Noise.** The machine has 4 cores, and two other spikes ran benchmarks at the same time, so the load average at the start of each run was between 2.9 and 6.6 (recorded in `load.txt`), and 6.6–7.4 during the scaling check. Single-thread numbers are the most trustworthy; pool numbers are depressed by contention, and their ratios matter more than their absolute values. Re-run `run.sh` on an idle machine before quoting absolute pool throughput.

## Risks

- **React serialisation drifts between versions.** The `useId` format alone changed during React 19.x. A Go renderer that is off by one byte causes hydration mismatches. Mitigation: run this spike's HTML check as a CI differential test of the Go renderer against `react-dom/server`, for every supported React version.
- **JS-only component libraries cannot be lowered.** npm ships compiled JavaScript, not TSX, so `nisse port` has no source to lower for many component kits. Those components land on the slow fallback or the sidecar. Size this per reference app with the W13-01 dependency triage before committing W17-09's scope.
- **Generated Go will be slower than hand-written Go.** The 7.5–11.6× figure is a ceiling. Not measured: how close compiler output gets. The ≥ 1.5× threshold leaves a wide margin.
- **Not yet designed: Suspense and out-of-order streaming in Go.** That means React's `<template>` and inline-script protocol for streamed boundaries, plus CSS-in-JS style collection during SSR.
- **`fastschema/qjs` is young** (v0.0.6, October 2025). Its maintenance and the robustness of its memory and time limits were not verified. Plan to vendor or fork it if it becomes the fallback.
- **Noisy measurements.** Other spikes ran on the same 4-core machine. Single-thread ratios are consistent across runs (the ranges above are narrow); absolute pool throughput is not trustworthy.

## Not verified

- Pool scaling on an idle machine, and on more than 4 cores.
- v8go as a cgo reference (did not build here; Node stands in for V8).
- Streaming SSR throughput and time to first byte.
- macOS, Windows and arm64. Every number is from linux/amd64.
- The memory-limit and execution-time-limit options of the QuickJS engines.
- Performance of a *generated* Go renderer, as opposed to the hand-written one.

## How to reproduce

```sh
cd spikes/D-23-react-ssr
export WORK=/tmp/d23            # node_modules, bundles, binaries and results go here
RUNS=3 DUR=10s ROWS=800 ./run.sh   # large page
RUNS=3 DUR=10s ROWS=100 ./run.sh   # small page
ROUNDS=2 DUR=8s ./scale.sh         # multicore scaling, after run.sh
```

Needs Node 22+, Go 1.26+ and network access to npm and the Go proxy on the first run. Results land in `$WORK/results/*.jsonl` (one line per run), `html-check.txt` and `load.txt`; the script prints medians. `go test -run TestQuick -v ./go` with `DIST=$WORK/dist` gives a one-minute sanity check per engine, including streaming.
