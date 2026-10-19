// Option A: type dump through TypeScript 7's (unstable) API: a JS client talking
// msgpack over IPC to the native Go compiler.
// Usage: node dump-ts7.mjs <tsconfig.json> <out.jsonl>
// Same output schema as dump-ts6.mjs.
// TS7_PKG selects the package: ts7 (7.0.2, default) or ts7next (typescript@next).
const pkg = process.env.TS7_PKG ?? "ts7";
const { API } = await import(`${pkg}/unstable/sync`);
const { SyntaxKind } = await import(`${pkg}/unstable/ast`);
import fs from "node:fs";
import path from "node:path";

const [configArg, outPath] = process.argv.slice(2);
const configPath = path.resolve(configArg);
const root = path.dirname(configPath);
const t0 = performance.now();
const api = new API({ cwd: root, collectTiming: true });
const snapshot = api.updateSnapshot({ openProjects: [configPath] });
const project = snapshot.getProject(configPath);
const { program, checker } = project;
const tLoad = performance.now();
const diagCount = program.getSemanticDiagnostics().length + program.getSyntacticDiagnostics().length;
const tCheck = performance.now();

const TYPE_FORMAT_NO_TRUNCATION = 1;
const API_ERROR = { id: -1 };
const apiErrors = [];
const typeCache = new Map();
const typeStr = (type) => {
  if (type === API_ERROR) return "<api-error>";
  if (!type) return null;
  let s = typeCache.get(type.id);
  if (s === undefined) {
    try { s = checker.typeToString(type, undefined, TYPE_FORMAT_NO_TRUNCATION); } catch (e) { apiErrors.push(`typeToString: ${String(e.message).split("\n")[0]}`); s = "<api-error>"; }
    typeCache.set(type.id, s);
  }
  return s;
};
// Line/column (1-based, UTF-16 columns) from a node, to compare with TypeScript 6.
const lineStarts = new Map();
const lineCol = (sf, pos) => {
  let ls = lineStarts.get(sf.fileName);
  if (!ls) {
    ls = [0];
    const text = sf.text;
    for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) ls.push(i + 1);
    lineStarts.set(sf.fileName, ls);
  }
  let lo = 0, hi = ls.length - 1;
  while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (ls[mid] <= pos) lo = mid; else hi = mid - 1; }
  return `${lo + 1}:${pos - ls[lo] + 1}`;
};
const declCache = new Map();
const declOf = (sym) => {
  if (!sym || sym === API_ERROR) return null;
  if (declCache.has(sym.id)) return declCache.get(sym.id);
  let res = null;
  const h = sym.declarations?.[0];
  const node = h?.resolve(project);
  if (node) {
    const sf = node.getSourceFile();
    res = `${path.relative(root, sf.fileName)}:${lineCol(sf, node.getStart(sf))}`;
  }
  declCache.set(sym.id, res);
  return res;
};
const sigStr = (sig) => {
  if (!sig) return null;
  const params = sig.getParameters();
  const types = params.length ? checker.getTypeOfSymbol(params) : [];
  return `(${types.map(typeStr).join(", ")}) => ${typeStr(checker.getReturnTypeOfSignature(sig))}`;
};
const isCallLike = (n) => n && (n.kind === SyntaxKind.CallExpression || n.kind === SyntaxKind.NewExpression);

const out = fs.openSync(outPath, "w");
let ids = 0;
const fileNames = program.getSourceFileNames();
for (const fileName of fileNames) {
  const sf = program.getSourceFile(fileName);
  if (!sf || sf.isDeclarationFile || program.isSourceFileFromExternalLibrary(sf)) continue;
  const rel = path.relative(root, sf.fileName);
  const nodes = [];
  const visit = (node) => {
    if (node.kind === SyntaxKind.Identifier) nodes.push(node);
    node.forEachChild(visit);
  };
  visit(sf);
  if (!nodes.length) continue;
  // Batched requests: one round trip each for the whole file. If the server fails on a
  // batch, retry node by node and record which nodes fail.
  const batch = (fn) => {
    try { return fn(nodes); } catch {
      return nodes.map((n) => { try { return fn(n); } catch (e) { apiErrors.push(`${rel}: ${String(e.message).split("\n")[0]}`); return API_ERROR; } });
    }
  };
  const types = batch((x) => checker.getTypeAtLocation(x));
  const syms = batch((x) => checker.getSymbolAtLocation(x));
  const seen = new Map();
  const lines = [];
  nodes.forEach((node, i) => {
    const name = node.text;
    const k = (seen.get(name) ?? 0) + 1;
    seen.set(name, k);
    const p = node.parent;
    const callee = isCallLike(p) && p.expression === node ? p
      : p && p.kind === SyntaxKind.PropertyAccessExpression && p.name === node && isCallLike(p.parent) && p.parent.expression === p ? p.parent
      : null;
    let sig = null;
    if (callee) { try { sig = sigStr(checker.getResolvedSignature(callee)); } catch (e) { apiErrors.push(`signature: ${String(e.message).split("\n")[0]}`); sig = "<api-error>"; } }
    const sym = syms[i];
    lines.push(JSON.stringify({ f: rel, n: name, k, t: typeStr(types[i]), s: sym === API_ERROR ? "<api-error>" : sym?.name ?? null, d: declOf(sym), sig }));
  });
  ids += nodes.length;
  fs.writeSync(out, lines.join("\n") + "\n");
}
fs.closeSync(out);
const tEnd = performance.now();
const timing = api.getTimingInfo();
api.close();
const errKinds = {};
for (const e of apiErrors) { const m = e.replace(/^[^:]*: /, ""); errKinds[m] = (errKinds[m] ?? 0) + 1; }
console.error(JSON.stringify({ option: "A-ts7-api", pkg, apiErrors: apiErrors.length, apiErrorKinds: errKinds, files: fileNames.length, ids, diagnostics: diagCount, loadMs: Math.round(tLoad - t0), checkMs: Math.round(tCheck - tLoad), dumpMs: Math.round(tEnd - tCheck), totalMs: Math.round(tEnd - t0), serverTimeMs: Math.round(timing?.totals?.serverTimeMs ?? 0), requests: timing?.totals?.requestCount, bytesReceived: timing?.totals?.bytesReceived }));
