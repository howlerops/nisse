# Wave 5 · Agent toolchain

PRD phase 2. The write → check → fix loop and everything an agent or human uses around it.

**Gate:** median fix loop of 2 on the suite; 60% safe-fix coverage.

## Fixes and diagnostics

- [ ] **W5-01** Fix engine: apply edits, detect overlapping fixes, re-check after applying, honour `--safe-only`.
  *Needs:* W2-06. *Done when:* tests for conflicting fixes and for a fix that unlocks a second fix.
- [ ] **W5-02** `nisse fix [path] [--safe-only]`.
  *Needs:* W5-01. *Done when:* end-to-end test over a file with several fixable errors.
- [ ] **W5-03** Fix-coverage audit: list every code with its fix applicability, rank by how often it fires in the suite, and add fixes until coverage reaches 60%.
  *Needs:* W5-01, W8-05. *Done when:* the coverage report is generated in CI and meets the target.
- [ ] **W5-04** `nisse explain <code>` with long-form pages for every code, embedded in the binary.
  *Needs:* W2-07. *Done when:* CI fails if any registered code lacks a page; every page's example compiles (or fails with the right code).
- [ ] **W5-05** `nisse check [path] [--json]` as a stable command; JSON schema version in the output.
  *Needs:* W2-09. *Done when:* end-to-end tests for both outputs.
- [ ] **W5-06** `nisse emit [-o dir]`.
  *Needs:* W4-27. *Done when:* writes Go identical to what `nisse build` compiles.

## Tests and watch mode

- [ ] **W5-07** `test("…", () => …)` lowered to `_test.go`; `nisse test [--race] [-run pattern]` wrapping `go test`, with failures mapped to `.nis` lines.
  *Needs:* W4-23. *Done when:* a failing test reports the Nisse location and assertion message.
- [ ] **W5-09** `nisse --watch` for `run`, `test` and `check`, restarting servers on change.
  *Needs:* W3-31. *Done when:* an edit triggers a re-run within the check latency budget plus Go build time.

## Agent docs

- [ ] **W5-10** `llms.txt` v1: the spec as diffs from TypeScript, leading with "values are copied", then strings, numbers and the other [Semantics](../PRD.md#semantics) rules, then the command guide.
  *Needs:* waves 3–4 complete. *Done when:* under ~15k tokens (checked in CI), and every example compiles in CI.
- [ ] **W5-11** `AGENTS.md` template users drop into their repos.
  *Needs:* W5-10. *Done when:* an agent given only the template and `llms.txt` completes 3 suite tasks.
- [ ] **W5-08** (proposed, D-07) `nisse init`: scaffold `go.mod`, `main.nis`, `AGENTS.md`.
  *Needs:* W5-11. *Done when:* `nisse init && nisse run main.nis` works in an empty directory.
- [ ] **W5-14** (proposed, D-06) `nisse fmt`: one canonical layout; fixes produce formatted edits.
  *Needs:* W2-03. *Done when:* formatting is idempotent across the whole test corpus.

## Editor support

- [ ] **W5-12** Language server, first cut: diagnostics on change and fixes as code actions, from the same checker.
  *Needs:* W5-01, W3-31. *Done when:* works in VS Code and one other LSP client.
- [ ] **W5-13** VS Code extension: TextMate grammar for `.nis`, thin LSP client, published to the marketplace.
  *Needs:* W5-12. *Done when:* installable from the marketplace.

## Measurement

- [ ] **W5-15** Fix-loop measurement in the suite harness: count `check` → `fix` iterations to a clean build, report the median.
  *Needs:* W8-01. *Done when:* reported per suite run.
