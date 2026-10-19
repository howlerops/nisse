# Nisse

A strict, TypeScript-shaped language for coding agents that compiles to readable Go and ships as a single static binary.

> **Status:** pre-alpha. Nothing to install yet — this repo currently holds the design. See [docs/PRD.md](docs/PRD.md) for the full PRD and build plan, and [docs/ledger](docs/ledger/README.md) for the work ledger.

## Why

Agents write TypeScript well. Go runs servers well. Nisse sits in between: TS syntax agents already know, a strict checker (no `any`, `null` or `==`; every fallible call marked `try`), and diagnostics that return machine-applicable fixes. Everything after type-checking is handed to Go's toolchain, so you get Go's runtime, cross-compilation and ecosystem for free.

```ts
type Shape =
  | { kind: "circle", r: f64 }
  | { kind: "rect", w: f64, h: f64 }

function area(s: Shape): f64 {
  switch (s.kind) {
    case "circle": return Math.PI * s.r * s.r
    case "rect":   return s.w * s.h
  }
}
```

## Planned CLI

```
nisse check [path] [--json]   # type-check; JSON diagnostics with fixes
nisse fix [path]              # apply safe fixes
nisse run file.nis            # build and run
nisse build -o app            # emit Go and build one binary
nisse bind <go-module>        # typed declarations for any Go package
```

## Inspiration

Nisse borrows its agent-first ideas from [Tov](https://github.com/richiemcilroy/Tov), and takes a different back end: Go instead of a custom C runtime.

## License

[MIT](LICENSE)
