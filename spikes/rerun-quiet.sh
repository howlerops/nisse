#!/usr/bin/env bash
# Sequential re-run of the D-16, D-23 and D-25 benchmarks on a quiet machine.
set -u
S=${S:?set S to the scratch directory holding d16, d23 and d25}; R=${R:-$S/rerun}; mkdir -p "$R"
log(){ echo "$(date +%T) $* | load $(cut -d' ' -f1-3 /proc/loadavg)" | tee -a $R/progress.txt; }

log "D-23 start"
mv $S/d23/results $S/d23/results-parallel 2>/dev/null
( cd $N/D-23-react-ssr && WORK=$S/d23 RUNS=3 DUR=10s ROWS=100 ./run.sh > $R/d23-r100.txt 2>&1 )
( cd $N/D-23-react-ssr && WORK=$S/d23 RUNS=3 DUR=10s ROWS=800 ./run.sh > $R/d23-r800.txt 2>&1 )
( cd $N/D-23-react-ssr && WORK=$S/d23 ROUNDS=2 DUR=8s ./scale.sh > $R/d23-scale.txt 2>&1 )
log "D-23 done"

log "D-25 bench start"
W=$S/d25
cp -r $W/out/bench.md $R/d25-bench-parallel.md 2>/dev/null
( cd $N/D-25-regex && node bench-prep.mjs "$W/out/cases.json" "$W/out" && node bench-node.mjs "$W/out" 5 &&   for e in re2fast hybridlre libregexpopt libregexp hybrid regexp2v2 regexp2v1 quickjs qjswasm goja; do ./bin/d25 bench -engine $e -cases "$W/out/bench-patterns.json" -harness harness.js -text "$W/out/bench-text.txt" -runs 5 -out "$W/out/bench-$e.json"; done &&   node bench-report.mjs "$W/out" > $R/d25-bench.md ) > $R/d25.log 2>&1
log "D-25 done"

log "D-16 start"
cp -r $N/D-16-ts-frontend/out $R/d16-out-parallel
( cd $N/D-16-ts-frontend && WORK=$S/d16 RUNS=5 ./bench.sh > $R/d16.txt 2>&1 )
cp $N/D-16-ts-frontend/out/summary.md $R/d16-summary.md
log "D-16 done"
touch $R/all-done
