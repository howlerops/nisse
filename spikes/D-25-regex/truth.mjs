// Node ground truth: runs harness.js in a vm context with a per-case timeout.
// usage: node truth.mjs <cases.json> <out results.json>
import fs from "node:fs"; import vm from "node:vm";
const [,, casesFile, out] = process.argv;
const cases = JSON.parse(fs.readFileSync(casesFile, "utf8"));
const ctx = vm.createContext({}); vm.runInContext(fs.readFileSync(new URL("./harness.js", import.meta.url), "utf8"), ctx);
const res = []; let timeouts = 0; const t0 = Date.now();
for (const c of cases) {
  const row = [];
  for (const s of c.in) {
    ctx.__p = c.p; ctx.__f = c.f; ctx.__s = s;
    try { row.push(JSON.stringify(vm.runInContext("__runCase(__p,__f,__s)", ctx, { timeout: 1000 }))); }
    catch (e) { row.push(JSON.stringify({ to: true })); timeouts++; }
  }
  res.push(row);
}
fs.writeFileSync(out, JSON.stringify(res));
console.error(`node: ${Date.now() - t0}ms timeouts=${timeouts}`);
