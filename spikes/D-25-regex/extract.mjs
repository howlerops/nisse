// Extract regex literals and new RegExp("…") string patterns from installed npm packages.
// usage: node extract.mjs <node_modules dir> <out corpus.json>
import fs from "node:fs"; import path from "node:path"; import { createRequire } from "node:module";
const [,, nm, out] = process.argv;
const require = createRequire(path.join(nm, "x.js"));
const acorn = require("acorn"); const walk = require("acorn-walk");
const seen = new Map(); let files = 0;
function* jsFiles(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) { if (e.name !== ".bin" && e.name !== "test" && e.name !== "tests") yield* jsFiles(p); }
    else if (/\.(c|m)?js$/.test(e.name) && !/\.min\.js$/.test(e.name)) yield p;
  }
}
for (const f of jsFiles(nm)) {
  const src = fs.readFileSync(f, "utf8"); if (src.length > 3e6) continue;
  let ast; try { ast = acorn.parse(src, { ecmaVersion: "latest", sourceType: "module", allowHashBang: true, allowReturnOutsideFunction: true }); }
  catch { try { ast = acorn.parse(src, { ecmaVersion: "latest", sourceType: "script", allowHashBang: true, allowReturnOutsideFunction: true }); } catch { continue; } }
  files++;
  const pkg = path.relative(nm, f).split(path.sep).slice(0, 1)[0];
  const add = (pattern, flags, kind) => { const k = flags + "/" + pattern; if (!seen.has(k)) seen.set(k, { pattern, flags, kind, pkg }); };
  walk.full(ast, n => {
    if (n.type === "Literal" && n.regex) add(n.regex.pattern, n.regex.flags, "literal");
    if ((n.type === "NewExpression" || n.type === "CallExpression") && n.callee.type === "Identifier" && n.callee.name === "RegExp"
        && n.arguments[0]?.type === "Literal" && typeof n.arguments[0].value === "string") {
      const fl = n.arguments[1]?.type === "Literal" && typeof n.arguments[1].value === "string" ? n.arguments[1].value : "";
      add(n.arguments[0].value, fl, "constructor");
    }
  });
}
const list = [...seen.values()].filter(r => { try { new RegExp(r.pattern, r.flags); return true; } catch { return false; } });
fs.writeFileSync(out, JSON.stringify(list));
console.error(`files=${files} unique=${list.length}`);
