// Summarises bench-*.json against Node as a markdown table.
// usage: node bench-report.mjs <outdir>
import fs from "node:fs";
const dir = process.argv[2];
const load = e => JSON.parse(fs.readFileSync(`${dir}/bench-${e}.json`, "utf8"));
const engines = fs.readdirSync(dir).filter(f => /^bench-.*\.json$/.test(f) && f !== "bench-patterns.json").map(f => f.slice(6, -5));
const node = load("node"); const re2 = engines.includes("re2fast") ? load("re2fast") : { perPatternUs: {} };
const q = (xs, p) => { const s = [...xs].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };
const geo = xs => Math.exp(xs.reduce((a, x) => a + Math.log(x), 0) / xs.length);
const fmt = x => x >= 10 ? x.toFixed(0) + "×" : x.toFixed(2) + "×";
console.log("| Engine | Compile all corpus patterns (ms) | Match time vs Node: median | p90 | geo-mean | RE2-eligible subset: median vs Node | patterns timed |");
console.log("| --- | --- | --- | --- | --- | --- | --- |");
const order = ["node", "re2fast", "hybridlre", "libregexp", "hybrid", "regexp2v2", "regexp2v1", "quickjs", "qjswasm", "goja"];
for (const e of order.filter(x => engines.includes(x))) {
  const b = load(e); const r = [], rr = [];
  for (const [i, us] of Object.entries(b.perPatternUs)) { const n = node.perPatternUs[i]; if (n > 0 && us > 0) { r.push(us / n); if (re2.perPatternUs[i] !== undefined) rr.push(us / n); } }
  console.log(`| ${e} | ${b.compileMs.toFixed(0)} (${b.compiled} ok) | ${r.length ? fmt(q(r, 0.5)) : "—"} | ${r.length ? fmt(q(r, 0.9)) : "—"} | ${r.length ? fmt(geo(r)) : "—"} | ${rr.length ? fmt(q(rr, 0.5)) : "—"} | ${r.length} |`);
}
