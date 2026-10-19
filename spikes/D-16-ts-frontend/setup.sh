#!/usr/bin/env bash
# Fetch everything the D-16 spike needs into $WORK (default: a scratch dir), then build.
# Nothing large lands in this directory except node_modules/ and bin/ (both git-ignored).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:-${TMPDIR:-/tmp}/nisse-d16}"
mkdir -p "$WORK/proj"
export GOTOOLCHAIN=auto

# Test projects, pinned.
if [ ! -d "$WORK/proj/rw" ]; then
  git clone -q https://github.com/gothinkster/node-express-realworld-example-app "$WORK/proj/rw"
  git -C "$WORK/proj/rw" checkout -q 30b68e1
  (cd "$WORK/proj/rw" && npm ci --ignore-scripts --no-audit --no-fund && npx prisma generate)
fi
if [ ! -d "$WORK/proj/immich" ]; then
  git clone -q --filter=blob:none --sparse https://github.com/immich-app/immich "$WORK/proj/immich"
  git -C "$WORK/proj/immich" checkout -q fff2bd7
  git -C "$WORK/proj/immich" sparse-checkout set server packages/plugin-sdk
  (cd "$WORK/proj/immich" && pnpm install --filter 'immich...' --ignore-scripts)
fi
# TypeScript's Go compiler (option B) at the revision the spike used, plus typescript-go
# at 2026-04-17 for the churn check.
if [ ! -d "$WORK/ts" ]; then
  git clone -q --filter=blob:none --no-checkout https://github.com/microsoft/TypeScript "$WORK/ts"
  git -C "$WORK/ts" sparse-checkout set --no-cone '/tsc/*' '!/tsc/testdata/'
  git -C "$WORK/ts" checkout -q d7d39da5c4
fi
if [ ! -d "$WORK/old-apr17" ]; then
  git clone -q --filter=blob:none --no-checkout https://github.com/microsoft/typescript-go "$WORK/tsgo"
  mkdir -p "$WORK/old-apr17"
  git -C "$WORK/tsgo" archive 1de8d6823 internal go.mod LICENSE NOTICE.txt | tar -x -C "$WORK/old-apr17"
fi

# Options A and C: TypeScript 6.0.3 (classic JS API), 7.0.2 and a 7.1 nightly (native API).
(cd "$here" && npm install --no-audit --no-fund)
# Option B: vendor and build.
"$here/vendored/vendor.sh" "$WORK/ts"
(cd "$here/vendored" && go build -o "$here/bin/nissedump" ./cmd/nissedump)
(cd "$here/measure" && go build -o "$here/bin/measure" .)
echo "setup done: WORK=$WORK"
