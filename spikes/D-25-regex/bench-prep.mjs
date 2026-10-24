// Writes bench-patterns.json (sample of corpus patterns, made global), compile-list.json (all corpus
// patterns) and bench-text.txt (~16 KB of mixed backend-style text, BMP only, \n line ends).
// usage: node bench-prep.mjs <cases.json> <outdir>
import fs from "node:fs";
const [,, casesFile, dir] = process.argv;
const cases = JSON.parse(fs.readFileSync(casesFile, "utf8")).filter(c => c.src.startsWith("npm:"));
fs.writeFileSync(dir + "/compile-list.json", JSON.stringify(cases.map(c => [c.p, c.f])));
const sample = [];
cases.forEach((c, i) => { if (i % 7 === 0 && sample.length < 300) { const f = [...new Set((c.f.replace(/[yd]/g, "") + "g").split(""))].join(""); try { new RegExp(c.p, f); sample.push({ p: c.p, f }); } catch {} } });
fs.writeFileSync(dir + "/bench-patterns.json", JSON.stringify(sample));
let seed = 7; const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
const parts = [
  () => `2026-10-${String(1 + Math.floor(rnd() * 28)).padStart(2, "0")}T12:${String(Math.floor(rnd() * 60)).padStart(2, "0")}:00.000Z INFO request id=${Math.floor(rnd() * 1e9)} path=/api/v1/users/${Math.floor(rnd() * 1000)} status=200 dur=${(rnd() * 100).toFixed(2)}ms`,
  () => JSON.stringify({ id: Math.floor(rnd() * 1e6), name: "Ünïcode Ñame", email: "user" + Math.floor(rnd() * 1000) + "@example.com", tags: ["a", "b"], ok: rnd() > 0.5 }),
  () => `<div class="item item-${Math.floor(rnd() * 10)}"><a href="https://example.com/p?q=${Math.floor(rnd() * 100)}&x=y">Link text</a> <span>Привет мир</span></div>`,
  () => "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
  () => `function f${Math.floor(rnd() * 100)}(a, b) { return a + b * ${Math.floor(rnd() * 10)}; } // comment`,
  () => `    key_${Math.floor(rnd() * 50)}: "value with spaces"   # yaml-ish`,
];
let text = ""; while (text.length < 16000) text += parts[Math.floor(rnd() * parts.length)]() + "\n";
fs.writeFileSync(dir + "/bench-text.txt", text);
console.error(`bench: ${sample.length} patterns, ${cases.length} for compile, text ${text.length} chars`);
