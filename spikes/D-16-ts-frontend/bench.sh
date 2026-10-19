#!/usr/bin/env bash
# Run every option RUNS times on each project, then compare outputs.
# Usage: WORK=... RUNS=5 ./bench.sh      (run ./setup.sh first with the same WORK)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:-${TMPDIR:-/tmp}/nisse-d16}"
RUNS="${RUNS:-5}"
out="$here/out"; mkdir -p "$out"; log="$out/bench.log"; : > "$log"
declare -A cfg=([rw]="$WORK/proj/rw/tsconfig.app.json" [im]="$WORK/proj/immich/server/tsconfig.json")
run() { # label outfile cmd...
  local label=$1 file=$2; shift 2
  "$here/bin/measure" "$label" "$@" 2>&1 | grep -E '^\{"option"|^MEASURE' | sed "s/^/$label\t/" >> "$log"
}
for p in rw im; do
  for i in $(seq "$RUNS"); do
    run "$p C-ts6"        "$out/$p-ts6.jsonl"      node --max-old-space-size=8192 "$here/dump-ts6.mjs" "${cfg[$p]}" "$out/$p-ts6.jsonl"
    run "$p A-ts7.0.2"    "$out/$p-ts7.jsonl"      env TS7_PKG=ts7 node "$here/dump-ts7.mjs" "${cfg[$p]}" "$out/$p-ts7.jsonl"
    run "$p A-ts7.1-dev"  "$out/$p-ts7next.jsonl"  env TS7_PKG=ts7next node "$here/dump-ts7.mjs" "${cfg[$p]}" "$out/$p-ts7next.jsonl"
    run "$p B-vendored-j1" "$out/$p-vend.jsonl"    "$here/bin/nissedump" -j 1 "${cfg[$p]}" "$out/$p-vend.jsonl"
    run "$p B-vendored-j4" "$out/$p-vend-j4.jsonl" "$here/bin/nissedump" -j 4 "${cfg[$p]}" "$out/$p-vend-j4.jsonl"
  done
  for pair in "ts6 ts7next" "ts6 vend" "ts7 ts7next" "ts7next vend" "vend vend-j4"; do
    set -- $pair
    python3 "$here/compare.py" "$out/$p-$1.jsonl" "$out/$p-$2.jsonl" --examples 8 > "$out/cmp-$p-$1-$2.json"
  done
done
python3 "$here/summarize.py" "$out" | tee "$out/summary.md"
