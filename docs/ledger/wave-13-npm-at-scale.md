# Wave 13 · npm at scale

PRD phase 6. The four npm tracks from the PRD's [npm at scale](../PRD.md#npm-at-scale): Go-native compat packages, ports from TypeScript source, the embedded engine ([wave 9](wave-9-npm.md)), and flagged blockers.

**Gate:** every dependency of the tier 1 reference app has a working track; the top-500 dashboard is live (its ≥ 80% target applies at the end of Phase 9).

## Resolution and tracks

- [ ] **W13-01** Dependency triage: for a TypeScript project, list every dependency (including transitive ones) with its proposed track and blockers. This is the first step of every port.
  *Needs:* W13-02. *Done when:* runs on all three reference app candidates and the reports are committed.
- [ ] **W13-02** `package.json` and `node_modules` resolution: `exports`/`imports` maps, conditions, ESM/CJS interop semantics, workspaces.
  *Needs:* W10-24. *Done when:* resolution matches Node on its resolution test fixtures.
- [ ] **W13-03** Track selection and a lock file recording the chosen track per package and version (D-24).
  *Needs:* W13-01. *Done when:* two builds from the same lock file choose the same tracks.
- [ ] **W13-13** Flagged track: a diagnostic per unsupported package with the reason and a suggested alternative.
  *Needs:* W13-03. *Done when:* tests for native addons and for packages needing unsupported Node APIs.

## Go-native compat packages

- [ ] **W13-04** Compat package framework: how a Go-native package declares the npm package and version range it replaces, its API (`.d.nis`), and its ported tests.
  *Needs:* W7-01, W11-12. *Done when:* documented, with one sample package built on it.
- [ ] **W13-05** Compat packages, first batch (tier 1 needs and the most common backend packages): `pg`, `ioredis`, `jsonwebtoken`, `bcrypt`/`bcryptjs`, `uuid`, `dotenv`, `zod`, `pino`, `winston`, `axios`, `node-fetch`, `lodash`, `date-fns`, `dayjs`, `cors`, `helmet`, `cookie-parser`, `multer`, `nodemailer`, `ws`.
  *Needs:* W13-04. *Done when:* each passes ≥ 90% of tests ported from the original package.
- [ ] **W13-06** Compat packages, second batch: the list comes from the tier 2 and tier 3 triage reports.
  *Needs:* W13-01, W13-05. *Done when:* each passes ≥ 90% of its ported tests.
- [ ] **W13-14** Media for tier 3: a `sharp` compat package on a cgo-free Go image pipeline, with the HEIC/RAW gap measured; `exiftool` and `ffmpeg` stay external programs.
  *Needs:* W13-04. *Done when:* Immich's thumbnail tests pass, or the gap is recorded and the Hoppscotch fallback is triggered.
- [ ] **W13-10** Fidelity tests: tooling to port a package's own test suite (Jest, Mocha, Vitest) against its compat package.
  *Needs:* W15-01. *Done when:* used by every package in W13-05.

## Other tracks

- [ ] **W13-07** Port-from-source track: dependencies that ship TypeScript source go through `nisse port` and build as modules of the app.
  *Needs:* W14-04. *Done when:* three real TypeScript-source packages build this way and pass their tests.
- [ ] **W13-08** Engine track at scale: bind the wave 12 Go implementations of the Node APIs into the embedded engine, replacing the minimal wave 9 shim.
  *Needs:* W9-06, wave 12. *Done when:* the 20-package set from W9-09 grows to 100 pure-JS packages passing.
- [ ] **W13-09** Engine boundary performance: batched calls, zero-copy buffers, profiling hooks.
  *Needs:* W13-08. *Done when:* call overhead and throughput recorded and improved against the W9-02 baseline.

## Coverage and supply chain

- [ ] **W13-11** Compatibility dashboard: the top 500 npm packages by downloads, each with its track and status, updated in CI.
  *Needs:* W13-03. *Done when:* published with each release.
- [ ] **W13-12** Supply chain: lock-file integrity hashes, license report for every bundled or ported package.
  *Needs:* W13-03. *Done when:* `nisse build` fails on a hash mismatch, and the license report is generated.
