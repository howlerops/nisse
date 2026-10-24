#!/usr/bin/env bash
# Multicore scaling check: total renders/sec with 1, 2 and 4 instances on as
# many goroutines, small page. Run after run.sh (needs $WORK/bench and dist).
# Usage: WORK=/some/scratch/dir ROUNDS=2 DUR=8s ./scale.sh
set -euo pipefail
WORK="${WORK:?set WORK to a scratch directory}"
ROUNDS="${ROUNDS:-2}"
DUR="${DUR:-8s}"
for r in $(seq 1 "$ROUNDS"); do
  echo "round $r load: $(cat /proc/loadavg)" >&2
  for c in native:ssr.js goja:ssr-join.js modernc:ssr.js qjs-wazero:ssr-join.js; do
    for n in 1 2 4; do
      "$WORK/bench" -engine "${c%%:*}" -bundle "${c#*:}" -dist "$WORK/dist" -rows 100 -dur "$DUR" -pool-only "$n"
    done
  done
done
