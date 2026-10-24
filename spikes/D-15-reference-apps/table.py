#!/usr/bin/env python3
"""Print the D-15 scorecard tables (markdown) from results/scorecard.json."""
import json
import sys

rows = json.load(open(sys.argv[1] if len(sys.argv) > 1 else "results/scorecard.json"))


def lic(r):
    l = r["license"]
    s = l["spdx"]
    if l["enterprise_dirs"] or l.get("enterprise_license_files"):
        s += " + enterprise dir"
    return s


for tier in (1, 2, 3):
    print(f"\n### Tier {tier}\n")
    print("| Repo | Pinned SHA | Last commit | License | TS+TSX LOC | Files (ts/tsx) | Direct deps (rt+dev) | Transitive | Runtime native deps | Decorators | Stack | Tests (unit / e2e cases) | Services |")
    print("| --- | --- | --- | --- | ---: | --- | --- | ---: | --- | ---: | --- | --- | --- |")
    for r in rows:
        if r["tier"] != tier:
            continue
        stack = ", ".join(r["backend"] + [f for f in r["frontend"] if f not in r["backend"]] + r["orm"])
        if r["prisma_version"]:
            stack += f" (Prisma {r['prisma_version']})"
        print(f"| {r['repo']} | `{r['sha'][:12]}` | {r['last_commit'][:10]} | {lic(r)} | {r['ts_loc_total']:,} | "
              f"{r['files']['ts']}/{r['files']['tsx']} | {r['deps_direct_runtime']}+{r['deps_direct_dev']} | "
              f"{r['deps_transitive_unique']:,} | {', '.join(r['native_runtime']) or '—'} | {r['decorators']['count']:,} | "
              f"{stack} | {r['tests']['cases']:,} / {r['tests']['e2e_cases']:,} ({', '.join(r['test_frameworks'])}) | "
              f"{', '.join(r['services']) or '—'} |")
