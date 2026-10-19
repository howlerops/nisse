# Wave 14 · `nisse port`

PRD phase 7. The TypeScript → Nisse translator described in the PRD's [How a port works](../PRD.md#how-a-port-works).

**Gate (with waves 15 and 18):** the tier 1 reference app ports, passes its original tests and reaches ≥ 99.9% replay parity; port automation ≥ 90%.

## Foundations

- [ ] **W14-01** TypeScript front end (D-16): spike typescript-go through its API against our own TS-shaped parser plus a type dump from `tsc`; choose on fidelity, speed and API stability.
  *Needs:* W11-14. *Done when:* the choice is recorded in D-16 with the spike numbers.
- [ ] **W14-02** Project loader: `tsconfig.json` (including `extends`, `paths`, strictness flags, module settings), `package.json`, monorepos and workspaces.
  *Needs:* W14-01. *Done when:* the three reference app candidates load with full type information.
- [ ] **W14-03** Translation rules table: one entry per TypeScript construct, marked direct, rewrite-with-record, or `port` diagnostic. It drives the translator, the port report and the docs.
  *Needs:* waves 10–11. *Done when:* covers every construct in the TypeScript grammar; generated into `docs/spec/port-rules.md`.
- [ ] **W14-04** Translator core: TypeScript AST plus types → Nisse AST → printed `.nis`, keeping comments, blank lines and layout through `nisse fmt` (D-06).
  *Needs:* W14-03, W5-14. *Done when:* a 1k-line project round-trips with comments intact and the output checks clean in `compat`.

## Semantic rewrites

- [ ] **W14-05** Numbers: keep `number` as is; an optional pass turns provably integral values into `int` (used by `--tighten`).
  *Needs:* W14-04, W10-02. *Done when:* tests showing loop counters and lengths become `int` only under `--tighten`.
- [ ] **W14-06** Aliasing: run alias analysis and insert `Shared<T>` where the program relies on sharing (D-17).
  *Needs:* W10-13. *Done when:* a corpus of aliasing patterns ports with TypeScript behaviour preserved, and no `Shared<T>` where none is needed.
- [ ] **W14-07** Strings: `.length`, indexing and offset methods rewritten to UTF-16 views unless the text is provably ASCII or only compared with `0`.
  *Needs:* W10-21. *Done when:* tests with non-ASCII strings match Node.
- [ ] **W14-08** `null`/`undefined` kept as written; `==`/`!=` rewritten (`x == null` → a check against both).
  *Needs:* W10-04. *Done when:* tests for each `==` pattern.
- [ ] **W14-09** `any` → `unknown` with a `port` diagnostic suggesting concrete types inferred from usage.
  *Needs:* W10-05. *Done when:* suggestions applied by an agent check clean in tests.
- [ ] **W14-10** Exceptions: whole-program inference of what each function throws, to write `throws` clauses and type `catch`.
  *Needs:* W10-29. *Done when:* a ported project needs no manual `throws` edits for its own error classes.
- [ ] **W14-11** Concurrency: ported projects run on the event-loop executor (D-26); a report lists code that would be safe to run in parallel, for later tightening.
  *Needs:* W12-15. *Done when:* the report runs on the tier 1 app.
- [ ] **W14-12** Module graph: barrels, path aliases and cycles handed to the compiler (W10-24–W10-26) unchanged.
  *Needs:* W10-26. *Done when:* the reference app candidates' module graphs build.
- [ ] **W14-13** Dependencies: run triage (W13-01) and rewrite imports to the chosen tracks.
  *Needs:* W13-03. *Done when:* imports in ported code point at compat packages, ported sources or engine bundles as the lock file says.

## Output and workflow

- [ ] **W14-14** Port report: JSON and human output listing every untranslated construct with code, location, reason and suggested change, in the same shape as diagnostics (with `fixes[]`).
  *Needs:* W14-03, W2-09. *Done when:* an agent can work through the report with `nisse fix` and the check → fix loop.
- [ ] **W14-15** Incremental re-port: re-run on an updated TypeScript source and keep manual edits to the Nisse output (three-way merge using the last report), so a team can port while the original keeps changing.
  *Needs:* W14-14. *Done when:* re-porting after an upstream change keeps a set of manual edits in a test.
- [ ] **W14-16** `nisse port --tighten`: proposals that move ported code from `compat` towards `strict` (remove unneeded `Shared<T>`, infer `int`, `null` → `undefined`, parallel-safe code off the event loop).
  *Needs:* W14-05, W14-06, W14-11. *Done when:* applied to the tier 1 port with its tests still passing.
- [ ] **W14-17** Agent workflow docs for ports: an `AGENTS.md` for ported projects and a porting section in `llms-compat.txt`.
  *Needs:* W14-14, W10-34. *Done when:* an agent ports a 2k-line project to passing tests using only these docs.
- [ ] **W14-18** Port corpus: 20 small-to-mid open-source TypeScript repos ported nightly, tracking the automation rate and new `port` diagnostics.
  *Needs:* W14-14. *Done when:* nightly report published; regressions alarm.
