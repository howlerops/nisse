#!/usr/bin/env bash
# Bundle the benchmark app for every engine. Needs WORK (scratch dir with node_modules).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:?set WORK to a scratch directory}"
mkdir -p "$WORK"
cd "$WORK"
[ -f package.json ] || npm init -y >/dev/null
[ -d node_modules/react-dom ] || npm i --silent react@19.3.1 react-dom@19.3.1 esbuild@0.28.2 web-streams-polyfill@4.3.0
ln -sfn "$HERE/js" src
mkdir -p dist
ESB="./node_modules/.bin/esbuild --bundle --preserve-symlinks --format=iife --minify --define:process.env.NODE_ENV=\"production\""
$ESB --platform=browser src/entry.jsx --outfile=dist/entry.js --log-level=warning
$ESB --platform=browser src/entry-stream.jsx --outfile=dist/entry-stream.js --log-level=warning
cat src/prelude.js dist/entry.js > dist/ssr.js
cat src/prelude.js dist/entry-stream.js > dist/ssr-stream.js
ls -l dist/ssr.js dist/ssr-stream.js
