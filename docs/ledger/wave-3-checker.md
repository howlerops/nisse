# Wave 3 · Checker

PRD phase 1. The type checker and every rule in the PRD's [Semantics](../PRD.md#semantics) section. Each item is done only with positive and negative tests, and each diagnostic it introduces has a registered code (W2-07) and a fix where the PRD promises one.

**Gate:** every Semantics rule is enforced and tested; every emitted code has at least one test; checker benchmarks are in place.

## Types core

- [ ] **W3-01** Type representation, interning and a printer that writes types the way users wrote them.
  *Needs:* W2-03. *Done when:* round-trip tests for every type form.
- [ ] **W3-02** Assignability and structural typing for records and interfaces.
  *Needs:* W3-01. *Done when:* table tests for assignable/not assignable pairs, including excess-property errors on literals.
- [ ] **W3-03** Functions: parameters, optional and default parameters, rest parameters, arrows, closures, function types, return-type inference. No overloads.
  *Needs:* W3-02. *Done when:* tests for each, plus a missing-return error.
- [ ] **W3-04** Definite assignment, unreachable code and missing-return analysis.
  *Needs:* W3-03. *Done when:* negative tests for each.

## Numbers and conditions

- [ ] **W3-05** Numeric types: `int`, `f64`, sized ints, contextual literal typing, explicit conversions (`f64(x)`, `i32(x)`, `Math.trunc`).
  *Needs:* W3-01. *Done when:* `const x: f64 = 1` passes, `const y: int = 1.5` fails, conversions type-check.
- [ ] **W3-06** Mixed arithmetic error with `f64(a)`/`Math.trunc(b)` fixes; `int / int` error with `f64(a) / f64(b)` and `Math.idiv(a, b)` fixes.
  *Needs:* W3-05. *Done when:* applying each fix yields code that checks clean.
- [ ] **W3-07** Conditions must be `bool`: truthiness on optionals, numbers and strings is an error with a `safe` fix.
  *Needs:* W3-05. *Done when:* tests for each operand type, fixes applied in tests.

## Strings

- [ ] **W3-08** String API typing: `byteLength`, `runeCount()`, `byteAt`, `runes()`, `slice`/`indexOf`/`substring` on byte offsets, `for...of` over code points.
  *Needs:* W3-05. *Done when:* the API is listed in `docs/spec/strings.md` and each member has a test.
- [ ] **W3-09** `s.length` and `s[i]` errors with the fixes in the PRD (`safe` for `s.length` compared with `0`).
  *Needs:* W3-08. *Done when:* fixes applied in tests check clean.
- [ ] **W3-10** Template literals: allowed interpolation types (primitives and types with a `toString()` method).
  *Needs:* W3-08. *Done when:* a record without `toString()` in a template is an error.

## Values, mutability and equality

- [ ] **W3-11** `let`/`const`, deep immutability of `const` for value types, `readonly` fields; error with the `const` → `let` fix.
  *Needs:* W3-02. *Done when:* mutation through `const` (push, field write, nested write) is rejected and the fix works.
- [ ] **W3-12** Alias warning: a `let` copy is mutated and the original is read afterwards.
  *Needs:* W3-11. *Done when:* the warning fires in the canonical case, doesn't fire when the original is dead, and offers its fixes.
- [ ] **W3-13** `inout` parameters and `&x` arguments: only `let` bindings of value types; the same variable passed twice to one call is an error.
  *Needs:* W3-11. *Done when:* tests for each rule.
- [ ] **W3-14** Equality: `===`/`!==` need the same type; functions are not comparable; structural for values, identity for classes.
  *Needs:* W3-02. *Done when:* table tests for each type kind.
- [ ] **W3-15** `Map` key and `Set` element comparability check.
  *Needs:* W3-14. *Done when:* arrays, maps and functions as keys are rejected.

## Collections and intrinsics

- [ ] **W3-16** Array, `Map` and `Set` types with literal syntax and the indexing rules (`xs[i]` → `T`, `xs.at(i)`/`m.get(k)` → `T | undefined`).
  *Needs:* W3-02. *Done when:* tests for literals, indexing and iteration.
- [ ] **W3-17** Intrinsics table: generic signatures for built-in methods on arrays, `Map`, `Set`, strings, optionals and promises (`map`, `filter`, `reduce`, `find`, `flatMap`, `some`, `every`, `sort`, `join`, `includes`, …). One table drives the checker, lowering (W4-05) and docs.
  *Needs:* W3-16, W3-20. *Done when:* every intrinsic has a signature and a test; the list is generated into `docs/spec/intrinsics.md`.

## Classes, interfaces and generics

- [ ] **W3-18** Classes: fields, constructors, methods, `private`/`#` fields, `this`, `implements` checks, method values binding their receiver; `extends` error pointing at composition.
  *Needs:* W3-02. *Done when:* tests for each, including a missing `implements` member.
- [ ] **W3-19** Interfaces: structural, assignability, no generic methods.
  *Needs:* W3-02. *Done when:* tests for each.
- [ ] **W3-20** Generics: type parameters on functions and types, constraints, inference at calls; generic methods rejected with a fix to a free function; any generic shape Go can't express is a Nisse error.
  *Needs:* W3-03. *Done when:* inference tests plus a test for each Go-representability error.

## Unions, optionals and narrowing

- [ ] **W3-21** Control-flow narrowing: `typeof`, `instanceof`, comparisons with literals, `!== undefined`, early return, `switch`.
  *Needs:* W3-04. *Done when:* narrowing tests through `if`, `else`, `switch` and early returns.
- [ ] **W3-22** Tagged unions: find the discriminant (any shared field with distinct literal types).
  *Needs:* W3-21. *Done when:* tests with `kind`, `type` and a non-conventional field name.
- [ ] **W3-23** Literal unions as `enum` replacements.
  *Needs:* W3-21. *Done when:* string and int literal unions check and narrow.
- [ ] **W3-24** Untagged unions: accept only distinguishable members; untagged records error with an add-`kind` fix.
  *Needs:* W3-22. *Done when:* tests for accepted and rejected unions.
- [ ] **W3-25** Exhaustive `switch` with a fix inserting the missing `case`s; `never` type.
  *Needs:* W3-22, W3-23. *Done when:* the fix output checks clean.
- [ ] **W3-26** Optionals: `T | undefined`, optional fields `a?: T`, `?.`, `??`, narrowing.
  *Needs:* W3-21. *Done when:* tests for each.

## Errors

- [ ] **W3-27** Error types: classes and records implementing `Error`; `throw` checks its operand.
  *Needs:* W3-18. *Done when:* throwing a non-error is rejected.
- [ ] **W3-28** `throws` clauses and `try` marking: missing `try` error with an insert-`try` fix; escaping error not in `throws` with an add-to-`throws` fix.
  *Needs:* W3-27. *Done when:* both fixes applied in tests check clean.
- [ ] **W3-29** `try`/`catch`: `e` typed as the union thrown by the block, narrowing, rethrow, unhandled types must escape legally.
  *Needs:* W3-28, W3-21. *Done when:* tests for full handling, partial handling with rethrow, and an illegal escape.

## Quality and performance

- [ ] **W3-30** Checker test corpus organised by code; CI fails if a registered code has no test.
  *Needs:* W2-07. *Done when:* the CI check exists and passes.
- [ ] **W3-31** Incremental checking: cache results per module, recheck only changed modules and dependents, check modules in parallel.
  *Needs:* W3-01. *Done when:* a one-line change in a 10k-line project rechecks only the affected modules.
- [ ] **W3-32** Checker benchmark on a synthetic 10k-line project, warm and cold.
  *Needs:* W3-31. *Done when:* the benchmark runs in CI and reports numbers against the 500 ms target (enforced in W8-08).
