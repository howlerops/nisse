#!/usr/bin/env python3
"""Measure D-15 reference-app candidates.

Usage: python3 -I measure.py <clones-dir> [repos.txt] > results/scorecard.json

Reads each clone listed in repos.txt (cloned by clone.sh) and prints one JSON
object per repo with the metrics the D-15 report uses. Everything is static
analysis of the checkout and its lockfile; nothing is installed or executed.
"""
import json
import os
import re
import subprocess
import sys

SKIP_DIRS = {
    "node_modules", ".git", "dist", "build", ".next", "out", "coverage",
    ".turbo", ".yarn", "vendor", ".nx", "storybook-static", "generated",
}
SRC_EXT = {".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"}

NODE_BUILTINS = {
    "assert", "async_hooks", "buffer", "child_process", "cluster", "console",
    "crypto", "dgram", "diagnostics_channel", "dns", "events", "fs", "http",
    "http2", "https", "inspector", "module", "net", "os", "path", "perf_hooks",
    "process", "querystring", "readline", "repl", "stream", "string_decoder",
    "timers", "tls", "tty", "url", "util", "v8", "vm", "wasi", "worker_threads",
    "zlib", "test", "sqlite",
}

BUILTIN_SUBPATHS = {
    "fs/promises", "stream/promises", "stream/web", "util/types", "path/posix",
    "path/win32", "timers/promises", "dns/promises", "readline/promises",
    "assert/strict", "stream/consumers",
}

# Packages known to compile or download native code at install time.
KNOWN_NATIVE = {
    "bcrypt", "argon2", "sharp", "canvas", "sqlite3", "better-sqlite3",
    "node-gyp", "bufferutil", "utf-8-validate", "re2", "cpu-features",
    "ssh2", "node-pty", "leveldown", "libxmljs", "libxmljs2", "grpc",
    "@prisma/engines", "prisma", "@swc/core", "esbuild", "fsevents",
    "sodium-native", "zeromq", "kerberos", "snappy", "msgpackr-extract",
    "@napi-rs/canvas", "lightningcss", "@parcel/watcher", "node-rdkafka",
    "farmhash", "isolated-vm", "exiftool-vendored",
    "@tensorflow/tfjs-node", "onnxruntime-node", "pg-native", "heapdump",
    "unix-dgram", "deasync", "microtime", "nodejieba", "oracledb", "ffi-napi",
    "ref-napi", "@sentry/profiling-node", "@datadog/pprof", "@napi-rs/nice",
    "@napi-rs/lzma", "@documenso/skia-canvas", "aws-crt", "protobufjs-native",
    "@node-rs/argon2", "@node-rs/bcrypt", "@node-rs/xxhash", "@resvg/resvg-js",
    "@mapbox/node-pre-gyp", "prebuild-install", "node-addon-api", "nan",
}
# Packages whose native code only matters at build time, not in the running app.
BUILD_ONLY_NATIVE = {
    "@swc/core", "esbuild", "fsevents", "lightningcss", "@parcel/watcher",
    "node-gyp", "prebuild-install", "@mapbox/node-pre-gyp", "node-addon-api", "nan",
}

FRONTEND = ["next", "react", "vue", "nuxt", "svelte", "@sveltejs/kit", "solid-js",
            "@angular/core", "@remix-run/react", "react-router", "vite"]
BACKEND = ["express", "@nestjs/core", "fastify", "koa", "hono", "@hapi/hapi",
           "next", "@trpc/server", "graphql", "apollo-server", "@apollo/server"]
ORM = ["@prisma/client", "typeorm", "drizzle-orm", "kysely", "sequelize",
       "knex", "mongoose", "mikro-orm", "@mikro-orm/core", "objection"]
TEST_FW = ["jest", "vitest", "mocha", "ava", "@playwright/test", "cypress",
           "supertest", "newman", "@japa/runner", "tap", "uvu"]
SERVICES = ["postgres", "redis", "mysql", "mariadb", "mongo", "minio",
            "elasticsearch", "clickhouse", "kafka", "rabbitmq", "valkey",
            "typesense", "meilisearch", "localstack", "mailhog", "redpanda"]

IMPORT_RE = re.compile(
    r"""(?:from\s+|import\s*\(\s*|require\s*\(\s*|import\s+)['"]([^'"]+)['"]""")
DECORATOR_RE = re.compile(r"^\s*@[A-Za-z_]\w*(?:\.\w+)*\s*\(", re.M)
TEST_CALL_RE = re.compile(r"(?<![\w.])(?:it|test)(?:\.(?:each\s*\([^)]*\)|concurrent|only|skip|todo))?\s*\(\s*[`'\"]")


def walk(root):
    for d, dirs, files in os.walk(root):
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS and not x.startswith(".")
                   or x in (".github",)]
        for f in files:
            yield os.path.join(d, f)


def read(path, limit=4_000_000):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read(limit)
    except OSError:
        return ""


def git(root, *args):
    return subprocess.run(["git", "-C", root, *args], capture_output=True,
                          text=True).stdout.strip()


def detect_license(root):
    text = ""
    for name in ("LICENSE", "LICENSE.md", "LICENSE.txt", "license", "LICENCE",
                 "COPYING", "LICENSE.MIT", "LICENSE-MIT"):
        p = os.path.join(root, name)
        if os.path.exists(p):
            text = read(p, 20000)
            break
    # Split licensing: an open licence plus an enterprise licence file.
    extra = sorted(f for f in os.listdir(root)
                   if re.match(r"(LICENSE[-_.]?(ENTERPRISE|EE)|EE-.*LICENSE)", f, re.I))
    pkg = {}
    try:
        pkg = json.loads(read(os.path.join(root, "package.json")) or "{}")
    except json.JSONDecodeError:
        pass
    declared = pkg.get("license")
    t = text.lower()
    if "business source license" in t:
        spdx = "BUSL-1.1"
    elif "functional source license" in t:
        spdx = "FSL"
    elif "gnu affero general public license" in t:
        spdx = "AGPL-3.0"
    elif "gnu general public license" in t:
        spdx = "GPL"
    elif "apache license" in t:
        spdx = "Apache-2.0"
    elif "permission is hereby granted, free of charge" in t:
        spdx = "MIT"
    elif "isc license" in t or "permission to use, copy, modify, and/or distribute" in t:
        spdx = "ISC"
    else:
        spdx = declared or "unknown"
    # Mixed licensing (enterprise directories under a different license).
    ee = [d for d in ("ee", "enterprise", "packages/ee", "apps/ee")
          if os.path.isdir(os.path.join(root, d))]
    osi = spdx in {"MIT", "Apache-2.0", "AGPL-3.0", "GPL", "ISC", "BSD-3-Clause"}
    return {"spdx": spdx, "declared": declared, "osi_approved": osi,
            "enterprise_dirs": ee, "enterprise_license_files": extra}


def package_jsons(root):
    out = []
    for p in walk(root):
        if os.path.basename(p) == "package.json":
            try:
                out.append((p, json.loads(read(p))))
            except json.JSONDecodeError:
                pass
    return out


def lockfile_packages(root):
    """Return (kind, set of name@version, set of names, set of install-script names)."""
    pl = os.path.join(root, "package-lock.json")
    pn = os.path.join(root, "pnpm-lock.yaml")
    yl = os.path.join(root, "yarn.lock")
    bl = os.path.join(root, "bun.lock")
    pkgs, scripts = set(), set()
    if os.path.exists(pl):
        data = json.loads(read(pl, 200_000_000))
        for k, v in (data.get("packages") or {}).items():
            if "node_modules/" not in k:
                continue
            name = k.rsplit("node_modules/", 1)[1]
            pkgs.add(f"{name}@{v.get('version')}")
            if v.get("hasInstallScript"):
                scripts.add(name)
        if not pkgs:
            for name, v in (data.get("dependencies") or {}).items():
                pkgs.add(f"{name}@{v.get('version')}")
        kind = "npm"
    elif os.path.exists(pn):
        text = read(pn, 200_000_000)
        section = None
        for line in text.splitlines():
            if not line.startswith(" ") and line.endswith(":"):
                section = line[:-1]
                continue
            if section == "packages" and re.match(r"^  ['\"]?/?[^ ]", line) and line.rstrip().endswith(":"):
                key = line.strip().rstrip(":").strip("'\"").lstrip("/")
                # name@version(peer...)  or name/version (old format)
                key = re.sub(r"\(.*\)$", "", key)
                if "@" in key[1:]:
                    i = key.rindex("@")
                    pkgs.add(f"{key[:i]}@{key[i+1:]}")
                else:
                    pkgs.add(key)
            if section == "packages" and "requiresBuild: true" in line:
                pass
        # requiresBuild markers (pnpm <9)
        for m in re.finditer(r"\n  ['\"]?/?([^\s:'\"]+)['\"]?:\n(?:    .*\n)*?    requiresBuild: true", text):
            key = re.sub(r"\(.*\)$", "", m.group(1))
            scripts.add(key[: key.rindex("@")] if "@" in key[1:] else key)
        kind = "pnpm"
    elif os.path.exists(yl):
        text = read(yl, 200_000_000)
        cur = None
        for line in text.splitlines():
            if line and not line.startswith((" ", "#")) and line.endswith(":"):
                first = line.rstrip(":").split(",")[0].strip().strip('"')
                cur = first[: first.rindex("@")] if "@" in first[1:] else first
            elif cur and line.strip().startswith("version"):
                ver = line.split(None, 1)[1].strip().strip('"')
                if not cur.endswith("@workspace"):
                    pkgs.add(f"{cur}@{ver}")
                cur = None
        kind = "yarn"
    elif os.path.exists(bl):
        kind = "bun"
    else:
        kind = None
    names = {p[: p.rindex("@")] if "@" in p[1:] else p for p in pkgs}
    return kind, pkgs, names, scripts


def measure(root):
    git_sha = git(root, "rev-parse", "HEAD")
    date = git(root, "log", "-1", "--format=%cI")
    pjs = package_jsons(root)
    ws_names = {pj.get("name") for _, pj in pjs if pj.get("name")}
    direct_rt, direct_dev = set(), set()
    for _, pj in pjs:
        for k in (pj.get("dependencies") or {}):
            if k not in ws_names:
                direct_rt.add(k)
        for k in (pj.get("devDependencies") or {}):
            if k not in ws_names:
                direct_dev.add(k)
    all_direct = direct_rt | direct_dev
    kind, pkgs, names, scripts = lockfile_packages(root)

    loc = {"ts": 0, "tsx": 0, "js": 0}
    files = {"ts": 0, "tsx": 0, "js": 0}
    builtins = {}
    decorators = 0
    decorator_files = 0
    tests = {"files": 0, "cases": 0, "e2e_files": 0, "e2e_cases": 0}
    binding_gyp = []
    services = set()
    for p in walk(root):
        base = os.path.basename(p)
        ext = os.path.splitext(p)[1]
        rel = os.path.relpath(p, root)
        if base == "binding.gyp":
            binding_gyp.append(rel)
        if base.startswith(("docker-compose", "compose")) and ext in (".yml", ".yaml") \
                or (rel.startswith(".github/workflows") and ext in (".yml", ".yaml")):
            low = read(p).lower()
            for s in SERVICES:
                if re.search(r"image:\s*['\"]?[\w./-]*" + s, low) or re.search(r"\b" + s + r":\s*\n\s+image", low):
                    services.add(s)
        if base == "schema.prisma" or base.endswith(".prisma"):
            m = re.search(r'provider\s*=\s*"(postgresql|mysql|sqlite|mongodb|sqlserver|cockroachdb)"', read(p))
            if m:
                services.add("prisma:" + m.group(1))
        if ext not in SRC_EXT or base.endswith(".d.ts") or ".min." in base:
            continue
        text = read(p)
        n = sum(1 for line in text.splitlines() if line.strip())
        key = "tsx" if ext == ".tsx" else "ts" if ext in (".ts", ".mts", ".cts") else "js"
        loc[key] += n
        files[key] += 1
        for m in IMPORT_RE.finditer(text):
            spec = m.group(1)
            if spec.startswith("node:"):
                b = spec[5:].split("/")[0]
            elif spec in NODE_BUILTINS or spec in BUILTIN_SUBPATHS:
                b = spec.split("/")[0]
            else:
                continue  # path aliases such as "test/utils" are not builtins
            if b in NODE_BUILTINS and (b not in ("test", "sqlite") or spec.startswith("node:")):
                builtins[b] = builtins.get(b, 0) + 1
        if key in ("ts", "tsx"):
            d = len(DECORATOR_RE.findall(text))
            decorators += d
            decorator_files += 1 if d else 0
        is_test = re.search(r"\.(test|spec|e2e-spec|e2e)\.[cm]?[jt]sx?$", base) or "/__tests__/" in "/" + rel
        if is_test:
            c = len(TEST_CALL_RE.findall(text))
            e2e = re.search(r"(^|/)(e2e|playwright|cypress)(/|$)|e2e", rel) is not None
            if e2e:
                tests["e2e_files"] += 1
                tests["e2e_cases"] += c
            else:
                tests["files"] += 1
                tests["cases"] += c

    # Collapse per-platform binary packages (foo-linux-x64-gnu) onto their base.
    def base_name(n):
        return re.sub(r"-(linux|darwin|win32|freebsd|android|openharmony)(-[a-z0-9]+)*$", "", n)
    native = sorted({base_name(n) for n in names
                     if base_name(n) in KNOWN_NATIVE or base_name(n) in {"@napi-rs/canvas"}})
    runtime_native = sorted(n for n in native if n not in BUILD_ONLY_NATIVE)
    # Platform-specific prebuilt binaries (esbuild, swc, rollup, sharp ...).
    prebuilt = sorted({re.sub(r"-(linux|darwin|win32|freebsd|android).*$", "", n)
                       for n in names if re.search(r"-(linux|darwin|win32)-(x64|arm64|ia32)", n)})

    prisma = None
    for _, pj in pjs:
        for sect in ("dependencies", "devDependencies"):
            v = (pj.get(sect) or {}).get("@prisma/client") or (pj.get(sect) or {}).get("prisma")
            if v:
                prisma = v
    m = re.search(r"\d+", prisma or "")
    prisma_major = int(m.group()) if m else None

    def present(lst):
        return [x for x in lst if x in all_direct]

    return {
        "sha": git_sha,
        "last_commit": date,
        "license": detect_license(root),
        "loc": loc,
        "files": files,
        "ts_loc_total": loc["ts"] + loc["tsx"],
        "workspace_packages": len(pjs),
        "lockfile": kind,
        "deps_direct_runtime": len(direct_rt),
        "deps_direct_dev": len(direct_dev),
        "deps_transitive_unique": len(pkgs),
        "native_known": native,
        "native_runtime": runtime_native,
        "install_scripts": sorted(scripts),
        "prebuilt_platform_binaries": prebuilt,
        "binding_gyp_in_repo": binding_gyp,
        "node_builtins": dict(sorted(builtins.items(), key=lambda kv: -kv[1])),
        "decorators": {"count": decorators, "files": decorator_files},
        "frontend": present(FRONTEND),
        "backend": present(BACKEND),
        "orm": present(ORM),
        "prisma_version": prisma,
        # Prisma 7 defaults to the TypeScript query compiler (no Rust engine
        # at runtime); earlier majors run a native query-engine binary.
        "prisma_rust_engine": None if prisma_major is None else prisma_major < 7,
        "test_frameworks": present(TEST_FW),
        "tests": tests,
        "services": sorted(services),
    }


def main():
    clones = sys.argv[1]
    repos = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "repos.txt")
    out = []
    for line in open(repos):
        if not line.strip() or line.startswith("#"):
            continue
        tier, repo = line.split()
        root = os.path.join(clones, repo.replace("/", "_"))
        if not os.path.isdir(root):
            print(f"missing clone: {root}", file=sys.stderr)
            continue
        r = measure(root)
        r.update({"repo": repo, "tier": int(tier)})
        out.append(r)
        print(f"measured {repo}", file=sys.stderr)
    json.dump(out, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
