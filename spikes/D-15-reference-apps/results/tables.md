
### Tier 1

| Repo | Pinned SHA | Last commit | License | TS+TSX LOC | Files (ts/tsx) | Direct deps (rt+dev) | Transitive | Runtime native deps | Decorators | Stack | Tests (unit / e2e cases) | Services |
| --- | --- | --- | --- | ---: | --- | --- | ---: | --- | ---: | --- | --- | --- |
| gothinkster/node-express-realworld-example-app | `30b68e1e8814` | 2024-01-04 | MIT | 2,095 | 37/0 | 11+28 | 733 | @prisma/engines, prisma | 0 | express, @prisma/client (Prisma ^4.16.1) | 27 / 1 (jest) | prisma:postgresql |
| lujakob/nestjs-realworld-example-app | `c1c2cc4e448b` | 2021-01-18 | ISC | 967 | 35/0 | 22+10 | 664 | argon2 | 123 | @nestjs/core, typeorm | 1 / 0 (jest, supertest) | — |
| mutoe/nestjs-realworld-example-app | `0c30a1828af5` | 2023-04-29 | MIT | 718 | 26/0 | 19+20 | 951 | — | 39 | @nestjs/core, typeorm | 20 / 7 (jest, supertest, newman) | postgres |

### Tier 2

| Repo | Pinned SHA | Last commit | License | TS+TSX LOC | Files (ts/tsx) | Direct deps (rt+dev) | Transitive | Runtime native deps | Decorators | Stack | Tests (unit / e2e cases) | Services |
| --- | --- | --- | --- | ---: | --- | --- | ---: | --- | ---: | --- | --- | --- |
| umami-software/umami | `ec0ff50388c2` | 2026-09-16 | MIT | 120,906 | 826/536 | 60+31 | 1,162 | @napi-rs/lzma, @prisma/engines, prisma, sharp | 0 | next, react, @prisma/client (Prisma ^7.10.0) | 1,134 / 38 (vitest, @playwright/test) | clickhouse, postgres, prisma:postgresql |
| lukevella/rallly | `04f7ce8372ec` | 2026-10-19 | AGPL-3.0 | 133,385 | 568/597 | 122+40 | 2,484 | @napi-rs/lzma, @prisma/engines, prisma, sharp | 0 | hono, next, @trpc/server, react, @prisma/client (Prisma ^7.8.0) | 1,167 / 0 (vitest, @playwright/test) | postgres, prisma:postgresql, redis |
| documenso/documenso | `359177ee9054` | 2026-10-19 | AGPL-3.0 + enterprise dir | 248,798 | 1518/708 | 170+79 | 1,974 | @datadog/pprof, @documenso/skia-canvas, @napi-rs/canvas, @node-rs/argon2, @node-rs/bcrypt, @prisma/engines, aws-crt, msgpackr-extract, prisma, sharp | 1 | hono, next, @trpc/server, react, react-router, vite, @prisma/client, kysely (Prisma ^6.19.0) | 256 / 1,255 (vitest, @playwright/test) | postgres, prisma:postgresql, redis |
| dubinc/dub | `dff810b69dad` | 2026-10-18 | AGPL-3.0 | 522,890 | 2581/2082 | 185+45 | 2,819 | @prisma/engines, prisma, sharp, sqlite3 | 1 | next, react, vite, @prisma/client (Prisma 6.19.1) | 1,228 / 441 (vitest, @playwright/test) | mailhog, mysql, prisma:mysql, redis |

### Tier 3

| Repo | Pinned SHA | Last commit | License | TS+TSX LOC | Files (ts/tsx) | Direct deps (rt+dev) | Transitive | Runtime native deps | Decorators | Stack | Tests (unit / e2e cases) | Services |
| --- | --- | --- | --- | ---: | --- | --- | ---: | --- | ---: | --- | --- | --- |
| immich-app/immich | `fff2bd73f9b3` | 2026-10-19 | AGPL-3.0 | 150,464 | 1078/11 | 137+104 | 2,698 | bcrypt, canvas, cpu-features, exiftool-vendored, msgpackr-extract, sharp, ssh2 | 2,789 | express, @nestjs/core, react, svelte, @sveltejs/kit, vite, kysely | 3,022 / 482 (vitest, @playwright/test, supertest) | postgres, valkey |
| outline/outline | `2f68f32b40c4` | 2026-10-19 | BUSL-1.1 | 318,068 | 1527/745 | 228+76 | 1,959 | @datadog/pprof, @napi-rs/nice, aws-crt, msgpackr-extract, unix-dgram | 1,211 | koa, react, vite, sequelize | 4,822 / 0 (vitest) | postgres, redis |
| twentyhq/twenty | `091659c20fdc` | 2026-10-19 | AGPL-3.0 | 2,267,291 | 27490/5688 | 312+199 | 4,952 | @napi-rs/lzma, @napi-rs/nice, @sentry/profiling-node, bcrypt, canvas, cpu-features, msgpackr-extract, sharp, ssh2 | 17,656 | express, @nestjs/core, next, graphql, react, vite, typeorm | 26,889 / 28 (jest, vitest, @playwright/test, supertest) | clickhouse, postgres, redis |
| hoppscotch/hoppscotch | `63273f8e599e` | 2026-09-30 | MIT | 210,655 | 1089/0 | 178+115 | 2,704 | @napi-rs/canvas, @prisma/engines, argon2, bcrypt, isolated-vm, prisma | 1,962 | express, @nestjs/core, graphql, @apollo/server, vue, svelte, vite, @prisma/client (Prisma 7.9.1) | 3,695 / 78 (jest, vitest, mocha, supertest) | postgres, prisma:postgresql |
| medusajs/medusa | `61c19c3099d5` | 2026-10-19 | MIT | 981,390 | 6891/3274 | 257+136 | 2,303 | @napi-rs/lzma, msgpackr-extract | 1,731 | express, next, graphql, react, vite, @mikro-orm/core | 8,539 / 0 (jest, vitest, supertest) | postgres, redis |
| novuhq/novu | `b30b8ce1406f` | 2026-10-19 | MIT + enterprise dir | 988,424 | 7543/1746 | 438+229 | 5,491 | @datadog/pprof, @napi-rs/nice, @sentry/profiling-node, bcrypt, msgpackr-extract, sharp | 15,360 | express, @nestjs/core, hono, next, react, @sveltejs/kit, solid-js, vite, mongoose | 7,096 / 2,189 (jest, vitest, mocha, @playwright/test, supertest) | clickhouse, localstack, mongo, redis |
