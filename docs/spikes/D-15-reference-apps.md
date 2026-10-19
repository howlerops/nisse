# D-15 spike: reference apps

Oct 19, 2026 · decides [D-15](../ledger/decisions.md) and [W18-01](../ledger/wave-18-reference-ports.md)

## Recommendation

| Tier | App | Pinned commit | Why |
| --- | --- | --- | --- |
| 1 | [gothinkster/node-express-realworld-example-app](https://github.com/gothinkster/node-express-realworld-example-app) (Express + Prisma) | `30b68e1e881462b2f4164ea09ab4c4f5699c7b0b` | MIT; 2.1k lines; no decorators; Postgres only; the official RealWorld backend. **Runs here:** 309 of 311 API assertions pass against the matching RealWorld spec (`a0767b86710599a793fe185e56d4c5756060573d`) |
| 1b (optional) | [mutoe/nestjs-realworld-example-app](https://github.com/mutoe/nestjs-realworld-example-app) (NestJS + TypeORM) | `0c30a1828af52cf6528d0b3e9ddf86fc459723f6` | MIT; 718 lines, 39 decorators. A cheap early check of decorator and NestJS work (W10-19, W16-03) before tier 2 |
| 2 | [umami-software/umami](https://github.com/umami-software/umami) (Next.js + Prisma 7) | `ec0ff50388c264ed8ce46f00967e92f7e71476ae` | MIT; the smallest dependency tree of the tier 2 candidates (1,162 packages); Prisma 7 (no Rust query engine); no decorators; Postgres with ClickHouse optional; 1,134 unit and 38 Playwright cases, plus an API test runner |
| 3 | [immich-app/immich](https://github.com/immich-app/immich) server and web | `fff2bd73f9b38a5a5b510ea69a77a470dd1acf56` | AGPL-3.0 (OSI); 103k-line NestJS server using BullMQ, socket.io, Kysely, Postgres, Valkey and OpenTelemetry, which covers most of wave 16; 3,022 unit and 482 end-to-end cases; very active |
| 3 fallback | [hoppscotch/hoppscotch](https://github.com/hoppscotch/hoppscotch) backend and web | `63273f8e599e73f0fb38c0739314e8d46b6bc83d` | MIT; smaller NestJS backend (39.5k lines) on Apollo GraphQL, Prisma 7 and Passport. Use it if Immich's media dependencies block the port |

**PRD change needed:** Outline, a PRD tier 3 candidate, is **disqualified**. Its license is Business Source License 1.1, with a use restriction ("Document Service") until 2030-10-12. That is not OSI-approved, so a public port would be restricted.

**React SSR coverage moves to tier 2.** Immich's web app is a SvelteKit SPA built with `adapter-static`, so tier 3 exercises client bundling and serving (W17-06) but not React SSR. Umami (Next.js App Router, `src/app` is 61k lines) carries the React SSR and Next.js port path (W17-07, W17-09). No tier 3 candidate was React-based, OSI-licensed and of manageable size at once (see Twenty and Outline below).

## Method

1. **Shortlist.** GitHub search isn't reachable from this environment, so candidates came from known projects and were confirmed with `git ls-remote`. Three NestJS RealWorld repos named in planning (`ottonova/…`, `dhedegaard/…`, `Abdulrahman-Tayara/…`) did not exist and were dropped.
2. **Clone** each candidate shallowly at its default-branch HEAD on Oct 19, 2026 (`clone.sh`).
3. **Measure** each checkout statically (`measure.py`). Nothing is installed:
   - license file text, OSI status and enterprise directories
   - non-blank TS/TSX lines, excluding `.d.ts`, `node_modules`, `dist` and `generated`
   - direct dependencies across every workspace `package.json`, and unique `name@version` entries in the lockfile
   - runtime native-addon dependencies: a known-native list matched against the lockfile, with platform-binary packages folded into their base name and build-only tools (esbuild, swc) left out
   - imports of Node built-in modules
   - decorator lines in TS files
   - framework, ORM and test-framework dependencies
   - `it`/`test` call counts in test files
   - services in compose files, CI workflows and Prisma datasources
   - Prisma major version (7 and later run without the Rust query engine)
4. **Split monorepos** into server and web parts (`subtree_loc.py`).
5. **Run the tier 1 candidate** on Node 22 against a local Postgres 16 (`run-tier1.sh`).

Raw results are in `spikes/D-15-reference-apps/results/scorecard.json`; the tables below come from `table.py`.

## Scorecard

Tests are `unit / e2e` case counts from static counting, so treat them as approximate. "Runtime native deps" lists packages whose native code runs in production.

### Tier 1

| Repo | Last commit | License | TS lines | Deps (direct rt+dev / transitive) | Runtime native deps | Decorators | Stack | Tests | Services |
| --- | --- | --- | ---: | --- | --- | ---: | --- | --- | --- |
| gothinkster/node-express-realworld-example-app | 2024-01-04 | MIT | 2,095 | 11+28 / 733 | Prisma 4 query engine | 0 | Express, Prisma 4 | 27 / 1 (Jest) | Postgres |
| lujakob/nestjs-realworld-example-app | 2021-01-18 | ISC | 967 | 22+10 / 664 | argon2 | 123 | NestJS, TypeORM | 1 / 0 (Jest) | not declared |
| mutoe/nestjs-realworld-example-app | 2023-04-29 | MIT | 718 | 19+20 / 951 | — | 39 | NestJS, TypeORM | 20 / 7 (Jest, newman) | Postgres |

### Tier 2

| Repo | Last commit | License | TS+TSX lines | Deps (direct / transitive) | Runtime native deps | Decorators | Stack | Tests | Services |
| --- | --- | --- | ---: | --- | --- | ---: | --- | --- | --- |
| umami-software/umami | 2026-09-16 | MIT | 120,906 | 60+31 / 1,162 | sharp (Next image optimisation), @napi-rs/lzma | 0 | Next.js, React, Prisma 7 | 1,134 / 38 (Vitest, Playwright) | Postgres; ClickHouse optional |
| lukevella/rallly | 2026-10-19 | AGPL-3.0 | 133,385 | 122+40 / 2,484 | sharp, @napi-rs/lzma | 0 | Next.js, tRPC, Hono, Prisma 7 | 1,167 / 0 | Postgres, Redis |
| documenso/documenso | 2026-10-19 | AGPL-3.0 + enterprise dir | 248,798 | 170+79 / 1,974 | canvas ×2, argon2, bcrypt, pprof, aws-crt, sharp, Prisma 6 engine | 1 | Next.js, React Router, tRPC, Hono, Prisma 6, Kysely | 256 / 1,255 | Postgres, Redis |
| dubinc/dub | 2026-10-18 | AGPL-3.0 | 522,890 | 185+45 / 2,819 | sharp, sqlite3, Prisma 6 engine | 1 | Next.js, Prisma 6 | 1,228 / 441 | MySQL, Redis |

### Tier 3

| Repo | Last commit | License | TS+TSX lines (server · web) | Deps (direct / transitive) | Runtime native deps | Decorators | Stack | Tests | Services |
| --- | --- | --- | --- | --- | --- | ---: | --- | --- | --- |
| immich-app/immich | 2026-10-19 | AGPL-3.0 | 150,464 (103k · 59k incl. 37k Svelte) | 137+104 / 2,698 | sharp, bcrypt, exiftool-vendored, canvas; ffmpeg via `child_process` | 2,789 | NestJS, BullMQ, socket.io, Kysely; SvelteKit SPA | 3,022 / 482 | Postgres, Valkey |
| outline/outline | 2026-10-19 | **BUSL-1.1** | 318,068 (144k · 104k) | 228+76 / 1,959 | pprof, aws-crt, unix-dgram | 1,211 | Koa, React, Sequelize | 4,822 / 0 | Postgres, Redis |
| twentyhq/twenty | 2026-10-19 | AGPL-3.0 | 2,267,291 (1,021k · 663k) | 312+199 / 4,952 | sharp, bcrypt, canvas, ssh2, profiling | 17,656 | NestJS, GraphQL, TypeORM, React | 26,889 / 28 | Postgres, Redis, ClickHouse |
| hoppscotch/hoppscotch | 2026-09-30 | MIT | 210,655 (39.5k · 175k incl. 72k Vue) | 178+115 / 2,704 | argon2, bcrypt; isolated-vm and canvas only outside the backend | 1,962 | NestJS, Apollo GraphQL, Passport, Prisma 7; Vue | 3,695 / 78 | Postgres |
| medusajs/medusa | 2026-10-19 | MIT | 981,390 (framework: core 283k, modules 161k · admin 143k) | 257+136 / 2,303 | — | 1,731 | Express, MikroORM, React admin | 8,539 / 0 | Postgres, Redis |
| novuhq/novu | 2026-10-19 | MIT + enterprise dir | 988,424 | 438+229 / 5,491 | sharp, bcrypt, pprof, profiling | 15,360 | NestJS, Next.js, Mongoose | 7,096 / 2,189 | MongoDB, Redis, ClickHouse |

Node built-in imports for each candidate are in `scorecard.json`. For the picks:

- **Tier 1:** none.
- **Umami:** `path`, `fs`, `crypto`, `url`, `child_process` and `zlib` most often.
- **Immich:** `fs`, `path`, `stream`, `crypto`, `child_process` and `os`, plus `worker_threads`.

## Tier 1 run on Node (what actually happened)

| Step | Result |
| --- | --- |
| `npm ci` | 734 packages in 8 s; the Prisma 4 Linux engine downloaded without trouble |
| `jest` without a database | 7 of 27 fail: Prisma client init errors on missing `DATABASE_URL`, even though the tests mock Prisma |
| `jest` with Postgres 16 and migrations applied | 22 pass, 1 todo, **4 fail** in `auth.service.test.ts`, all rejecting with `[Error: [object Object]]`. This reproduces with a real database, so it looks like an upstream test bug at this commit. Not investigated further |
| Current RealWorld Hurl suite (spec HEAD `ebbcdeb8`, 154 requests, Hurl 8.0.0) | All 13 files fail. The causes are spec drift: since Feb 2026 the spec expects `image: null`, 409 for duplicates, a new error shape and no `body` in feeds, and the app predates all of that. Hurl 7.0.0 can't parse the suite (`isList`) |
| RealWorld Postman collection from the app's era (spec `a0767b86`, 2024-01-04, newman 6) | **309 / 311 assertions pass** across 32 requests. The 2 failures assume a fixed tag order (`dragons`, `training`) |

The tier 1 port therefore uses the Jan 2024 Postman collection as its API test oracle, not the current Hurl suite. Running the same collection against the Node original and the port also gives the replay baseline (W15-07). The 4 failing unit tests are documented as pre-existing and excluded from the "original tests pass" gate.

## Risks and findings for the ledger

- **Schedule mismatch.** Tier 1 gates Phase 7, but Express (W16-01) and the Prisma client generator (W16-05) sit in wave 16, Phase 8. A minimal Express subset and a Prisma client covering the tier 1 schema need to land in Phase 7, or tier 1 needs those items moved earlier.
- **Immich media pipeline.** `sharp` (libvips) does thumbnails and conversions inside `server/src/repositories/media.repository.ts`; `exiftool-vendored` (Perl) and `ffmpeg`/`ffprobe` run as external processes. A Go-native `sharp` compat package without cgo may not cover HEIC/RAW. The original Docker image also ships ffmpeg and exiftool, so "one binary" for Immich has to mean "one binary plus the same external tools". The ML service is Python and stays out of scope. This is why Hoppscotch is the fallback.
- **Spec drift.** RealWorld moved to a v2 spec in Feb 2026, and most TS implementations predate it. Pin both the app and the spec commit.
- **Umami** also supports ClickHouse. The port should target Postgres only at first.
- **Not verified:**
  - Umami's, Immich's and Hoppscotch's test suites were not run here.
  - Test case counts are static estimates.
  - Native-dependency detection uses a known-package list, not an install; packages outside that list would be missed.
  - Prisma 7's Rust-free default comes from Prisma's documentation; here it was only seen as `engineType = "client"` in one schema.
- **License obligations.** AGPL picks (Immich) are fine for a public port, but the ported code must stay AGPL. MIT picks have no constraint.

## How to reproduce

```bash
cd spikes/D-15-reference-apps
./clone.sh /tmp/d15                          # shallow clones of every candidate
python3 -I measure.py /tmp/d15 > results/scorecard.json
python3 -I table.py results/scorecard.json   # markdown tables
python3 -I subtree_loc.py /tmp/d15/immich-app_immich server web

# Tier 1 run (needs Postgres 16 on :5432, user postgres/postgres, db conduit)
service postgresql start
su postgres -c "psql -c \"ALTER USER postgres PASSWORD 'postgres';\" -c 'CREATE DATABASE conduit;'"
./run-tier1.sh /tmp/d15-run
```

Clones made by `clone.sh` today pick up newer commits. To reproduce these numbers exactly, check out the pinned SHAs listed in `results/scorecard.json`.
