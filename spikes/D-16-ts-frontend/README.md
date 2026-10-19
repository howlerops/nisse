# D-16 spike: TypeScript front end for `nisse port`

Throwaway code for decision D-16. The report is [docs/spikes/D-16-ts-frontend.md](../../docs/spikes/D-16-ts-frontend.md).

Each option dumps one JSON line per identifier in a project's own source files (type string, symbol name, declaration location, resolved call signature), and `compare.py` diffs the dumps.

| File | Option |
| --- | --- |
| `dump-ts7.mjs` | A: TypeScript 7's native API (JS client, msgpack over IPC to the Go compiler); `TS7_PKG=ts7` (7.0.2) or `ts7next` (7.1 nightly) |
| `vendored/` | B: TypeScript's Go compiler packages vendored into our module and used as a library (`vendor.sh` + `cmd/nissedump`) |
| `dump-ts6.mjs` | C: the classic TypeScript 6.0.3 compiler API in Node (the type-dump half of option C) |
| `compare.py` | Identifier-by-identifier diff, classifying differences as exact, cosmetic, type-position API difference, or real |
| `apidiff/` | Exported-API diff of the Go compiler packages between two revisions (churn check) |
| `measure/` | Wall time and peak RSS of a whole process tree |
| `setup.sh`, `bench.sh`, `summarize.py` | Fetch and build everything, run N rounds, write `out/summary.md` |

Reproduce:

```sh
WORK=/tmp/nisse-d16 ./setup.sh          # clones, installs, vendors (~1.5 GB in $WORK)
WORK=/tmp/nisse-d16 RUNS=5 ./bench.sh   # ~25 minutes on 4 cores
```

Needs Node 22, pnpm, and Go (with `GOTOOLCHAIN=auto`; the vendored compiler needs Go 1.27).
