#!/usr/bin/env bash
# D-25 regex spike: reproduce every number in docs/spikes/D-25-regex.md.
# usage: spikes/D-25-regex/run.sh [workdir]   (needs Node 22, Go, network for npm/test262/modules)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
W=${1:-${D25_WORK:-$HERE/out}}
mkdir -p "$W/out"
cd "$HERE"
ENGINES="goja quickjs qjswasm regexp2v1 regexp2v2 libregexp libregexpopt re2fast hybrid hybridlre"

step() { echo "== $*" >&2; }

if [ ! -d "$W/pkgs/node_modules" ]; then
  step "install npm corpus packages"
  # Top-level versions are pinned from corpus-packages.json; transitive dependencies can still drift.
  PKGS=$(node -e 'const p=require(process.argv[1]);console.log(Object.entries(p).map(([k,v])=>k+"@"+v).join(" "))' "$HERE/corpus-packages.json")
  mkdir -p "$W/pkgs" && (cd "$W/pkgs" && npm init -y >/dev/null && npm i --silent $PKGS)
fi
if [ ! -d "$W/test262" ]; then
  step "clone test262 (RegExp-related directories only)"
  git clone -q --depth 1 --filter=blob:none --sparse https://github.com/tc39/test262.git "$W/test262"
  (cd "$W/test262" && git sparse-checkout set harness test/built-ins/RegExp test/built-ins/String/prototype/match \
    test/built-ins/String/prototype/matchAll test/built-ins/String/prototype/replace test/built-ins/String/prototype/replaceAll \
    test/built-ins/String/prototype/split test/built-ins/String/prototype/search test/language/literals/regexp)
fi

step "build harness"
(cd go && go build -o ../bin/d25 .)

step "extract corpus and generate cases"
node extract.mjs "$W/pkgs/node_modules" "$W/corpus-all.json"
node gen.mjs "$W/corpus-all.json" "$W/pkgs/node_modules" "$W/out/cases.json" 4000

step "Node ground truth"
node truth.mjs "$W/out/cases.json" "$W/out/truth-node.json"

step "differential runs"
# goja panics (a Go panic, not a JS error) on some patterns; the driver restarts it and records the case as a crash.
for e in $ENGINES; do ./bin/d25 drive -engine $e -cases "$W/out/cases.json" -harness harness.js -out "$W/out/res-$e.json" 2>&1 | grep -E '^[a-z0-9]+: [0-9]+ cases' >&2; done
./bin/d25 diff -cases "$W/out/cases.json" -truth "$W/out/truth-node.json" -out "$W/out/diff.md" \
  $(for e in $ENGINES; do echo "$e=$W/out/res-$e.json"; done) > /dev/null

step "RE2 fast-path coverage"
./bin/d25 re2cov -cases "$W/out/cases.json" -out "$W/out/re2cov.md"

step "test262"
node t262-node.mjs "$W/test262" "$W/out/t262-node.json"
for e in goja quickjs qjswasm; do ./bin/d25 t262 -engine $e -text "$W/test262" -out "$W/out/t262-$e.json"; done
./bin/d25 t262report -out "$W/out/t262.md" node="$W/out/t262-node.json" goja="$W/out/t262-goja.json" quickjs="$W/out/t262-quickjs.json" qjswasm="$W/out/t262-qjswasm.json"

step "benchmarks"
node bench-prep.mjs "$W/out/cases.json" "$W/out"
node bench-node.mjs "$W/out" 5
for e in re2fast hybridlre libregexpopt libregexp hybrid regexp2v2 regexp2v1 quickjs qjswasm goja; do ./bin/d25 bench -engine $e -cases "$W/out/bench-patterns.json" -harness harness.js -text "$W/out/bench-text.txt" -runs 5 -out "$W/out/bench-$e.json"; done
node bench-report.mjs "$W/out" > "$W/out/bench.md"

step "catastrophic backtracking"
node redos-node.mjs > "$W/out/redos.tsv"
for e in goja quickjs qjswasm regexp2v2 libregexp libregexpopt hybridlre; do ./bin/d25 redos -engine $e -harness harness.js >> "$W/out/redos.tsv"; done



step "binary size (stripped linux/amd64 hello-world per engine)"
(cd go && for d in base re2 regexp2 lre; do go build -ldflags="-s -w" -o "$W/out/size-$d" ./sizes/$d; done)
ls -l "$W/out"/size-* | awk '{print $5, $NF}' > "$W/out/sizes.txt"
step "done: results in $W/out (diff.md, re2cov.md, t262.md, bench.md, redos.tsv, sizes.txt)"
