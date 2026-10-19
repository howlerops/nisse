#!/usr/bin/env bash
# D-23 spike: React SSR in embedded JS engines vs Node vs Go-native.
# Usage: WORK=/some/scratch/dir RUNS=3 DUR=10s ./run.sh
# Results: $WORK/results/*.jsonl (one JSON line per run), summary on stdout.
# Overrides: CONFIGS="engine:bundle ..." (see below). Start from an empty $WORK/results to avoid mixing runs.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:?set WORK to a scratch directory}"
RUNS="${RUNS:-3}"
DUR="${DUR:-10s}"
DURSEC="${DUR%s}"

WORK="$WORK" "$HERE/build.sh" >/dev/null
(cd "$HERE/go" && CGO_ENABLED=0 go build -o "$WORK/bench" .)
mkdir -p "$WORK/results"
D="$WORK/dist"

# Each config is "engine:bundle". ssr-join.js is the one-line join patch (see js/build.mjs).
CONFIGS="${CONFIGS:-node:ssr.js node:ssr-join.js native:- goja:ssr.js goja:ssr-join.js modernc:ssr.js modernc:ssr-join.js qjs-wazero:ssr.js qjs-wazero:ssr-join.js}"
for run in $(seq 1 "$RUNS"); do
  echo "run $run start: $(cat /proc/loadavg)" >> "$WORK/results/load.txt"
  for c in $CONFIGS; do
    e="${c%%:*}"; b="${c#*:}"; [ "$b" = "-" ] && b=ssr.js
    tag="$e-${b%.js}"
    if [ "$e" = node ]; then
      node --expose-gc "$HERE/node/bench.mjs" "$D" "$DURSEC" "$D/$tag.html" "$b" >> "$WORK/results/$tag.jsonl"
    else
      "$WORK/bench" -engine "$e" -dist "$D" -bundle "$b" -dur "$DUR" -dump "$D/$tag.html" >> "$WORK/results/$tag.jsonl"
    fi
    if cmp -s "$D/$tag.html" "$D/node-ssr.html"; then same="identical"; else same="DIFFERENT"; fi
    echo "run $run $tag: HTML $same to node-ssr" | tee -a "$WORK/results/html-check.txt"
  done
done

# Median of each metric across runs.
node -e '
const fs = require("fs"), dir = process.argv[1];
const med = (a) => { a = a.slice().sort((x, y) => x - y); return a[Math.floor(a.length / 2)]; };
const keys = ["rendersPerSec", "p50ms", "p99ms", "startupMs", "memPerInstanceMB", "pool4RendersPerSec"];
for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".jsonl"))) {
  const rows = fs.readFileSync(dir + "/" + f, "utf8").trim().split("\n").map(JSON.parse);
  const out = { config: f.replace(".jsonl", ""), runs: rows.length, stream: rows[rows.length - 1].stream };
  for (const k of keys) { out[k] = +med(rows.map((r) => r[k])).toFixed(2); out[k + "Range"] = [Math.min(...rows.map((r) => r[k])), Math.max(...rows.map((r) => r[k]))].map((x) => +x.toFixed(2)); }
  console.log(JSON.stringify(out));
}' "$WORK/results"
