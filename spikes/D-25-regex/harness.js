// Shared case runner. Runs unchanged in Node, goja and QuickJS so every engine is
// measured by the same code. ES5 syntax on purpose.
function __advance(s, i, unicode) {
  if (!unicode || i + 1 >= s.length) return i + 1;
  var c = s.charCodeAt(i);
  if (c >= 0xD800 && c <= 0xDBFF) { var d = s.charCodeAt(i + 1); if (d >= 0xDC00 && d <= 0xDFFF) return i + 2; }
  return i + 1;
}
function __runCase(p, f, s) {
  var re;
  try { re = new RegExp(p, f); } catch (e) { return { ce: String(e && e.message || e).slice(0, 80) }; }
  var out = { m: [] };
  var g = f.indexOf("g") >= 0 || f.indexOf("y") >= 0;
  var u = f.indexOf("u") >= 0 || f.indexOf("v") >= 0;
  var hasD = f.indexOf("d") >= 0;
  for (var k = 0; k < 20; k++) {
    var m = re.exec(s);
    if (m === null) { out.m.push(null); break; }
    var rec = { i: m.index, c: [], li: re.lastIndex };
    for (var j = 0; j < m.length; j++) rec.c.push(m[j] === undefined ? null : m[j]);
    if (m.groups) { rec.g = []; for (var key in m.groups) rec.g.push([key, m.groups[key] === undefined ? null : m.groups[key]]); }
    if (hasD && m.indices) { rec.d = []; for (var j2 = 0; j2 < m.indices.length; j2++) rec.d.push(m.indices[j2] === undefined ? null : [m.indices[j2][0], m.indices[j2][1]]); }
    out.m.push(rec);
    if (!g) break;
    if (m[0] === "") re.lastIndex = __advance(s, re.lastIndex, u);
  }
  re.lastIndex = 0;
  out.r = s.replace(re, "<$&|$1>");
  re.lastIndex = 0;
  out.sp = s.split(re, 10);
  for (var q = 0; q < out.sp.length; q++) if (out.sp[q] === undefined) out.sp[q] = null;
  return out;
}
