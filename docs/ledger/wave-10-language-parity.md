# Wave 10 · Language parity

PRD phase 5. The TypeScript features v1 leaves out, added back with Go lowerings, as listed in the PRD's [Language additions](../PRD.md#language-additions). Each item is done only when the feature is checked, lowered, golden-tested, run end to end, and reported correctly in both profiles.

**Gate (with wave 11):** every row of the Language additions table done; built-in fidelity ≥ 95% on the test262 subsets (W10-32).

## Profiles

- [ ] **W10-01** `nisse.json` with the `strict`/`compat` profile (D-22); every parity feature registers which profile reports it, and at what severity.
  *Needs:* W2-10. *Done when:* the same file gives errors under `strict` and warnings under `compat` for each parity feature.
- [ ] **W10-33** Strict-profile diagnostics for every parity feature, with fixes towards the strict idiom (`null` → `undefined`, `Shared<T>` → value or class, `extends` → composition where possible).
  *Needs:* W10-01. *Done when:* one test per feature; `safe` fixes where the rewrite is mechanical.

## Numbers and primitives

- [ ] **W10-02** `number` as an alias of `f64` with JavaScript semantics (D-19): ToInt32/ToUint32 for bitwise operators and shifts, `>>>`, `NaN`/`Infinity`, `toFixed`, `toString(radix)`, `Number.*` statics, number-to-string formatting identical to JavaScript.
  *Needs:* W3-05. *Done when:* test262 `Number` subset passes at ≥ 95%.
- [ ] **W10-03** `bigint` on `math/big`: `10n` literals, arithmetic, comparison, `BigInt()` conversion; no mixing with `number`.
  *Needs:* W3-05. *Done when:* test262 `BigInt` subset passes at ≥ 95%.
- [ ] **W10-04** `null` as a distinct unit type (D-18): `T | null`, `T | null | undefined`, `??` and `?.` over both; option kinds that keep the difference in lowering.
  *Needs:* W3-26. *Done when:* `x === null` and `x === undefined` behave as in TypeScript in end-to-end tests.
- [ ] **W10-05** `unknown` with `rt` dynamic values: narrowing by `typeof`, `instanceof`, `in` and type predicates; `JSON.parse` without a type returns `unknown`.
  *Needs:* W3-21. *Done when:* tests for every narrowing form and for property access without narrowing being an error.

## Classes and objects

- [ ] **W10-06** Class inheritance: `extends`, `super` calls and constructors, `override` checks, `abstract` classes and members, `protected`, `instanceof` across the hierarchy. Lowered with an embedded parent struct and a per-class interface held in a `self` field for virtual dispatch.
  *Needs:* W3-18, W4-15. *Done when:* a parent method calling an overridden method dispatches to the child, `class X extends Error` works with `catch`, and goldens are readable.
- [ ] **W10-07** `static` members, static blocks, class expressions.
  *Needs:* W10-06. *Done when:* tests for each, including static inheritance.
- [ ] **W10-08** Getters and setters on classes, including inherited and overridden accessors.
  *Needs:* W10-06. *Done when:* tests for each.
- [ ] **W10-09** `enum`: numeric with auto-increment and reverse mapping, string, `const enum`.
  *Needs:* W3-23. *Done when:* reverse mapping generated only when used; tests for each kind.
- [ ] **W10-10** Overloads: several signatures, one implementation, one Go function.
  *Needs:* W3-03. *Done when:* call resolution matches TypeScript for overload tests from the conformance suite (W11-14).
- [ ] **W10-11** Index signatures, `Record<K, V>`, computed keys in object literals.
  *Needs:* W3-16. *Done when:* lowered to `rt.Map`; tests for each.
- [ ] **W10-12** `Object.keys`/`values`/`entries`/`assign`/`fromEntries`/`freeze`, `obj[key]` with `keyof`, and the `in` operator over records, through generated per-type field tables.
  *Needs:* W11-01. *Done when:* key order matches JavaScript (declaration order) in tests.
- [ ] **W10-13** `Shared<T>`: a reference box with forwarded methods for arrays, maps, sets and records, plus the alias-analysis API `nisse port` uses to insert it (D-17).
  *Needs:* W4-04, W4-10. *Done when:* aliasing tests produce the same results as TypeScript.
- [ ] **W10-28** `globalThis`, module-level mutable state, `structuredClone`.
  *Needs:* W10-25. *Done when:* tests for each.
- [ ] **W10-30** Tuple extras: labelled, optional and rest elements; `readonly T[]` and `ReadonlyArray`.
  *Needs:* W3-33. *Done when:* tests for each.
- [ ] **W10-31** Remaining operators: `**`, `??=`, `||=`, `&&=`, `f?.()`, `delete` on optional fields and maps, `void`.
  *Needs:* W10-04. *Done when:* tests for each.

## Iteration and async

- [ ] **W10-14** Iteration protocols: `Symbol.iterator` and `Symbol.asyncIterator` on classes; `for...of` and spread over user iterables.
  *Needs:* W3-16. *Done when:* a user-defined iterable works in `for...of`, spread and `Array.from`.
- [ ] **W10-15** Generators: `function*`, `yield`, `yield*`, `.next()`/`.return()`/`.throw()`, lowered to `iter.Seq` and `iter.Pull`.
  *Needs:* W10-14. *Done when:* test262 generator subset at ≥ 95%.
- [ ] **W10-16** Async generators and `for await`, lowered to channels with a context.
  *Needs:* W10-15, W6-04. *Done when:* tests including early `break` releasing the producer.
- [ ] **W10-17** Promise chains (`.then`/`.catch`/`.finally`) and `new Promise((resolve, reject) => …)` bridged to `rt.Future`.
  *Needs:* W6-04. *Done when:* test262 `Promise` subset at ≥ 95%, and a callback-style API wrapped in `new Promise` works.

## Decorators

- [ ] **W10-18** TC39 standard decorators on classes, methods, fields and accessors, lowered to compile-time registration code (D-21).
  *Needs:* W10-06. *Done when:* the TC39 decorator examples behave as in TypeScript.
- [ ] **W10-19** Legacy `experimentalDecorators` with parameter decorators and `emitDecoratorMetadata` (`design:type`, `design:paramtypes`, `design:returntype`) as static metadata tables in `rt`, plus a `reflect-metadata`-compatible read API.
  *Needs:* W10-18. *Done when:* a minimal NestJS-style injector built on it resolves constructor dependencies in a test.

## Strings, regex, dates

- [ ] **W10-20** Regex literals and `nisse:regex` with JavaScript semantics: flags `d g i m s u v y`, named groups, lookbehind, backreferences; RE2 fast path when the pattern allows; `replace`/`match`/`matchAll`/`split`/`search` with regexes (D-25).
  *Needs:* W3-08. *Done when:* test262 `RegExp` subset at ≥ 95%.
- [ ] **W10-21** UTF-16 string views and JavaScript-compatible string methods: `utf16Length()`, `utf16At(i)`, `charCodeAt`, `codePointAt`, `normalize`, `localeCompare`, `padStart`, and the rest of `String.prototype`.
  *Needs:* W3-08. *Done when:* test262 `String` subset at ≥ 95%.
- [ ] **W10-22** `Date` with JavaScript semantics: parsing formats, local time and UTC, `toISOString`, getters and setters, invalid dates.
  *Needs:* W7-13. *Done when:* test262 `Date` subset at ≥ 95%.
- [ ] **W10-23** `Intl` subset on `golang.org/x/text`: `DateTimeFormat`, `NumberFormat`, `Collator`, `PluralRules` for common locales.
  *Needs:* W10-22. *Done when:* output matches Node for a fixed locale test matrix.

## Modules

- [ ] **W10-24** Module features: default exports and imports, `import * as`, `export * from`, `export { x as y }`, barrels, `import type`, JSON imports, `tsconfig` path aliases.
  *Needs:* W2-12. *Done when:* tests for each form.
- [ ] **W10-25** ESM evaluation order and top-level `await`, through an `rt` init sequence instead of Go `init()` order.
  *Needs:* W10-24. *Done when:* side-effect order across a diamond import graph matches Node.
- [ ] **W10-26** Circular imports: each strongly connected group of modules becomes one Go package, with name prefixes to avoid clashes (D-20).
  *Needs:* W10-25. *Done when:* a three-module cycle builds and runs, with evaluation order matching Node.
- [ ] **W10-27** `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry` on Go's `weak` package and `runtime.AddCleanup`.
  *Needs:* W4-01. *Done when:* tests for each (finalisation tests tolerate GC timing).

## Errors

- [ ] **W10-29** Error parity in `compat`: throwing any value (wrapped), `catch (e: unknown)`, `Error.cause`, `AggregateError`, and `stack` mapped to source lines.
  *Needs:* W3-29, W10-05. *Done when:* tests for each.

## Fidelity and docs

- [ ] **W10-32** test262 harness: translate the test262 subsets for `Array`, `String`, `Number`, `Math`, `JSON`, `Map`, `Set`, `Promise`, `RegExp`, `Date`, generators and iteration into Nisse tests, with a pass-rate report.
  *Needs:* W5-07. *Done when:* report generated in CI per subset.
- [ ] **W10-34** `llms-compat.txt`: the parity features, kept out of the core `llms.txt` so its token budget holds.
  *Needs:* W5-10. *Done when:* published with every example compiling in CI.
