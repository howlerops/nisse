// Option C: type dump through the classic TypeScript 6 compiler API (JS, in Node).
// Usage: node dump-ts6.mjs <tsconfig.json> <out.jsonl>
// One JSON line per identifier in the project's own source files.
import ts from "ts6";
import fs from "node:fs";
import path from "node:path";

const [configPath, outPath] = process.argv.slice(2);
const t0 = performance.now();
const parsed = ts.getParsedCommandLineOfConfigFile(path.resolve(configPath), {}, {
  ...ts.sys,
  onUnRecoverableConfigFileDiagnostic: (d) => { throw new Error(ts.flattenDiagnosticMessageText(d.messageText, "\n")); },
});
const program = ts.createProgram({ rootNames: parsed.fileNames, options: parsed.options, projectReferences: parsed.projectReferences });
const checker = program.getTypeChecker();
const tLoad = performance.now();
const diagCount = ts.getPreEmitDiagnostics(program).length;
const tCheck = performance.now();

const root = path.dirname(path.resolve(configPath));
const out = fs.openSync(outPath, "w");
const flags = ts.TypeFormatFlags.NoTruncation;
let ids = 0;
const typeStr = (type) => (type ? checker.typeToString(type, undefined, flags) : null);
const declOf = (sym) => {
  const d = sym?.declarations?.[0];
  if (!d) return null;
  const sf = d.getSourceFile();
  const { line, character } = sf.getLineAndCharacterOfPosition(d.getStart(sf));
  return `${path.relative(root, sf.fileName)}:${line + 1}:${character + 1}`;
};
const sigStr = (sig) => {
  if (!sig) return null;
  const params = sig.getParameters().map((p) => typeStr(checker.getTypeOfSymbol(p)));
  return `(${params.join(", ")}) => ${typeStr(checker.getReturnTypeOfSignature(sig))}`;
};

for (const sf of program.getSourceFiles()) {
  if (sf.isDeclarationFile || program.isSourceFileFromExternalLibrary(sf)) continue;
  const rel = path.relative(root, sf.fileName);
  const seen = new Map();
  const lines = [];
  const visit = (node) => {
    if (ts.isIdentifier(node)) {
      const name = node.text;
      const k = (seen.get(name) ?? 0) + 1;
      seen.set(name, k);
      const sym = checker.getSymbolAtLocation(node);
      let sig = null;
      const p = node.parent;
      const callee = p && (ts.isCallExpression(p) || ts.isNewExpression(p)) && p.expression === node ? p
        : p && ts.isPropertyAccessExpression(p) && p.name === node && p.parent && (ts.isCallExpression(p.parent) || ts.isNewExpression(p.parent)) && p.parent.expression === p ? p.parent
        : null;
      if (callee) sig = sigStr(checker.getResolvedSignature(callee));
      lines.push(JSON.stringify({ f: rel, n: name, k, tp: ts.isPartOfTypeNode(node), t: typeStr(checker.getTypeAtLocation(node)), s: sym?.name ?? null, d: declOf(sym), sig }));
      ids++;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  if (lines.length) fs.writeSync(out, lines.join("\n") + "\n");
}
fs.closeSync(out);
const tEnd = performance.now();
console.error(JSON.stringify({ option: "C-ts6", version: ts.version, files: program.getSourceFiles().length, ids, diagnostics: diagCount, loadMs: Math.round(tLoad - t0), checkMs: Math.round(tCheck - tLoad), dumpMs: Math.round(tEnd - tCheck), totalMs: Math.round(tEnd - t0) }));
