# Wave 8 · Measurement and release

Runs alongside phases 1–3: the agent suite, baselines and benchmarks that the PRD's gates depend on, then the 0.1 release. The harness and baselines (W8-01–W8-05) must exist before the Phase 1 gate.

**Gate (public 0.1):** suite ≥ 80% and the baseline targets in the PRD's [Success metrics](../PRD.md#success-metrics); HTTP ≥ 90% of Go; binary size within 10%; zero generated-Go errors.

## Agent suite

- [ ] **W8-01** Suite harness: task format (prompt, starter files, hidden tests), sandboxed runner, scoring, saved transcripts. Language-agnostic, so the same task runs in Nisse, TypeScript and Go.
  *Needs:* W1-12. *Done when:* one task runs end to end in all three languages.
- [ ] **W8-02** 50 tasks: 35 development tasks, plus 15 held out and written after the spec freezes. Include tasks that probe each TS divergence (aliasing, strings, division, map order).
  *Needs:* W8-01. *Done when:* all 50 committed with hidden tests; held-out tasks stored apart and never shown to the model during development.
- [ ] **W8-03** Baselines: run the suite in strict TypeScript (Node) and plain Go with the same agent, model and harness.
  *Needs:* W8-02. *Done when:* baseline results committed.
- [ ] **W8-04** Pinned runs: agent and model versions recorded; results committed with dates under `suite/results/`.
  *Needs:* W8-01. *Done when:* two runs on different dates can be compared from the repo alone.
- [ ] **W8-05** Suite report: pass rate per language, escaped bugs (hidden-test failures on builds that passed `check`), fix-loop median, safe-fix coverage weighted by frequency, most frequent diagnostic codes.
  *Needs:* W8-03, W5-15. *Done when:* generated on every suite run.

## Benchmarks and budgets

- [ ] **W8-06** HTTP benchmark: a JSON route with request validation in Nisse vs hand-written Go `net/http` and vs Encore.ts (D-27), same machine, published method.
  *Needs:* W7-11, W7-14. *Done when:* results recorded per release; ≥ 90% of Go for 0.1.
- [ ] **W8-07** Binary size check: stripped hello-world HTTP server vs plain Go, in CI.
  *Needs:* W1-10. *Done when:* CI fails above 10% overhead.
- [ ] **W8-08** Check-latency budget in CI on the 10k-line project (W3-32), with a regression alarm.
  *Needs:* W3-32. *Done when:* CI fails above 500 ms warm on the reference runner.
- [ ] **W8-09** Runtime microbenchmarks tracked over time: copy-on-write arrays, ordered maps, checked arithmetic, futures.
  *Needs:* W4-04, W4-06, W4-02, W6-04. *Done when:* trend recorded per release.

## Release

- [ ] **W8-10** Install: `go install github.com/howlerops/nisse/cmd/nisse@latest` and prebuilt binaries with checksums for all six targets.
  *Needs:* W1-09. *Done when:* a release workflow publishes binaries from a tag.
- [ ] **W8-11** Versioning policy: semver before 1.0, compiler and `rt` in lockstep, JSON diagnostics schema versioned separately.
  *Needs:* W4-01, W2-09. *Done when:* documented in `docs/versioning.md`.
- [ ] **W8-12** Supply-chain checks: `govulncheck` in CI, dependency review.
  *Needs:* W0-03. *Done when:* in CI.
- [ ] **W8-14** Windows coverage: CRLF sources, backslash paths in `//line` directives, process and signal behaviour.
  *Needs:* W0-08, W4-23. *Done when:* dedicated end-to-end tests pass on the Windows runner.
- [ ] **W8-15** Docs site: language spec, stdlib reference, explain pages, `llms.txt` hosted at `nisselang.dev` (D-01).
  *Needs:* W5-04, W5-10, W7-17. *Done when:* published and linked from the README.
- [ ] **W8-16** Example projects: an HTTP JSON service, a CLI, and a service using a `go:` database driver.
  *Needs:* W7-10, W7-11. *Done when:* each builds in CI.
- [ ] **W8-13** 0.1 release checklist: gate metrics met, docs published, domain secured, announcement written.
  *Needs:* everything above. *Done when:* tagged and announced.
