// Build cases.json: corpus patterns (sampled) + test262-derived feature patterns, each with inputs.
// usage: node gen.mjs <corpus-all.json> <node_modules> <out cases.json> [maxPatterns]
import fs from "node:fs"; import path from "node:path"; import { createRequire } from "node:module";
const [,, corpusFile, nm, out, maxP = "4000"] = process.argv;
const RandExp = createRequire(path.join(nm, "x.js"))("randexp");
let seed = 12345; const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
const generic = ["", "a", "abc", "Hello, World!", "foo bar  baz\tqux\n", "line1\r\nline2\nline3", "user@example.com",
  "https://example.com/path?q=1&r=two#frag", "2026-10-19T12:34:56.789Z", "  padded  ", "123-456-7890", "1.5e10 -42 0x1F",
  "<div class=\"x\">t</div>", "café ñandú", "ſ K k S s İ i", "😀 emoji 🎉", "a b c", " nbsp﻿bom",
  "{\"a\":[1,2,{\"b\":null}]}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaa!", "AbC aBc ABC"];
const mutate = s => { if (!s) return "x"; const i = Math.floor(rnd() * s.length); return rnd() < 0.5 ? s.slice(0, i) + s.slice(i + 1) : s.slice(0, i) + "Z" + s.slice(i); };
const ok = s => !/[\ud800-\udfff]/.test(s.replace(/[\ud800-\udbff][\udc00-\udfff]/g, "")) && s.length < 300;
function inputsFor(p, f) {
  const xs = new Set(generic);
  try {
    const r = new RandExp(new RegExp(p, f.replace(/[dvy]/g, "")));
    r.defaultRange.add(0xa0, 0x17f); r.max = 6; r.randInt = (a, b) => a + Math.floor(rnd() * (1 + b - a));
    for (let i = 0; i < 4; i++) { const s = r.gen(); if (ok(s)) { xs.add(s); xs.add(mutate(s)); xs.add("pre " + s + " post"); } }
  } catch {}
  return [...xs];
}
const corpus = JSON.parse(fs.readFileSync(corpusFile, "utf8"));
for (let i = corpus.length - 1; i > 0; i--) { const j = Math.floor(rnd() * (i + 1)); [corpus[i], corpus[j]] = [corpus[j], corpus[i]]; }
const picked = corpus.slice(0, +maxP).map(r => ({ src: "npm:" + r.pkg, p: r.pattern, f: r.flags }));
// Feature probes: hand-picked from test262 feature areas so every feature has coverage.
const probes = [
  ["named-groups", "(?<year>\\d{4})-(?<month>\\d{2})", ""], ["named-groups", "(?<a>x)|(?<b>y)", "g"], ["named-groups", "\\k<w>(?<w>a)", ""],
  ["duplicate-named-groups", "(?<n>a)|(?<n>b)", "g"],
  ["lookbehind", "(?<=\\$)\\d+(\\.\\d*)?", ""], ["lookbehind", "(?<!\\$)\\b\\d+", "g"], ["lookbehind", "(?<=(\\d)(\\d))x", ""], ["lookbehind", "(?<=\\1(a))b", ""],
  ["lookahead", "\\w+(?=,)", "g"], ["lookahead", "^(?!.*bad).*$", "m"],
  ["backreference", "(a)\\1", ""], ["backreference", "(['\"]).*?\\1", "g"], ["backreference", "\\1(a)", ""], ["backreference", "(?:(a)|b)\\1c", ""],
  ["capture-reset", "((a)|b)+", ""], ["capture-reset", "(z)((a+)?(b+)?(c))*", ""], ["capture-reset", "(a*)*b", ""], ["capture-reset", "(a|ab)(c|bcd)(d*)", ""],
  ["unicode", "^.$", "u"], ["unicode", "^.$", ""], ["unicode", "\\u{1F600}", "u"], ["unicode", "[😀-🎉]", "u"], ["unicode", "\\p{L}+", "gu"], ["unicode", "\\p{Script=Greek}", "u"], ["unicode", "\\P{ASCII}", "gu"], ["unicode", "\\p{Emoji_Presentation}", "gu"],
  ["unicode-sets-v", "[\\p{L}--[a-z]]", "gv"], ["unicode-sets-v", "[[a-z]&&[aeiou]]", "gv"], ["unicode-sets-v", "\\p{RGI_Emoji}", "gv"], ["unicode-sets-v", "[\\q{abc|d}]", "gv"],
  ["sticky", "a", "y"], ["sticky", "\\d", "gy"], ["sticky", "foo|bar", "y"],
  ["dotall", "a.b", "s"], ["dotall", "a.b", ""], ["dotall", "^.+$", "m"],
  ["has-indices", "(a)(?<n>b)?", "d"], ["has-indices", "(\\w)+", "dg"],
  ["case-folding", "s", "i"], ["case-folding", "s", "iu"], ["case-folding", "k", "i"], ["case-folding", "k", "iu"], ["case-folding", "[a-z]+", "i"], ["case-folding", "ß", "iu"], ["case-folding", "\\w", "iu"], ["case-folding", "i", "i"], ["case-folding", "[^k]", "iu"],
  ["multiline-anchors", "^\\w+$", "gm"], ["multiline-anchors", "$", "g"], ["multiline-anchors", "^", "gm"], ["multiline-anchors", "\\bfoo\\b", "g"],
  ["classes", "\\s+", "g"], ["classes", "\\S+", "g"], ["classes", "\\d+", "g"], ["classes", "[\\w-]+", "g"], ["classes", "[^]", "g"], ["classes", "[]", ""], ["classes", "\\cJ", ""], ["classes", "[\\b]", ""],
  ["annex-b", "\\a", ""], ["annex-b", "a{", ""], ["annex-b", "]", ""], ["annex-b", "\\8", ""], ["annex-b", "(?=a)*", ""], ["annex-b", "\\x4", ""], ["annex-b", "[\\d-z]", "g"],
  ["quantifiers", "a{2,3}?", "g"], ["quantifiers", "(?:ab)*?c", ""], ["quantifiers", "a+?", "g"], ["quantifiers", "x*", "g"], ["quantifiers", "a{0}", "g"],
  ["modifiers", "(?i:a)b", ""], ["modifiers", "(?-i:a)b", "i"], ["modifiers", "(?s:.)", ""],
  ["empty-match", "", "g"], ["empty-match", "\\b", "g"], ["empty-match", "(?:)", "gu"],
];
const featured = probes.map(([feat, p, f]) => ({ src: "probe:" + feat, p, f }));
const cases = [...featured, ...picked].map(c => ({ ...c, in: inputsFor(c.p, c.f) }));
const n = cases.reduce((a, c) => a + c.in.length, 0);
fs.writeFileSync(out, JSON.stringify(cases));
console.error(`patterns=${cases.length} inputs=${n}`);
