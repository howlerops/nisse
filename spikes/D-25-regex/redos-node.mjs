// Node 22 behaviour on the catastrophic-backtracking cases, with and without V8's linear-engine fallback.
import { spawnSync } from "node:child_process";
const cases = [
  ["nested quantifier", "^(a+)+$", "a".repeat(30) + "!"],
  ["overlapping alternation", "^(a|aa)+$", "a".repeat(40) + "!"],
  ["ambiguous adjacent", "(x+x+)+y", "x".repeat(30)],
  ["email-style (real CVE shape)", "^([a-zA-Z0-9])(([\\-.]|[_]+)?([a-zA-Z0-9]+))*(@){1}[a-z0-9]+[.]{1}(([a-z]{2,3})|([a-z]{2,3}[.]{1}[a-z]{2,3}))$", "a".repeat(32) + "!"],
];
for (const [label, flags] of [["node", []], ["node --enable-experimental-regexp-engine-on-excessive-backtracks", ["--enable-experimental-regexp-engine-on-excessive-backtracks"]]]) {
  for (const [name, p, s] of cases) {
    const code = `const t0=Date.now(); new RegExp(${JSON.stringify(p)}).exec(${JSON.stringify(s)}); console.log(Date.now()-t0)`;
    const t0 = Date.now(); const r = spawnSync(process.execPath, [...flags, "-e", code], { timeout: 15000 });
    const ms = r.error || r.signal ? null : +String(r.stdout).trim();
    console.log(`${label}\t${name}\t${ms === null ? ">15000ms" : ms + "ms"}\t${ms === null ? "hung (killed; no timeout without vm)" : "finished"}`);
  }
}
