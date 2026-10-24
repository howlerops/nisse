#!/usr/bin/env bash
# Run the tier 1 candidate (gothinkster Express + Prisma) on Node against a
# local Postgres, then run its unit tests and the RealWorld API tests.
#
# Usage: run-tier1.sh <work-dir>
# Needs: git, node 22, npm, a running Postgres on localhost:5432 with user
# postgres/postgres and a database named conduit (see the D-15 report).
set -euo pipefail

work=${1:?work dir}
mkdir -p "$work" && cd "$work"
app_sha=30b68e1e881462b2f4164ea09ab4c4f5699c7b0b
spec_sha=a0767b86710599a793fe185e56d4c5756060573d   # spec as of 2024-01-04, matching the app

[ -d app ] || git clone -q https://github.com/gothinkster/node-express-realworld-example-app app
git -C app checkout -q "$app_sha"
[ -d spec ] || git clone -q https://github.com/gothinkster/realworld spec
git -C spec checkout -q "$spec_sha"

export DATABASE_URL=postgresql://postgres:postgres@localhost:5432/conduit
export JWT_SECRET=spike-secret PORT=8000

cd app
npm ci --no-audit --no-fund
npx prisma generate
npx prisma migrate deploy
npx jest || echo "unit tests: failures above (4 auth tests fail upstream at this commit)"

npx ts-node --transpile-only -P tsconfig.app.json src/main.ts > ../server.log 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
for _ in $(seq 1 30); do curl -sf localhost:8000/api/tags > /dev/null && break; sleep 1; done

cd ../spec/api
u=u$(date +%s)
npx -y newman@6 run Conduit.postman_collection.json --delay-request 20 \
  --global-var APIURL=http://localhost:8000/api --global-var "USERNAME=$u" \
  --global-var "EMAIL=$u@mail.com" --global-var PASSWORD=password
