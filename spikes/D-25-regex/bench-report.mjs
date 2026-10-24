// Summarises bench-*.json against Node as markdown tables.
// usage: node bench-report.mjs <outdir>
import fs from "node:fs";
const dir = process.argv[2];
const load = e => JSON.parse(fs.readFileSync(`${dir}/bench-${e}.json`, "utf8"));
const engines = fs.readdirSync(dir).filter(f => /^bench-.*\.json$/.test(f) && f !== "bench-patterns.json").map(f => f.slice(6, -5));
const node = load("node"); const re2 = engines.includes("re2fast") ? load("re2fast") : { perPatternUs: {} };
const q = (xs, p) => { const s = [...xs].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };
const fmt = x => x >= 10 ? x.toFixed(0) + "×" : x.toFixed(1) + "×";
const us = x => x >= 100 ? x.toFixed(0) : x >= 1 ? x.toFixed(1) : x.toFixed(2);
const order = ["node", "re2fast", "hybridlre", "libregexpopt", "libregexp", "hybrid", "regexp2v2", "regexp2v1", "quickjs", "qjswasm", "goja"];
console.log("| Engine | Compile all 2,056 corpus patterns, cold (ms) | Per-line exec: median µs | vs Node median | vs Node p90 | Global match over 16 KB: median µs | vs Node median | vs Node p90 | Patterns |");
console.log("| --- | --- | --- | --- | --- | --- | --- | --- | --- |");
for (const e of order.filter(x => engines.includes(x))) {
  const b = load(e); const r = [], rl = [], absG = [], absL = [];
  for (const [i, v] of Object.entries(b.perPatternUs)) { const n = node.perPatternUs[i]; if (n > 0 && v > 0) r.push(v / n); absG.push(v); }
  for (const [i, v] of Object.entries(b.perLineUs || {})) { const n = node.perLineUs[i]; if (n > 0 && v > 0) rl.push(v / n); absL.push(v); }
  console.log(`| ${e} | ${b.compileMs.toFixed(0)} (${b.compiled} ok) | ${us(q(absL, .5))} | ${fmt(q(rl, .5))} | ${fmt(q(rl, .9))} | ${us(q(absG, .5))} | ${fmt(q(r, .5))} | ${fmt(q(r, .9))} | ${r.length} |`);
}
