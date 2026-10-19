# Wave 2 · Front end

PRD phase 1. The full grammar, module system and the diagnostics machinery every later wave reports through.

**Gate:** the whole v1 grammar parses with error recovery; every removed TypeScript feature has a diagnostic; diagnostics render as human text and JSON.

## Grammar and parser

- [ ] **W2-01** Write the v1 grammar (EBNF) in `docs/spec/grammar.md`, including the semicolon rule (D-12).
  *Needs:* W1-13. *Done when:* every construct in the PRD's language table appears in it.
- [ ] **W2-02** Full lexer: all operators, template literals, numeric literal forms (int vs float, `_` separators, hex/octal/binary), comments, CRLF input.
  *Needs:* W2-01. *Done when:* token goldens cover every form, and fuzzing (W2-11) runs clean for an hour.
- [ ] **W2-03** Full parser: declarations, statements, expressions with TS precedence, type syntax, `inout`/`&x`, `spawn`, `test(...)`.
  *Needs:* W2-02. *Done when:* AST goldens exist for each construct in the grammar.
- [ ] **W2-04** Parser error recovery, so one run reports several syntax errors instead of stopping at the first.
  *Needs:* W2-03. *Done when:* a file with five unrelated syntax errors reports all five.
- [ ] **W2-05** Diagnostics for every removed TypeScript feature: `any`, `null`, `==`/`!=`, `var`, `enum`, decorators, class `extends`, overloads, prototypes, declaration merging, side-effecting getters/setters, `eval`. Each has a code (`E0xxx`) and a fix or replacement pointer.
  *Needs:* W2-03, W2-06. *Done when:* one negative test per feature, and `safe` fixes where the PRD says there is a direct replacement (`==` → `===`, `var` → `let`, `enum` → literal union).
- [ ] **W2-11** Parser and lexer fuzzing with Go's native fuzzer.
  *Needs:* W2-03. *Done when:* a fuzz target runs in CI for a fixed time on each PR and the corpus is committed.

## Diagnostics infrastructure

- [ ] **W2-06** Diagnostic model: code, severity, primary and secondary spans, message, why, alternatives, `fixes[]` with `label`, `applicability` and text `edits[]`.
  *Needs:* W0-02. *Done when:* used by the parser and covered by unit tests.
- [ ] **W2-07** Code registry: one source file listing every code with title, area range (`0xxx`–`7xxx`), severity and explain-page path. CI fails if a code is emitted but not registered, registered without an explain page, or removed and reused.
  *Needs:* W2-06. *Done when:* the CI check catches each of those three mistakes in a test.
- [ ] **W2-08** Human renderer: location, source excerpt with carets, message, why line, alternatives; colour only on a TTY.
  *Needs:* W2-06. *Done when:* goldens for single-span, multi-span and fix-bearing diagnostics.
- [ ] **W2-09** JSON renderer with a versioned schema committed as a JSON Schema file; diagnostics sorted by file and position.
  *Needs:* W2-06. *Done when:* every JSON golden validates against the schema in CI.

## Modules and names

- [ ] **W2-10** Project discovery (D-10): find the `go.mod` root, map each directory to one module, collect `.nis` files.
  *Needs:* W0-01. *Done when:* nested directories become separate modules in a test project.
- [ ] **W2-12** Import resolution for relative paths, `go:` and `nisse:` specifiers, with an import-cycle error (Go forbids cycles, so Nisse must too).
  *Needs:* W2-10. *Done when:* tests for each specifier kind and a cycle diagnostic with the full cycle path.
- [ ] **W2-13** Scopes and name resolution: block scoping, shadowing, use before definition, duplicate declarations.
  *Needs:* W2-03. *Done when:* negative tests for each, with codes in `E1xxx`.
- [ ] **W2-14** Unused imports and variables are warnings in Nisse (Go makes them errors; the emitter must never produce them, see W4-19).
  *Needs:* W2-13. *Done when:* `W1xxx` warnings with `safe` fixes that remove them.
- [ ] **W2-15** Export rules and export-collision error (`foo` and `Foo` both exported).
  *Needs:* W2-13. *Done when:* a negative test for the collision.
