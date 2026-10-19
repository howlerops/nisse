# Wave 17 · Frontend and SSR

PRD phase 9. Full-stack apps as one binary, as described in the PRD's [Frontend and SSR](../PRD.md#frontend-and-ssr). The highest-risk wave. The research gate (W17-01) is decided: render in Go, with an embedded engine as a fallback.

**Gate (with wave 18):** the tier 2 app's React frontend renders in Go and hydrates cleanly; the tier 3 app ships as one binary and passes its tests, frontend included.

## Research gate

- [x] **W17-01** SSR spike (D-23). Done by the [D-23 spike](../spikes/D-23-react-ssr.md): Go-native rendering is the main path, the embedded engine a fallback.

## Server rendering in Go

- [ ] **W17-02** TSX syntax and typing: intrinsic elements, components, props, `children`, fragments, keys.
  *Needs:* W17-01. *Done when:* React's JSX typing tests from the TypeScript conformance suite pass at ≥ 98%.
- [ ] **W17-03** Server JSX rendering in Go, the main SSR path: components, including the first render of client components (initial state, `useId`, context, `Suspense` boundaries), lowered to Go that streams HTML with React's escaping, attribute, style and comment-marker rules.
  *Needs:* W17-02. *Done when:* output matches `react-dom/server` byte for byte on a corpus that includes the D-23 benchmark app, at ≥ 1.5× Node's throughput with p99 no worse than Node's.
- [ ] **W17-14** Hydration tests: render in Go, hydrate with React in a headless browser, fail on any hydration warning; run for every supported React release.
  *Needs:* W17-03, W17-06. *Done when:* in CI for the corpus and the tier 2 app's pages.

## Shared code and client bundling

- [ ] **W17-04** Nisse → JavaScript emitter for shared modules (types, validation, utilities), keeping Nisse's value semantics.
  *Needs:* W17-01. *Done when:* the same validation module runs in Go and in the browser with identical test results.
- [ ] **W17-05** `.d.ts` output for Nisse modules, so TypeScript client code can import their types.
  *Needs:* W17-04. *Done when:* a client imports a Nisse API's types and `tsc` checks it.
- [ ] **W17-06** Client bundling: embedded esbuild builds client TS/TSX/CSS; assets embedded with `embed.FS`; a dev server with hot reload in `--watch`.
  *Needs:* W17-01, D-13. *Done when:* a Vite-style React SPA and its API ship as one binary with no Node at runtime.
- [ ] **W17-12** CSS tooling: Tailwind and PostCSS as a build step or embedded plugins.
  *Needs:* W17-06. *Done when:* a Tailwind app builds without Node installed.

## React SSR and frameworks

- [ ] **W17-07** Embedded-engine fallback for components that cannot be translated: React in QuickJS on wazero with the join patch, streaming render, engine pool, data from Nisse loaders.
  *Needs:* W17-06, W9-02. *Done when:* an untranslatable component renders inside a Go-rendered page, and the fallback meets D-23's production gate (25 pages/s per core, p99 ≤ 100 ms) or is limited to development and low-traffic routes.
- [ ] **W17-15** (proposed) Node sidecar for SSR: opt-in, off by default, for apps whose untranslatable components need more than the fallback's throughput.
  *Needs:* W17-07. *Done when:* decided after the tier 2 port.
- [ ] **W17-08** Port path for Vite SPAs: the client keeps building from its Vite config (common plugins supported), the server side ports with `nisse port`.
  *Needs:* W17-06, W14-04. *Done when:* a Vite + Express app ports to one binary.
- [ ] **W17-09** Port path for Next.js-style apps onto a Nisse framework: file-system routing (pages and app router), route handlers, middleware, data loading, server components, server actions, static assets.
  *Needs:* W17-03, W17-06. *Done when:* the tier 2 app's frontend runs on it; scope beyond that set after the tier 2 port.
- [ ] **W17-10** Port path for Remix/React Router loaders and actions.
  *Needs:* W17-09. *Done when:* a sample app ports.
- [ ] **W17-13** Client test strategy: client-side tests keep running on Node/Vitest during development; record the decision and its limits.
  *Needs:* W17-06. *Done when:* documented in the porting guide.
- [ ] **W17-11** (proposed) Nisse compiled to browser JavaScript for whole frontends, beyond shared modules.
  *Needs:* W17-04. *Done when:* decided after the tier 3 port.
