# Wave 12 · Node and Web API parity

PRD phase 6. Node's APIs and the Web globals, implemented in Go as `node:` modules, as listed in the PRD's [Runtime parity](../PRD.md#runtime-parity-node-and-web-apis). The same Go implementations are bound into the embedded JS engine (W13-08). Most items are independent and suit parallel contributors.

**Gate:** ported Node tests pass at ≥ 90% for each implemented module; the tier 1 reference app's API use is fully covered.

## Groundwork

- [ ] **W12-01** API coverage plan: rank `node:` APIs by use in the reference apps' dependency trees and by npm popularity; generate a coverage report from it.
  *Needs:* W13-01. *Done when:* the ranked list and report are committed and regenerated in CI.
- [ ] **W12-02** Node test harness: port the relevant tests from Node's own test suite (written against `node:test` and `node:assert`) per module, and report pass rates.
  *Needs:* W5-07, W10-32. *Done when:* runs in CI for every implemented module.
- [ ] **W12-15** Event-loop executor for the `compat` profile (D-26): one task at a time per loop, I/O on goroutines off the loop, Node's ordering of `process.nextTick`, promise microtasks, `setImmediate` and timers; several loops for apps without shared state.
  *Needs:* W6-04. *Done when:* Node's event-loop ordering tests pass, and a module-level counter mutated across awaits has no race under `-race`.

## Core modules

- [ ] **W12-03** `node:fs` and `node:fs/promises`: sync, callback and promise forms; streams; `stat`; `watch`.
  *Needs:* W12-07. *Done when:* ported tests ≥ 90% on all three operating systems.
- [ ] **W12-04** `node:path` (posix and win32), `node:os`, `node:url`, `node:querystring`.
  *Needs:* W12-02. *Done when:* ported tests ≥ 90%.
- [ ] **W12-05** `node:process`: `argv`, `env`, `exit` and exit codes, signals, stdio streams, `nextTick`, `hrtime`, `memoryUsage`, `uncaughtException` and `unhandledRejection`.
  *Needs:* W12-15. *Done when:* ported tests ≥ 90%.
- [ ] **W12-06** `node:events`: `EventEmitter` (including `once`, the `error` event and listener limits) and `EventTarget`.
  *Needs:* W12-02. *Done when:* ported tests ≥ 90%.
- [ ] **W12-07** `node:stream`: `Readable`, `Writable`, `Duplex`, `Transform`, `pipeline`, backpressure, object mode, async iteration; bridged to `io.Reader`/`io.Writer`.
  *Needs:* W12-06, W10-16. *Done when:* ported tests ≥ 90%.
- [ ] **W12-08** `node:buffer`: the full `Buffer` API with every encoding; `Blob`.
  *Needs:* W12-02. *Done when:* ported tests ≥ 90%.
- [ ] **W12-09** `node:http` and `node:https`, server and client, with Node's request and response API on `net/http`: keep-alive, chunked encoding, upgrades, trailers.
  *Needs:* W12-07. *Done when:* ported tests ≥ 90%, and an Express-style app (W16-01) runs on it.
- [ ] **W12-11** `node:crypto`: hashes, HMAC, `randomBytes`, `randomUUID`, ciphers, sign/verify, key objects, `scrypt`, `pbkdf2`, and Web Crypto `subtle`.
  *Needs:* W12-08. *Done when:* ported tests ≥ 90%.
- [ ] **W12-12** `node:zlib`: gzip, deflate, brotli, streaming forms.
  *Needs:* W12-07. *Done when:* ported tests ≥ 90%.
- [ ] **W12-13** `node:child_process`: `spawn`, `exec`, `execFile`, `fork` of Nisse programs, stdio piping.
  *Needs:* W12-07. *Done when:* ported tests ≥ 90% on all three operating systems.
- [ ] **W12-14** `node:timers` and `node:timers/promises`, including detached timers on the loop in `compat`.
  *Needs:* W12-15. *Done when:* ported tests ≥ 90%.
- [ ] **W12-16** `node:util` (`promisify`, `callbackify`, `inspect`, `format`, `types`, `parseArgs`), `node:assert`, `node:readline`, `node:string_decoder`, `node:perf_hooks`.
  *Needs:* W12-02. *Done when:* ported tests ≥ 90%.
- [ ] **W12-17** `node:async_hooks`: `AsyncLocalStorage` on context values.
  *Needs:* W12-15. *Done when:* context survives awaits, timers and event emitters in tests.

## Lower priority

- [ ] **W12-10** `node:net`, `node:tls`, `node:dns`, `node:dgram`.
  *Needs:* W12-07. *Done when:* ported tests ≥ 90% for the parts the reference apps use.
- [ ] **W12-18** `node:worker_threads` on goroutines with structured-clone message passing and `MessagePort`; `node:cluster` as several event loops.
  *Needs:* W12-15. *Done when:* ported tests ≥ 90% for the parts the reference apps use.

## Web globals and output

- [ ] **W12-19** Web globals: `fetch`, `Request`/`Response`/`Headers`, `FormData`, `URL`/`URLSearchParams`, `AbortController`/`AbortSignal`, `TextEncoder`/`TextDecoder`, Web Streams, `structuredClone`, `atob`/`btoa`, `crypto.subtle`, `Blob`/`File`, `queueMicrotask`, `performance`, `WebSocket`, `EventTarget`/`Event`.
  *Needs:* W12-07, W12-11. *Done when:* the relevant Web Platform Tests subsets pass at ≥ 90%.
- [ ] **W12-20** `console` parity: format specifiers and `util.inspect` output byte-identical to Node, so replay tests and logs match.
  *Needs:* W12-16. *Done when:* a fixed corpus of values prints identically to Node.
- [ ] **W12-22** Per-module fidelity report in CI, feeding the PRD's Node API fidelity metric.
  *Needs:* W12-02. *Done when:* the report is published per release.
