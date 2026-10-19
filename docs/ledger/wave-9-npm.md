# Wave 9 · npm escape hatch (v2)

PRD phase 4. Pure-JS npm packages run in an embedded QuickJS-on-wazero engine. Starts only if suite data shows npm access is a top blocker.

**Gate:** 20 popular pure-JS packages pass their tests through Nisse.

- [ ] **W9-01** Go/no-go: review suite data and user requests; record the decision in [decisions.md](decisions.md).
  *Needs:* W8-05. *Done when:* the decision is logged.
- [ ] **W9-02** Engine spike: QuickJS compiled to WASM running on wazero, compared with goja for startup, call overhead and memory.
  *Needs:* W9-01. *Done when:* numbers recorded and the engine chosen.
- [ ] **W9-03** `npm:` import specifier, bundled at build time with esbuild's Go API (D-13) and embedded in the binary.
  *Needs:* W9-02, W2-12. *Done when:* a program importing one package builds into a single binary with no Node installed.
- [ ] **W9-04** `Js` type: checked conversion with `as T` (traps on mismatch), every call into JS marked `try`.
  *Needs:* W9-03. *Done when:* tests for successful and failing conversions.
- [ ] **W9-05** Engine pool, one instance per worker goroutine, with limits and reuse.
  *Needs:* W9-02. *Done when:* a concurrent HTTP benchmark calling into JS runs without data races.
- [ ] **W9-06** Minimal Node shim in Go: `Buffer`, `URL`, `TextEncoder`/`TextDecoder`, `crypto.randomUUID`, timers.
  *Needs:* W9-02. *Done when:* each shim has tests; growth is driven by package failures.
- [ ] **W9-07** Unsupported-package detection: native addons, `worker_threads`, raw sockets and child processes give a Nisse error at build time.
  *Needs:* W9-03. *Done when:* tests for each category.
- [ ] **W9-08** (proposed) Generate `Js`-boundary declarations from a package's `.d.ts`.
  *Needs:* W9-04. *Done when:* decided, then tested on the 20 packages.
- [ ] **W9-09** Compatibility set: 20 popular pure-JS packages (validators, date libraries, utility libraries) with their tests run through Nisse in CI.
  *Needs:* W9-03–W9-06. *Done when:* all 20 pass.
