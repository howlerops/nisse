# Wave 16 · Frameworks

PRD phase 8. The backend frameworks and libraries complex apps are built on, as compat packages (W13-04) with their own test suites ported, as listed in the PRD's [Frameworks](../PRD.md#frameworks). Items are independent once their prerequisites land.

**Gate (with wave 18):** the tier 2 reference app's backend passes its tests.

## HTTP frameworks

- [ ] **W16-01** Express: routing, middleware chain, `req`/`res` API, error middleware, routers, static files, body parsers; on `node:http` (W12-09).
  *Needs:* W12-09, W13-04. *Done when:* Express's own test suite passes at ≥ 90%.
- [ ] **W16-02** Fastify (plugins, hooks, JSON schemas compiled to codecs), Koa (`ctx`, middleware), Hono.
  *Needs:* W16-01. *Done when:* each framework's own test suite passes at ≥ 90%.
- [ ] **W16-13** tRPC server: routers, procedures, middleware, adapters for the HTTP frameworks above.
  *Needs:* W16-01, W13-05 (`zod`). *Done when:* tRPC's server tests pass at ≥ 90%.

## NestJS

- [ ] **W16-03** NestJS: modules, controllers, providers, guards, pipes, interceptors, exception filters, configuration and the testing module; the dependency-injection container generated at compile time from decorator metadata (W10-19).
  *Needs:* W10-19, W16-01. *Done when:* Nest's sample apps and its core tests pass at ≥ 90%.
- [ ] **W16-04** `class-validator` and `class-transformer` on decorator metadata.
  *Needs:* W10-19. *Done when:* their test suites pass at ≥ 90%.

## Data

- [ ] **W16-05** Prisma: generate a Nisse client from `schema.prisma` on Go database drivers (Postgres, MySQL, SQLite), with query API parity for the operations reference apps use; migrations keep using the Prisma CLI at first.
  *Needs:* W13-04. *Done when:* the tier 1 app's data layer runs on it and Prisma's client query tests for those operations pass.
- [ ] **W16-06** Drizzle, Kysely, Knex (query builders) and TypeORM (decorator entities).
  *Needs:* W13-04, W10-19. *Done when:* each passes ≥ 90% of its ported tests for Postgres.

## Other services

- [ ] **W16-07** GraphQL: graphql-js-compatible schema and execution, Apollo Server and Nest GraphQL adapters.
  *Needs:* W16-01, W16-03. *Done when:* graphql-js execution tests pass at ≥ 90%.
- [ ] **W16-08** Realtime: `ws` (from W13-05) and socket.io server protocol compatibility.
  *Needs:* W12-09. *Done when:* the official socket.io client connects and passes socket.io's server tests at ≥ 90%.
- [ ] **W16-09** Jobs: BullMQ-compatible queues on Redis, `node-cron`.
  *Needs:* W13-05 (`ioredis`). *Done when:* a BullMQ producer on Node and a ported worker interoperate on the same Redis.
- [ ] **W16-10** Auth: Passport strategies (local, JWT, OAuth2), `express-session`, cookie handling.
  *Needs:* W16-01. *Done when:* Passport's strategy tests pass at ≥ 90%.
- [ ] **W16-11** Observability: the OpenTelemetry JS API on the Go OpenTelemetry SDK.
  *Needs:* W13-04. *Done when:* traces from a ported app reach an OTLP collector with correct spans.

## Test suites

- [ ] **W16-12** All framework test suites run in CI, with pass rates on the compatibility dashboard (W13-11).
  *Needs:* W13-10. *Done when:* in CI.
