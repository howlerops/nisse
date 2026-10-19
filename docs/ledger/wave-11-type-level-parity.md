# Wave 11 · Type-level parity

PRD phase 5. TypeScript's type-level features, as listed in the PRD's [Type-level features](../PRD.md#type-level-features). All of them are computed in the checker; Go only sees concrete types.

**Gate (with wave 10):** agreement with `tsc --strict` ≥ 98% on conformance tests within the supported subset (W11-14).

- [ ] **W11-01** `keyof`, `typeof` in type positions, indexed access types (`T["k"]`, `T[number]`).
  *Needs:* W3-01. *Done when:* tests for each, including on generic parameters.
- [ ] **W11-02** Mapped types, including `readonly`/`?` modifiers with `+`/`-`, and key remapping with `as`.
  *Needs:* W11-01. *Done when:* `Partial`, `Required` and `Readonly` are definable in user code and match TypeScript.
- [ ] **W11-03** Conditional types with `infer`, including distribution over unions and `never`.
  *Needs:* W11-01. *Done when:* `ReturnType`, `Parameters` and `Awaited` are definable in user code and match TypeScript.
- [ ] **W11-04** Template literal types and the intrinsic string types (`Uppercase`, `Lowercase`, `Capitalize`, `Uncapitalize`).
  *Needs:* W11-03. *Done when:* tests for inference through template literal types.
- [ ] **W11-05** The standard utility types library (`Partial`, `Required`, `Readonly`, `Pick`, `Omit`, `Record`, `Exclude`, `Extract`, `NonNullable`, `ReturnType`, `Parameters`, `ConstructorParameters`, `InstanceType`, `Awaited`, `NoInfer`, …).
  *Needs:* W11-02, W11-03. *Done when:* each matches TypeScript on its conformance tests.
- [ ] **W11-06** `satisfies`, `as const`, `const` type parameters.
  *Needs:* W3-05. *Done when:* tests for each, including literal-type preservation.
- [ ] **W11-07** Type predicates (`x is T`), assertion functions (`asserts x is T`, `asserts x`), polymorphic `this` types.
  *Needs:* W3-21. *Done when:* narrowing tests through each.
- [ ] **W11-09** Intersection types, merged into one record shape for lowering; conflicting members become `never`.
  *Needs:* W3-02. *Done when:* tests including intersections of generic types.
- [ ] **W11-08** Branded types (`string & { __brand: "UserId" }`) and `unique symbol` brands, lowered to the underlying type.
  *Needs:* W11-09. *Done when:* brands are enforced by the checker and cost nothing in generated Go.
- [ ] **W11-10** Monomorphisation: generic functions and types that apply type-level operators to their own type parameters are instantiated per concrete type, deduplicated, with a code-size report.
  *Needs:* W11-02, W11-03, W4-20. *Done when:* a generic using `Partial<T>` and `keyof T` lowers to one readable Go function per instantiation; code-size report in CI.
- [ ] **W11-12** Reading `.d.ts` files, including DefinitelyTyped packages, for the engine boundary and compat packages.
  *Needs:* W11-05, W10-24. *Done when:* the `.d.ts` files of the top 50 npm packages load without checker errors.
- [ ] **W11-13** Recursive types and an instantiation-depth limit like `tsc`'s, reported as a diagnostic instead of a hang.
  *Needs:* W11-03. *Done when:* a pathological type gives the diagnostic within the check time budget.
- [ ] **W11-14** Conformance harness: run TypeScript's conformance tests filtered to the supported subset, compare accept/reject (and error positions) with `tsc --strict`, report agreement and keep a documented list of divergences.
  *Needs:* W11-01. *Done when:* the report is generated in CI.
- [ ] **W11-15** Checker performance with type-level features: the 500 ms target holds for a 10k-line `strict` project, and a separate budget is set and tracked for `compat` projects.
  *Needs:* W3-32. *Done when:* both budgets enforced in CI.
