# Wave 0 · Foundations

PRD phase 0 (week 1). The repo, build and test scaffolding everything else stands on.

**Gate:** `go test ./...` passes in CI on Linux, macOS and Windows, and the golden-test harness runs.

- [ ] **W0-01** Create `go.mod` for `github.com/howlerops/nisse` and pin the minimum Go version (D-09).
  *Needs:* —. *Done when:* `go build ./...` works on a fresh clone.
- [ ] **W0-02** Lay out the repo: `cmd/nisse`, `internal/{syntax,check,lower,emit,diag,driver,bind}`, `rt/`, `std/`, `testdata/`, `suite/`, `docs/spec/`.
  *Needs:* W0-01. *Done when:* the layout is documented in the README and each package has a `doc.go` stating its job.
- [ ] **W0-03** CI on GitHub Actions: `gofmt`, `go vet`, `staticcheck`, `go test -race` on Linux, macOS and Windows.
  *Needs:* W0-01. *Done when:* a pull request shows all checks and a deliberately broken commit fails them.
- [ ] **W0-04** Golden-test harness: `testdata/**/*.nis` with expected `.go` output and expected diagnostics JSON, regenerated with `go test ./... -update`.
  *Needs:* W0-02. *Done when:* a sample golden test passes, and changing its expected file makes it fail with a readable diff.
- [ ] **W0-05** End-to-end harness: compile and run a `.nis` program, compare stdout, stderr and exit code with expected files.
  *Needs:* W0-04. *Done when:* runs in CI on all three operating systems.
- [ ] **W0-06** Contributor guide for agents and humans (`AGENTS.md`/`CLAUDE.md` at repo root): how to build, test, update goldens, add a diagnostic code, and where the ledger is.
  *Needs:* W0-02. *Done when:* a fresh agent session can add a golden test using only that file.
- [ ] **W0-07** `CONTRIBUTING.md` and `SECURITY.md`.
  *Needs:* —. *Done when:* both are linked from the README.
- [ ] **W0-08** Line-ending and path hygiene: `.gitattributes` forcing LF in `testdata`, path handling tested on Windows.
  *Needs:* W0-03. *Done when:* goldens pass on the Windows runner.
