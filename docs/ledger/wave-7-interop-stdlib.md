# Wave 7 · Go interop and standard library

PRD phase 3. `go:` imports through `nisse bind`, and the curated `nisse:` standard library. Rules are in the PRD's [Interop](../PRD.md#interop) section.

**Gate:** the interop test set (W7-10) builds and passes, and the stdlib covers what suite tasks need.

## `nisse bind` and `go:` imports

- [ ] **W7-01** `.d.nis` declaration file format: declarations only, with comments for skipped items.
  *Needs:* W2-03. *Done when:* specified in `docs/spec/declarations.md` and parsed by the checker.
- [ ] **W7-02** `nisse bind <go-module>` using `go/packages` and `go/types`: functions, methods, structs, interfaces, constants, variables, generics, variadics.
  *Needs:* W7-01. *Done when:* binds `net/http`, `os` and `strings` without errors.
- [ ] **W7-03** Type mapping rules: integer kinds, `float32` → `f32`, `float64`, `(T, error)` to `throws GoError`, struct pointers to classes and struct values (such as `time.Time`) to records, embedded fields and promoted methods, pointer vs value receivers, slices and maps copied at the boundary, function types with multiple returns, interfaces, Go constraints (`comparable`, `cmp.Ordered`, `~int | ~string`), Go → Nisse names lower-camel-cased, unexported members skipped; unmappable items skipped with a reason.
  *Needs:* W7-02. *Done when:* a table test per rule.
- [ ] **W7-04** Nil rules (non-nil with a nil error, `New*` constructors, overrides; slices and maps never optional) and boundary nil checks that trap with a clear message.
  *Needs:* W7-03. *Done when:* tests for each rule and for a trapping wrong assumption.
- [ ] **W7-05** Overrides file format per package: nilability, borrowed (zero-copy) slice parameters, renames.
  *Needs:* W7-03. *Done when:* specified and applied by `nisse bind`.
- [ ] **W7-06** Curated overrides for the Go standard library packages the stdlib and suite use.
  *Needs:* W7-05. *Done when:* `io`, `net/http`, `os`, `bufio`, `bytes`, `encoding/*` and `database/sql` have overrides.
- [ ] **W7-07** Declaration cache keyed by module version and Go version; warm checks never call `go/packages`; cold binds reported in `check` output.
  *Needs:* W7-02, W3-31. *Done when:* a warm check of a project with five `go:` imports meets the latency target.
- [ ] **W7-08** `go:` imports end to end in checker and lowering: direct Go calls, no wrapper layer.
  *Needs:* W7-02, W4-18. *Done when:* generated Go calls the Go package directly.
- [ ] **W7-09** `GoError` with `is`/`as` helpers over Go's `errors` package.
  *Needs:* W7-08. *Done when:* tests catching `os.ErrNotExist` and a typed Go error.
- [ ] **W7-10** Interop test set against popular modules: chi, pgx, `log/slog`, cobra, google/uuid, and one gRPC client.
  *Needs:* W7-08. *Done when:* a small program per module builds and passes in CI.

## Standard library (`nisse:`)

- [ ] **W7-11** `nisse:http`: server (routes, handlers, request and response, middleware, JSON helpers, graceful shutdown, a trap hook so a trap aborts only its request with the Nisse location logged) and a `fetch`-like client.
  *Needs:* W7-08, W6-04. *Done when:* the benchmark route (W8-06) and suite HTTP tasks run on it, and a trapping handler leaves the server running.
- [ ] **W7-12** `nisse:fs`: read, write, directory walk, paths, with typed errors.
  *Needs:* W7-08. *Done when:* tests on all three operating systems.
- [ ] **W7-14** `nisse:json`: generated codecs for records, tagged unions, literal unions, optionals and integer ranges; `parse<T>` and `stringify`; `JsonError` with a field path; unknown-field policy; JSON `null` decodes to `undefined` for optional fields and is an error elsewhere.
  *Needs:* W4-10, W4-16. *Done when:* round-trip and rejection tests, and decoding beats `encoding/json` reflection in a benchmark.
- [ ] **W7-15** `nisse:process`: args, env, exit codes, signals, running child processes.
  *Needs:* W7-08. *Done when:* tests on all three operating systems.
- [ ] **W7-16** `console` and `Math` globals, including `Math.idiv` and the wrapping-arithmetic functions.
  *Needs:* W4-11. *Done when:* documented and tested.
- [ ] **W7-13** `nisse:time` (instants, durations, time zones, formatting) and `nisse:crypto` (`randomUUID`, hashes, HMAC) (D-08).
  *Needs:* W7-08. *Done when:* documented and tested on all three operating systems.
- [ ] **W7-17** Stdlib reference docs generated from declarations and included in `llms.txt` within the token budget.
  *Needs:* W5-10. *Done when:* generated in CI.

## Editor support

- [ ] **W7-18** Language server, full: hover, completion, go-to-definition (including into `.d.nis` files), rename.
  *Needs:* W5-12, W7-07. *Done when:* each feature works in VS Code on a multi-module project.
