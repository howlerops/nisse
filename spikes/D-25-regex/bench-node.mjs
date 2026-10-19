// Node baseline for the Go bench command: same patterns, same text, same output format.
// usage: node bench-node.mjs <outdir> <runs>
import fs from "node:fs";
const [,, dir, runsS] = process.argv; const runs = +runsS;
const pats = JSON.parse(fs.readFileSync(dir + "/bench-patterns.json", "utf8"));
const clist = JSON.parse(fs.readFileSync(dir + "/compile-list.json", "utf8"));
const text = fs.readFileSync(dir + "/bench-text.txt", "utf8");
const med = xs => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
const ct = []; let compiled = 0;
for (let r = 0; r < runs; r++) {
  // V8 compiles lazily, so "compile" here is construct + first test(), as for every other engine.
  const t0 = process.hrtime.bigint(); compiled = 0;
  for (const [p, f] of clist) { try { new RegExp(p, f).test("x"); compiled++; } catch {} }
  ct.push(Number(process.hrtime.bigint() - t0) / 1e6);
}
const samples = {};
for (let r = 0; r < runs; r++) pats.forEach((p, i) => {
  const re = new RegExp(p.p, p.f); text.match(re); // warm-up
  const t0 = process.hrtime.bigint(); for (let k = 0; k < 3; k++) text.match(re);
  (samples[i] ||= []).push(Number(process.hrtime.bigint() - t0) / 1e3 / 3);
});
const perPatternUs = {}; for (const [i, xs] of Object.entries(samples)) perPatternUs[i] = med(xs);
fs.writeFileSync(dir + "/bench-node.json", JSON.stringify({ engine: "node", compileMs: med(ct), compiled, perPatternUs }));
console.error(`node: compile ${med(ct).toFixed(0)}ms, ${Object.keys(perPatternUs).length} patterns timed`);
