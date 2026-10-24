# Wave 8 · Measurement and release

Runs alongside phases 1–3: the agent suite, baselines and benchmarks that the PRD's gates depend on, then the 0.1 release. The harness and baselines (W8-01–W8-05) must exist before the Phase 1 gate.

**Gate (public 0.1):** suite ≥ 80% and the baseline targets in the PRD's [Success metrics](../PRD.md#success-metrics); HTTP ≥ 90% of Go; binary size within 10%; zero generated-Go errors.

## Agent suite

- [ ] **W8-01** Suite harness: task format (prompt, starter files, hidden tests), sandboxed runner, scoring, saved transcripts, at least 5 runs per task and language with 95% confidence intervals and paired comparisons. Language-agnostic, so the same task runs in Nisse, TypeScript and Go.
  *Needs:* W1-12, W1-14. *Done when:* one task runs end to end in all three languages and the report shows intervals.
- [ ] **W8-02** 50 tasks: 35 development tasks, plus 15 held out and written in Phase 3, after the spec freezes at the end of Phase 2. Include tasks that probe each TS divergence (aliasing, strings, division, map order) and each accepted divergence; set difficulty so strict TypeScript passes 50–65%.
  *Needs:* W8-01. *Done when:* all 50 committed with hidden tests; held-out tasks stored apart and never shown to the model during development.
- [ ] **W8-03** Baselines: run the suite in strict TypeScript (`tsc --strict` in the loop) and plain Go (`go vet` and `staticcheck` in the loop) with the same agent, model and harness.
  *Needs:* W8-02. *Done when:* baseline results committed with intervals.
- [ ] **W8-04** Pinned runs: agent and model versions recorded; results committed with dates under `suite/results/`.
  *Needs:* W8-01. *Done when:* two runs on different dates can be compared from the repo alone.
- [ ] **W8-05** Suite report: pass rate per language with intervals, escaped bugs, silent divergences, first-check diagnostics, fix-loop median, safe-fix coverage weighted by frequency, wrong-`safe`-fix rate, most frequent diagnostic codes.
  *Needs:* W8-03, W8-17, W8-18, W8-19. *Done when:* generated on every suite run.

## Benchmarks and budgets

- [ ] **W8-06** HTTP benchmark: a JSON route with request validation in Nisse vs hand-written Go `net/http`, Encore.ts (D-27) and the same TypeScript built with `bun build --compile`, same machine, published method.
  *Needs:* W7-11, W7-14. *Done when:* results recorded per release; ≥ 90% of Go for 0.1.
- [ ] **W8-07** Binary size check: stripped hello-world HTTP server vs plain Go in CI, with `bun build --compile` reported alongside.
  *Needs:* W1-10. *Done when:* CI fails above 10% overhead over Go.
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
- [ ] **W8-14** Windows coverage: CRLF sources, absolute backslash paths in `//line` directives, process and signal behaviour.
  *Needs:* W0-08, W4-23. *Done when:* dedicated end-to-end tests pass on the Windows runner.
- [ ] **W8-15** Docs site: language spec, stdlib reference, explain pages, `llms.txt` hosted at `nisselang.dev` (D-01).
  *Needs:* W5-04, W5-10, W7-17. *Done when:* published and linked from the README.
- [ ] **W8-16** Example projects: an HTTP JSON service, a CLI, and a service using a `go:` database driver.
  *Needs:* W7-10, W7-11. *Done when:* each builds in CI.
- [ ] **W8-17** Fix-loop measurement in the suite harness (moved from W5-15): count `check` → `fix` iterations to a clean build, report the median.
  *Needs:* W8-01. *Done when:* reported per suite run.
- [ ] **W8-18** Divergence metrics: tag hidden-test failures on builds that passed `check` with the TypeScript habit behind them (silent divergences), and count first-check diagnostics per task by code.
  *Needs:* W8-01. *Done when:* both reported per suite run.
- [ ] **W8-19** `safe`-fix correctness: apply every `safe` fix the suite triggers, re-run hidden tests, and report fixes that turn a pass into a fail.
  *Needs:* W8-01, W5-01. *Done when:* the rate is reported, and every fix that caused a failure has a bug filed.
- [ ] **W8-20** Distribution to agents: `go install`, an agent skill for Claude Code and Cursor that loads `llms.txt` and runs the check → fix loop, and a one-line setup in the README.
  *Needs:* W5-10, W5-11. *Done when:* a fresh agent session can install Nisse and finish one suite task from the skill alone.
- [ ] **W8-21** Adoption tracking for the parity gate: stars, installs and external repositories with Nisse code, reported 90 days after 0.1.
  *Needs:* W8-13. *Done when:* the 90-day report is committed and feeds the Phase 4 go/no-go.
- [ ] **W8-13** 0.1 release checklist: gate metrics met, docs published, domain secured, announcement written.
  *Needs:* everything above. *Done when:* tagged and announced.
