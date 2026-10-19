#!/usr/bin/env bash
# D-23 spike: React SSR in embedded JS engines vs Node vs Go-native.
# Usage: WORK=/some/scratch/dir RUNS=3 DUR=10s ./run.sh
# Results: $WORK/results/*.jsonl (one JSON line per run), summary on stdout.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:?set WORK to a scratch directory}"
RUNS="${RUNS:-3}"
DUR="${DUR:-10s}"
ENGINES="${ENGINES:-native goja modernc qjs-wazero}"
DURSEC="${DUR%s}"

WORK="$WORK" "$HERE/build.sh" >/dev/null
(cd "$HERE/go" && CGO_ENABLED=0 go build -o "$WORK/bench" .)
mkdir -p "$WORK/results"
D="$WORK/dist"

for run in $(seq 1 "$RUNS"); do
  node --expose-gc "$HERE/node/bench.mjs" "$D" "$DURSEC" "$D/node.html" >> "$WORK/results/node.jsonl"
  for e in $ENGINES; do
    "$WORK/bench" -engine "$e" -dist "$D" -dur "$DUR" -dump "$D/$e.html" >> "$WORK/results/$e.jsonl"
    if cmp -s "$D/$e.html" "$D/node.html"; then echo "run $run $e: HTML identical to Node"; else echo "run $run $e: HTML DIFFERS from Node"; fi
  done
done

# Median of each metric across runs.
node -e '
const fs = require("fs"), dir = process.argv[1];
const med = (a) => { a = a.slice().sort((x, y) => x - y); return a[Math.floor(a.length / 2)]; };
const keys = ["rendersPerSec", "p50ms", "p99ms", "startupMs", "memPerInstanceMB", "pool4RendersPerSec"];
for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".jsonl"))) {
  const rows = fs.readFileSync(dir + "/" + f, "utf8").trim().split("\n").map(JSON.parse);
  const out = { engine: rows[0].engine, runs: rows.length, stream: rows[rows.length - 1].stream };
  for (const k of keys) { out[k] = +med(rows.map((r) => r[k])).toFixed(2); out[k + "Range"] = [Math.min(...rows.map((r) => r[k])), Math.max(...rows.map((r) => r[k]))].map((x) => +x.toFixed(2)); }
  console.log(JSON.stringify(out));
}' "$WORK/results"
