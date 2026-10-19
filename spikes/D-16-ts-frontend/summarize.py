#!/usr/bin/env python3
"""Turn out/bench.log and out/cmp-*.json into markdown tables (medians over runs)."""
import json, statistics, sys, collections, glob, os
out = sys.argv[1]
runs = collections.defaultdict(lambda: collections.defaultdict(list))
for line in open(os.path.join(out, "bench.log")):
    label, payload = line.rstrip("\n").split("\t", 1)
    if payload.startswith("MEASURE "):
        m = json.loads(payload[8:])
        runs[label]["wallMs"].append(m["wallMs"]); runs[label]["peakTreeRssMB"].append(m["peakTreeRssMB"])
    else:
        s = json.loads(payload)
        for k in ("loadMs", "checkMs", "dumpMs", "totalMs", "ids", "apiErrors", "requests", "bytesReceived"):
            if k in s and s[k] is not None: runs[label][k].append(s[k])
med = lambda xs: statistics.median(xs) if xs else None
print("| Project · option | Runs | Wall (s) | Load (s) | Check (s) | Dump (s) | Peak RSS, process tree (MB) | API errors | IPC requests | IPC MB received |")
print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
for label in sorted(runs):
    r = runs[label]; f = lambda k, d=1000: "–" if med(r[k]) is None else f"{med(r[k]) / d:.1f}" if d != 1 else f"{med(r[k]):.0f}"
    print(f"| {label} | {len(r['wallMs'])} | {f('wallMs')} | {f('loadMs')} | {f('checkMs')} | {f('dumpMs')} | {f('peakTreeRssMB', 1)} | {f('apiErrors', 1)} | {f('requests', 1)} | {'–' if med(r['bytesReceived']) is None else round(med(r['bytesReceived']) / 1e6)} |")
print()
print("| Comparison | Identifiers | Exact | Cosmetic | Type-position (API) | Real difference | Symbols agree |")
print("| --- | --- | --- | --- | --- | --- | --- |")
for p in sorted(glob.glob(os.path.join(out, "cmp-*.json"))):
    d = json.load(open(p)); c = d.get("classes", {}); g = lambda k: f"{c[k]['pct']:.2f}%" if k in c else "0%"
    name = os.path.basename(p)[4:-5]
    print(f"| {name} | {d['common']} | {g('exact')} | {g('cosmetic (order or lib path)')} | {g('type-position identifier, same symbol (API difference)')} | {g('real difference')} | {d['s']['agree_pct']:.2f}% |")
