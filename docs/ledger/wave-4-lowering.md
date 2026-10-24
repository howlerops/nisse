# Wave 4 · Lowering and runtime

PRD phase 1. The `rt` runtime library, lowering from the checked tree to Go, and the guarantees around the generated code. Each lowering item is done only when it has emit goldens, end-to-end tests and passes `go vet`.

**Gate (with waves 2–3):** suite at 60% pass; zero generated-Go errors in CI; copy-on-write and checked-arithmetic benchmarks recorded.

## Runtime library (`github.com/howlerops/nisse/rt`)

- [ ] **W4-01** `rt` skeleton: no dependencies outside Go's standard library, versioned in lockstep with the compiler, compatibility check between compiler and `rt` versions.
  *Needs:* W0-02. *Done when:* a mismatched version gives a clear build error.
- [ ] **W4-02** Checked arithmetic: generic, inlinable add/sub/mul/div/neg/conversion for every integer type; explicit wrapping ops; trap messages naming the operation.
  *Needs:* W4-01. *Done when:* `go build -gcflags=-m` shows the helpers inline, and a benchmark against plain arithmetic is recorded.
- [ ] **W4-03** `rt.Option[T]`.
  *Needs:* W4-01. *Done when:* unit tests and a benchmark against a plain pointer.
- [ ] **W4-04** `rt.Array[T]` with copy-on-write (sticky shared bit in the backing-store header) and the array intrinsics. First enumerate every Go copy site the lowering must mark (assignment, call, return, `range`, closure capture, interface boxing, container insert) and measure the cost.
  *Needs:* W4-01. *Done when:* the copy-site list is in `docs/spec/values.md`, aliasing tests pass under `-race`, and benchmarks against plain slices for read, append and copy-heavy loops are recorded (feeds D-02).
- [ ] **W4-05** Intrinsics implementation driven by the W3-17 table: `rt` free functions, with inline loops for the hottest (`map`, `filter`, `forEach`).
  *Needs:* W4-04, W3-17. *Done when:* every intrinsic has an end-to-end test.
- [ ] **W4-06** `rt.Map[K,V]` and `rt.Set[T]` with insertion order and copy-on-write, including deletion during iteration.
  *Needs:* W4-01. *Done when:* order tests across insert/delete/reinsert and benchmarks against Go maps.
- [ ] **W4-07** Equality: Go `==` where comparable; a generated `Equal` method otherwise.
  *Needs:* W4-04, W4-06. *Done when:* equality tests for nested records with arrays and maps.
- [ ] **W4-08** Trap reporting: every `rt` panic carries a Nisse-level message, and the top frame maps to the `.nis` line.
  *Needs:* W4-02. *Done when:* end-to-end tests for overflow, index out of range and divide by zero.

## Lowering

- [ ] **W4-09** Name mangling from the PRD's [Names](../PRD.md#names-in-generated-go) rules: exported names capitalised, public members of every type capitalised so classes satisfy Go interfaces, Go keyword and predeclared clashes.
  *Needs:* W2-15. *Done when:* a golden that uses every rule, plus a class whose `serveHTTP` method satisfies `http.Handler`.
- [ ] **W4-10** Records: named structs, generated names for anonymous shapes (deduplicated by shape), `json` tags, field-wise conversion between equal shapes.
  *Needs:* W4-09, W3-02. *Done when:* goldens plus a cross-module record test.
- [ ] **W4-11** Numbers: checked arithmetic calls, conversions, literals.
  *Needs:* W4-02, W3-05. *Done when:* goldens and trap tests.
- [ ] **W4-12** Strings and string intrinsics, `for...of` over runes.
  *Needs:* W3-08. *Done when:* tests with non-ASCII text.
- [ ] **W4-13** Control flow and functions: `if`/`for`/`for...of`/`while`, labelled `break`/`continue`, closures, arrows, default and rest parameters.
  *Needs:* W1-06, W3-03. *Done when:* goldens for each.
- [ ] **W4-14** `const`/`let`/`inout`: values copied where the checker can't prove sharing is safe; pointer parameters for `inout`.
  *Needs:* W3-11, W3-13, W4-04. *Done when:* aliasing end-to-end tests match the PRD's value rules.
- [ ] **W4-15** Classes and interfaces: pointer receivers, constructors as `NewX`, `implements` assertions, bound method values.
  *Needs:* W3-18, W3-19. *Done when:* goldens and tests.
- [ ] **W4-16** Unions: sealed interfaces with an unexported marker method, variant structs, literal unions as named types plus constants, wrappers for untagged unions, type switches, unreachable default.
  *Needs:* W3-22–W3-25. *Done when:* goldens for each union kind.
- [ ] **W4-17** Optionals: `rt.Option`, `?.`, `??`.
  *Needs:* W4-03, W3-26. *Done when:* goldens and tests.
- [ ] **W4-18** Errors: `(T, error)` returns, `try` as early return, error types with `Error() string`, `catch` as a type switch on Nisse error types (no reflection) with `errors.As` only for `GoError`, rethrow.
  *Needs:* W3-27–W3-29. *Done when:* goldens and tests, including errors that cross modules.
- [ ] **W4-20** Generics to Go type parameters.
  *Needs:* W3-20. *Done when:* goldens for generic functions and types with constraints.
- [ ] **W4-21** Copy elision: no copy when the source binding is never mutated after the copy (a `const` source or a last use), as proven by the checker.
  *Needs:* W4-14. *Done when:* goldens show elided copies, a `let` source mutated after a `const` copy still copies, and benchmarks improve on W4-04's baseline.
- [ ] **W4-22** Range analysis to drop overflow checks on loop counters, indexes bounded by length, and constants.
  *Needs:* W4-11. *Done when:* goldens show elided checks; benchmark recorded.

- [ ] **W4-28** Tuples as generated structs with positional fields.
  *Needs:* W3-33. *Done when:* goldens and end-to-end tests.
- [ ] **W4-29** Destructuring and spread: field reads, struct construction from spreads, `rt` array helpers.
  *Needs:* W3-34, W4-10. *Done when:* goldens show plain field reads (no reflection) and end-to-end tests pass.

## Generated-Go guarantees

- [ ] **W4-19** The emitter never produces unused imports or variables, or anything else `go build` or `go vet` rejects.
  *Needs:* W2-14. *Done when:* a golden corpus of unused-import and unused-variable cases produces Go that `go vet` accepts, inside W4-24.
- [ ] **W4-23** `//line` directives with absolute paths on every statement; stack traces, `go vet` output and Delve breakpoints point at `.nis` lines.
  *Needs:* W1-07. *Done when:* end-to-end tests for traces and vet output, including Windows paths and `-trimpath`; Delve checked by hand once per release.
- [ ] **W4-24** CI invariant: every checker test that passes `check` is lowered, built and vetted; any Go error fails CI as a compiler bug.
  *Needs:* W3-30. *Done when:* in CI, and a deliberately broken lowering fails it.
- [ ] **W4-25** Fuzzing: mutate the golden corpus (edit, re-check, build when it checks) and require `go build` to succeed; a generator of random well-typed programs follows after 0.1.
  *Needs:* W4-24. *Done when:* runs nightly; first findings fixed.
- [ ] **W4-26** Readability review checklist for emitted Go, applied to every golden before release.
  *Needs:* —. *Done when:* checklist committed in `docs/spec/emit-style.md`.
- [ ] **W4-27** Build driver: generated Go written to `.nisse/gen/`, built as its own module or mapped into the user's module with `-overlay` (whose JSON points at those on-disk files), `go.mod`/`go.sum` handling for the user's project.
  *Needs:* W1-08. *Done when:* `nisse build` writes nothing outside `.nisse/` and the output binary.
