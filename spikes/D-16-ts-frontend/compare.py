#!/usr/bin/env python3
"""Compare two dumps (JSONL) identifier by identifier.
Usage: compare.py a.jsonl b.jsonl [--examples N]
Key = (file, identifier text, occurrence number), so byte vs UTF-16 offsets don't matter."""
import json, sys, collections

def load(p):
    d = {}
    for line in open(p):
        r = json.loads(line)
        d[(r["f"], r["n"], r["k"])] = r
    return d

a_path, b_path = sys.argv[1], sys.argv[2]
n_ex = int(sys.argv[sys.argv.index("--examples") + 1]) if "--examples" in sys.argv else 5
a, b = load(a_path), load(b_path)
keys = a.keys() & b.keys()
res = {"only_a": len(a.keys() - b.keys()), "only_b": len(b.keys() - a.keys()), "common": len(keys)}
for field in ("t", "s", "d", "sig"):
    diff = [k for k in keys if a[k][field] != b[k][field]]
    res[field] = {"agree": len(keys) - len(diff), "differ": len(diff), "agree_pct": round(100 * (len(keys) - len(diff)) / max(1, len(keys)), 3)}
    ex = collections.Counter((a[k][field], b[k][field]) for k in diff).most_common(n_ex)
    res[field]["top_differences"] = [{"a": x, "b": y, "count": c} for (x, y), c in ex]
import re
def norm(x):
    """Cosmetic normalisation: lib file locations, and member order inside unions and object types."""
    if x is None: return None
    x = re.sub(r"\S*/lib/(lib\.[\w.]+\.d\.ts)", r"<lib>/\1", x)
    return "".join(sorted(re.findall(r"\w+|[^\w\s]", x)))
tp = lambda k: a[k].get("tp") or b[k].get("tp")
cls = collections.Counter()
for k in keys:
    if all(a[k][f] == b[k][f] for f in ("t", "s", "d", "sig")): cls["exact"] += 1
    elif all(norm(a[k][f]) == norm(b[k][f]) for f in ("t", "s", "d", "sig")): cls["cosmetic (order or lib path)"] += 1
    elif tp(k) and a[k]["s"] == b[k]["s"] and norm(a[k]["d"]) == norm(b[k]["d"]): cls["type-position identifier, same symbol (API difference)"] += 1
    else: cls["real difference"] += 1
res["classes"] = {c: {"count": n, "pct": round(100 * n / max(1, len(keys)), 3)} for c, n in cls.most_common()}
real = [k for k in keys if not all(norm(a[k][f]) == norm(b[k][f]) for f in ("t", "s", "d", "sig")) and not (tp(k) and a[k]["s"] == b[k]["s"] and norm(a[k]["d"]) == norm(b[k]["d"]))]
res["real_difference_examples"] = [{"key": list(k), **{f: [a[k][f], b[k][f]] for f in ("t", "s", "d", "sig") if norm(a[k][f]) != norm(b[k][f])}} for k in real[:n_ex]]
all_agree = sum(1 for k in keys if all(a[k][f] == b[k][f] for f in ("t", "s", "d", "sig")))
res["all_fields_agree_pct"] = round(100 * all_agree / max(1, len(keys)), 3)
print(json.dumps(res, indent=1))
