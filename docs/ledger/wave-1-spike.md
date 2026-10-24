# Wave 1 · Spike

PRD phase 0 (weeks 1–2). A thin vertical slice from `.nis` source to a cross-compiled binary, to prove the pipeline before building it properly. Code here may be rewritten in waves 2–4; what must survive is the shape of the pipeline and the test harness.

**Gate:** a hello-world HTTP server written in Nisse builds and runs on Linux, macOS and Windows, and the paper-compiler experiment (W1-14) reaches its build decision. If it says reshape or stop, the PRD changes before Phase 1 starts.

- [ ] **W1-01** Write the 10-line spec of the spike subset: `function`, `int`/`f64`/`string`/`bool`, records, `let`/`const`, `if`/`for`/`while`, `return`, `console.log`, and one hand-declared `go:net/http` import.
  *Needs:* —. *Done when:* committed to `docs/spec/spike.md`.
- [ ] **W1-02** Write 5 sample programs covering the subset, one of them the HTTP server.
  *Needs:* W1-01. *Done when:* each has expected output for the end-to-end harness (W0-05).
- [ ] **W1-03** Lexer for the subset with byte-offset spans on every token.
  *Needs:* W0-02. *Done when:* token golden tests cover every token kind in the subset.
- [ ] **W1-04** Recursive-descent parser for the subset (D-11), spans on every node.
  *Needs:* W1-03. *Done when:* the 5 samples parse and an AST dump golden exists for each.
- [ ] **W1-05** Minimal type resolution for the subset: enough to pick Go types for lowering. No real checker yet.
  *Needs:* W1-04. *Done when:* every expression in the samples has a resolved type.
- [ ] **W1-06** Lower the subset to `go/ast` and print with `go/format`.
  *Needs:* W1-05. *Done when:* emitted Go for each sample matches its golden and passes `go vet`.
- [ ] **W1-07** Emit `//line` directives with absolute paths (Go 1.27 resolves relative ones against the generated file's directory) and prove a runtime panic reports the `.nis` file and line.
  *Needs:* W1-06. *Done when:* an end-to-end test asserts the panic location, including with `-trimpath`.
- [ ] **W1-08** Build driver: write generated Go to `.nisse/gen/` and run `go build`, optionally mapping the files into the user's module with `-overlay` (its JSON must point at files on disk).
  *Needs:* W1-06. *Done when:* `nisse run sample.nis` prints the expected output.
- [ ] **W1-09** `nisse build -o app --target os/arch`, mapping to `GOOS`/`GOARCH` and refusing anything but `amd64` and `arm64`, including when the `GOARCH` environment variable overrides `--target`.
  *Needs:* W1-08. *Done when:* builds for all six v1 targets succeed, and `--target linux/386` or `GOARCH=wasm` gives a Nisse error.
- [ ] **W1-10** Hello-world HTTP server through `go:net/http` using hand-written declarations.
  *Needs:* W1-08. *Done when:* the binary answers `GET /` with the expected body.
- [ ] **W1-11** CI matrix that builds the HTTP sample on each OS and curls it.
  *Needs:* W1-10, W0-03. *Done when:* green on Linux, macOS and Windows.
- [ ] **W1-12** Draft the first 10 agent-suite tasks (prompt, starter files, hidden tests). Format defined in W8-01, drafted loosely here.
  *Needs:* —. *Done when:* 10 tasks committed under `suite/tasks/`.
- [ ] **W1-13** Spike retro: write down what the full parser, checker and driver must do differently, and choose the codegen path: `go/ast` + `go/format` with `//line` added in a text pass, or a small text printer run through `go/format`.
  *Needs:* W1-01–W1-11. *Done when:* notes and the codegen choice are committed, and any new work is added to waves 2–4.
- [ ] **W1-14** Paper-compiler experiment, the bet check that gates Phase 1. Write the ~3k-token `llms.txt` core (divergences as wrong → right pairs) and a fake `nisse check --json` / `nisse fix`: `tsc --strict` on lightly rewritten Nisse plus a lint pass that emits the top divergences (`const` mutation, string `.length`, `int / int`, mixed numbers, truthiness, missing `try`, `==`, `null`, `any`) as coded JSON diagnostics with `safe` edits. Run 20 tasks with hidden tests, 3 seeds each, in four arms: Nisse with fixes, Nisse with prose-only diagnostics, strict TypeScript with `tsc`, and Go with `go vet`/`staticcheck`.
  *Needs:* W1-12. *Done when:* results are committed with the decision: build (Nisse ≥ Go, ≥ TypeScript − 5, median ≤ 2 iterations, ≥ 60% of first-check diagnostics covered by `safe` fixes); reshape (soften the divergences that dominate, such as value semantics if they fire on more than 20% of tasks); or stop (Nisse < Go, fixes add nothing over prose, or silent divergences on more than 2 tasks).
