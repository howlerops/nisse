#!/usr/bin/env bash
# Bundle the benchmark app for every engine. Needs WORK (scratch dir for node_modules and dist).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:?set WORK to a scratch directory}"
mkdir -p "$WORK"
cd "$WORK"
[ -f package.json ] || npm init -y >/dev/null
[ -d node_modules/web-streams-polyfill ] || npm i --silent react@19.3.1 react-dom@19.3.1 esbuild@0.28.2 web-streams-polyfill@4.3.0
ln -sfn "$HERE/js" src
cp "$HERE/js/build.mjs" ./build.mjs && node build.mjs
