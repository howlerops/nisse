// Runs the same test262 file set in Node 22 (vm context per test) and writes results in the Go runner's format.
// usage: node t262-node.mjs <test262 dir> <out.json>
import fs from "node:fs"; import path from "node:path"; import vm from "node:vm";
const [,, dir, out] = process.argv;
const list = []; (function walk(d) { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else if (p.endsWith(".js") && !p.includes("_FIXTURE")) list.push(p); } })(path.join(dir, "test"));
list.sort();
const H = n => fs.readFileSync(path.join(dir, "harness", n), "utf8");
const listOf = (front, key) => { let m = front.match(new RegExp("^" + key + ":\\s*\\[([^\\]]*)\\]", "m")); if (m) return m[1].split(",").map(s => s.trim()).filter(Boolean);
  m = front.match(new RegExp("^" + key + ":\\s*\\n((?:\\s+-\\s+.*\\n)+)", "m")); return m ? m[1].split("\n").map(l => l.trim().replace(/^-\s*/, "")).filter(Boolean) : []; };
const group = (rel, feats) => { const f = feats.find(f => f.startsWith("regexp-")); if (f) return f; const p = rel.split(path.sep);
  if (p[2] === "RegExp") { if (p[3] === "prototype" && p[4]) return "RegExp.prototype." + p[4]; if (p[3].endsWith(".js")) return "RegExp (top-level)"; return "RegExp/" + p[3]; }
  if (p[2] === "String") return "String.prototype." + p[4]; return p.slice(1, 3).join("/") + "/" + p[3]; };
const res = [];
for (const f of list) {
  const src = fs.readFileSync(f, "utf8"); const fm = (src.match(/\/\*---([\s\S]*?)---\*\//) || [, ""])[1] + "\n";
  const flags = listOf(fm, "flags"), feats = listOf(fm, "features"), inc = listOf(fm, "includes");
  const neg = (fm.match(/^negative:\s*\n(?:\s+.*\n)*?\s+type:\s*(\w+)/m) || [])[1];
  const rel = path.relative(dir, f); const g = group(rel, feats);
  const sk = flags.find(x => ["module", "async", "raw", "CanBlockIsFalse", "CanBlockIsTrue"].includes(x));
  if (sk) { res.push({ group: g, file: rel, result: "skip", msg: sk }); continue; }
  let code = (flags.includes("onlyStrict") ? '"use strict";\n' : "") + H("assert.js") + "\n" + H("sta.js") + "\n" + inc.map(H).join("\n") + "\n" + src;
  let err = "";
  try { vm.runInContext("var print=function(){};\n" + code, vm.createContext({}), { timeout: 10000 }); } catch (e) { err = (e && e.name ? e.name + ": " : "") + (e && e.message || String(e)); }
  let r = "pass"; if (neg && !err) { r = "fail"; err = "expected " + neg; } else if (neg && !err.includes(neg)) r = "fail"; else if (!neg && err) r = "fail";
  res.push({ group: g, file: rel, result: r, msg: r === "pass" ? "" : err.slice(0, 160) });
}
fs.writeFileSync(out, JSON.stringify(res));
console.error(`node: ${list.length} test262 files`);
