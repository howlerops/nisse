# D-23 spike: React SSR inside a Go binary

Oct 19, 2026 · decision [D-23](../ledger/decisions.md) · ledger [W17-01](../ledger/wave-17-frontend-ssr.md)

**Question.** Can React's server renderer run fast enough inside an embedded, cgo-free JavaScript engine in a Go binary to back W17-07 (React SSR with hydration), and how much does rendering the same JSX natively in Go gain (W17-03, W17-09)?

RESULTS_PLACEHOLDER

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

**Noise.** The machine has 4 cores, and two other spikes ran benchmarks at the same time, so the load average sat between LOAD_RANGE. Single-thread numbers are the most trustworthy; pool numbers are depressed by contention, and their ratios matter more than their absolute values. Re-run `run.sh` on an idle machine before quoting absolute pool throughput.

## How to reproduce

```sh
cd spikes/D-23-react-ssr
export WORK=/tmp/d23            # node_modules, bundles, binaries and results go here
RUNS=3 DUR=10s ROWS=800 ./run.sh   # large page
RUNS=3 DUR=10s ROWS=100 ./run.sh   # small page
```

Needs Node 22+, Go 1.26+ and network access to npm and the Go proxy on the first run. Results land in `$WORK/results/*.jsonl` (one line per run), `html-check.txt` and `load.txt`; the script prints medians. `go test -run TestQuick -v ./go` with `DIST=$WORK/dist` gives a one-minute sanity check per engine, including streaming.
