#!/usr/bin/env python3
"""Non-blank TS/TSX/JS/Vue/Svelte lines under sub-paths of a clone.

Usage: python3 -I subtree_loc.py <clone> <subpath>...
Splits monorepos into server and web parts for the tier 3 scorecard.
"""
import os
import sys

SKIP = {"node_modules", ".git", "dist", "build", ".next", "coverage", "generated", ".turbo"}
EXT = {".ts": "ts", ".tsx": "tsx", ".js": "js", ".jsx": "js", ".mjs": "js",
       ".vue": "vue", ".svelte": "svelte"}

root = sys.argv[1]
for sub in sys.argv[2:]:
    counts = {}
    for d, dirs, files in os.walk(os.path.join(root, sub)):
        dirs[:] = [x for x in dirs if x not in SKIP]
        for f in files:
            ext = os.path.splitext(f)[1]
            if ext not in EXT or f.endswith(".d.ts"):
                continue
            with open(os.path.join(d, f), encoding="utf-8", errors="replace") as fh:
                n = sum(1 for line in fh if line.strip())
            counts[EXT[ext]] = counts.get(EXT[ext], 0) + n
    print(f"{os.path.basename(root)}/{sub}: total={sum(counts.values())} {counts}")
