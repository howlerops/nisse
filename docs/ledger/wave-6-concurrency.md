# Wave 6 · Concurrency

PRD phase 3. Structured `async`/`await` on goroutines, as specified in the PRD's [Concurrency](../PRD.md#concurrency) rules.

**Gate:** async servers and CLIs work, cancel correctly, and leak no goroutines in tests.

- [ ] **W6-01** Typing for `async` functions, `Promise<T>`, `await` only inside `async`, `try await`.
  *Needs:* W3-03, W3-28. *Done when:* tests for each, including `await` outside `async`.
- [ ] **W6-02** Floating-promise error: an async call must be awaited, passed to a `Promise` combinator, or `spawn`ed. Fixes insert `await` or `spawn`.
  *Needs:* W6-01. *Done when:* both fixes applied in tests check clean.
- [ ] **W6-03** `spawn`: returns `Promise<T>`, scoped to the enclosing async function; never-awaited task warning.
  *Needs:* W6-01. *Done when:* tests for awaited, joined-at-scope-end and never-awaited tasks.
- [ ] **W6-04** `rt.Future[T]` and the lowering: goroutine per task, `context.Context` threaded as the first parameter of every async function, `await` as receive plus cancellation check.
  *Needs:* W6-01, W4-01. *Done when:* goldens and end-to-end tests.
- [ ] **W6-05** Structured scope join: an async function returns only after its spawned tasks finish; errors from tasks propagate.
  *Needs:* W6-03, W6-04. *Done when:* tests that a scope waits, and that a failing task's error reaches the caller.
- [ ] **W6-06** `Promise.all` (errgroup semantics, cancels siblings on first error), `Promise.race`, `Promise.allSettled`.
  *Needs:* W6-04. *Done when:* tests for cancellation of siblings.
- [ ] **W6-07** `nisse:context`: `timeout`, `deadline`, `cancelled()`, and `sleep(ms)` that respects cancellation. No detached `setTimeout`/`setInterval`.
  *Needs:* W6-04. *Done when:* tests for timeout propagation through nested calls.
- [ ] **W6-08** `async main` and `async` test bodies.
  *Needs:* W6-04, W5-07. *Done when:* both work end to end.
- [ ] **W6-09** Leak checking: goroutine leak detection in `rt` and end-to-end tests.
  *Needs:* W6-05. *Done when:* a deliberately leaking test fails.
- [ ] **W6-10** `--race` passthrough for `nisse test` and `nisse run`, and a doc section on sharing classes between tasks (D-05).
  *Needs:* W5-07. *Done when:* a racy class example is reported by `nisse test --race`.
