package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"
	"unicode/utf16"
	"unicode/utf8"

	"github.com/dlclark/regexp2"
	regexp2v2 "github.com/dlclark/regexp2/v2"
	"github.com/dop251/goja"
	"github.com/fastschema/qjs"
	"modernc.org/quickjs"

	"nisse-spike/d25/re2at"
)

// Engine runs one case and returns the harness's JSON result shape.
type Engine interface {
	Run(p, f, s string) string
	Close()
}

func newEngine(name string, harness string) (Engine, error) {
	switch name {
	case "goja":
		return newGoja(harness)
	case "quickjs":
		return newModernc(harness)
	case "qjswasm":
		return newQJS(harness)
	case "regexp2v1":
		return &nativeEngine{kind: "v1"}, nil
	case "regexp2v2":
		return &nativeEngine{kind: "v2"}, nil
	case "re2fast":
		return &nativeEngine{kind: "re2"}, nil
	case "hybrid":
		return &nativeEngine{kind: "hybrid"}, nil
	case "libregexp":
		return newLRE(), nil
	case "libregexpopt":
		e := newLRE()
		e.opt = true
		return e, nil
	case "hybridlre":
		e := newLRE()
		e.opt = true
		return &hybridLRE{re2: &nativeEngine{kind: "re2"}, lre: e}, nil
	}
	return nil, fmt.Errorf("unknown engine %q", name)
}

const jsGlue = `
function __run(p,f,s){ return JSON.stringify(__runCase(p,f,s)); }
function __benchMatch(p,f,s,n){ var re=new RegExp(p,f); var c=0; for(var i=0;i<n;i++){ var m=s.match(re); c+= m?m.length:0; } return c; }
function __compileAll(list){ var c=0; for(var i=0;i<list.length;i++){ try { new RegExp(list[i][0], list[i][1]).test("x"); c++; } catch(e){} } return c; }
function __benchLines(p,f,lines,n){ var re=new RegExp(p,f); var c=0; for(var k=0;k<n;k++){ for(var i=0;i<lines.length;i++){ re.lastIndex=0; if(re.exec(lines[i])!==null) c++; } } return c; }
`

func jsCall(fn string, args ...any) string {
	var b strings.Builder
	b.WriteString(fn + "(")
	for i, a := range args {
		if i > 0 {
			b.WriteByte(',')
		}
		j, _ := json.Marshal(a)
		b.Write(j)
	}
	b.WriteString(")")
	return b.String()
}

// ---- goja ----

type gojaEngine struct {
	vm  *goja.Runtime
	run goja.Callable
}

func newGoja(h string) (*gojaEngine, error) {
	vm := goja.New()
	if _, err := vm.RunString(h + jsGlue); err != nil {
		return nil, err
	}
	fn, _ := goja.AssertFunction(vm.Get("__run"))
	return &gojaEngine{vm: vm, run: fn}, nil
}

func (g *gojaEngine) Run(p, f, s string) string {
	t := time.AfterFunc(time.Second, func() { g.vm.Interrupt("timeout") })
	defer func() { t.Stop(); g.vm.ClearInterrupt() }()
	v, err := g.run(goja.Undefined(), g.vm.ToValue(p), g.vm.ToValue(f), g.vm.ToValue(s))
	if err != nil {
		if strings.Contains(err.Error(), "timeout") {
			return `{"to":true}`
		}
		return mustJSON(map[string]string{"err": trunc(err.Error())})
	}
	return v.String()
}
func (g *gojaEngine) Eval(code string) (string, error) {
	v, err := g.vm.RunString(code)
	if err != nil {
		return "", err
	}
	return v.String(), nil
}
func (g *gojaEngine) Close() {}

// ---- modernc.org/quickjs (QuickJS transpiled to Go, cgo-free) ----

type moderncEngine struct{ vm *quickjs.VM }

func newModernc(h string) (*moderncEngine, error) {
	vm, err := quickjs.NewVM()
	if err != nil {
		return nil, err
	}
	if _, err := vm.Eval(h+jsGlue, quickjs.EvalGlobal); err != nil {
		return nil, err
	}
	vm.SetEvalTimeout(time.Second)
	return &moderncEngine{vm: vm}, nil
}

func (m *moderncEngine) Run(p, f, s string) string {
	v, err := m.vm.Eval(jsCall("__run", p, f, s), quickjs.EvalGlobal)
	if err != nil {
		if strings.Contains(strings.ToLower(err.Error()), "interrupt") {
			return `{"to":true}`
		}
		return mustJSON(map[string]string{"err": trunc(err.Error())})
	}
	return fmt.Sprint(v)
}
func (m *moderncEngine) Eval(code string) (string, error) {
	v, err := m.vm.Eval(code, quickjs.EvalGlobal)
	return fmt.Sprint(v), err
}
func (m *moderncEngine) Close() { m.vm.Close() }

// ---- fastschema/qjs (QuickJS-ng compiled to WASM, run on wazero) ----

type qjsEngine struct {
	rt *qjs.Runtime
}

func newQJS(h string) (*qjsEngine, error) {
	rt, err := qjs.New(qjs.Option{MaxExecutionTime: 1000})
	if err != nil {
		return nil, err
	}
	v, err := rt.Context().Eval("harness.js", qjs.Code(h+jsGlue))
	if err != nil {
		return nil, err
	}
	v.Free()
	return &qjsEngine{rt: rt}, nil
}

func (q *qjsEngine) Run(p, f, s string) string {
	out, err := q.Eval(jsCall("__run", p, f, s))
	if err != nil {
		if strings.Contains(strings.ToLower(err.Error()), "interrupt") {
			return `{"to":true}`
		}
		return mustJSON(map[string]string{"err": trunc(err.Error())})
	}
	return out
}
func (q *qjsEngine) Eval(code string) (string, error) {
	v, err := q.rt.Context().Eval("case.js", qjs.Code(code))
	if err != nil {
		return "", err
	}
	defer v.Free()
	return v.String(), nil
}
func (q *qjsEngine) Close() { q.rt.Close() }

func trunc(s string) string {
	if len(s) > 80 {
		return s[:80]
	}
	return s
}

func mustJSON(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}

// ---- native engines: what a Nisse runtime wrapper would do ----

type nativeEngine struct {
	kind  string
	cache map[string]*compiled
}

type compiled struct {
	plan   re2Plan
	re2    *re2at.Regexp
	v1     *regexp2.Regexp
	v2     *regexp2v2.Regexp
	groups []string
	err    string
	g, y   bool
	u, d   bool
}

func (n *nativeEngine) Close() {}

func (n *nativeEngine) compile(p, f string) *compiled {
	key := f + "/" + p
	if n.cache == nil {
		n.cache = map[string]*compiled{}
	}
	if c, ok := n.cache[key]; ok {
		return c
	}
	c := &compiled{groups: scanGroups(p, strings.Contains(f, "v")),
		g: strings.Contains(f, "g"), y: strings.Contains(f, "y"), u: strings.ContainsAny(f, "uv"), d: strings.Contains(f, "d")}
	if n.kind == "re2" || n.kind == "hybrid" {
		c.plan = translateRE2(p, f)
		if c.plan.ok {
			re, err := re2at.Compile(c.plan.goPattern)
			if err != nil {
				c.plan.ok, c.plan.reason = false, "go compile: "+err.Error()
			} else {
				c.re2 = re
			}
		}
		if n.kind == "re2" && !c.plan.ok {
			c.err = "ineligible: " + c.plan.reason
		}
	}
	if n.kind == "v1" || n.kind == "v2" || (n.kind == "hybrid") {
		pat := p
		if !c.u {
			pat = astralToSurrogateEscapes(p)
		}
		if n.kind == "v1" {
			o := regexp2.RegexOptions(regexp2.ECMAScript)
			if strings.Contains(f, "i") {
				o |= regexp2.IgnoreCase
			}
			if strings.Contains(f, "m") {
				o |= regexp2.Multiline
			}
			if strings.Contains(f, "s") {
				o |= regexp2.Singleline
			}
			if c.u {
				o |= regexp2.Unicode
			}
			re, err := regexp2.Compile(pat, o)
			if err != nil {
				c.err = "compile: " + trunc(err.Error())
			} else {
				re.MatchTimeout = time.Second
				c.v1 = re
			}
		} else {
			o := regexp2v2.ECMAScript
			if strings.Contains(f, "i") {
				o |= regexp2v2.IgnoreCase
			}
			if strings.Contains(f, "m") {
				o |= regexp2v2.Multiline
			}
			if strings.Contains(f, "s") {
				o |= regexp2v2.Singleline
			}
			if c.u {
				o |= regexp2v2.Unicode
			}
			re, err := regexp2v2.Compile(pat, o, regexp2v2.OptionMaintainCaptureOrder())
			if err != nil {
				c.err = "compile: " + trunc(err.Error())
			} else {
				re.MatchTimeout = time.Second
				c.v2 = re
			}
		}
	}
	n.cache[key] = c
	return c
}

func astralToSurrogateEscapes(p string) string {
	var b strings.Builder
	for _, r := range p {
		if r > 0xFFFF {
			hi, lo := utf16.EncodeRune(r)
			fmt.Fprintf(&b, `\u%04X\u%04X`, hi, lo)
		} else {
			b.WriteRune(r)
		}
	}
	return b.String()
}

type rec struct {
	I  int       `json:"i"`
	C  []*string `json:"c"`
	Li int       `json:"li"`
	G  [][]any   `json:"g,omitempty"`
	D  [][]int   `json:"d,omitempty"`
}

// match is one engine-neutral match: utf16 start/end per group (-1 if unset) plus text.
type match struct {
	idx  []int // utf16 offsets, 2 per group
	text []*string
}

func (n *nativeEngine) Run(p, f, s string) string {
	c := n.compile(p, f)
	if c.err != "" {
		return mustJSON(map[string]string{"ce": c.err})
	}
	in := newInput(s, c.u)
	useRE2 := c.re2 != nil
	if useRE2 && c.plan.astralUnsafe && in.hasAstral {
		useRE2 = false
	}
	if useRE2 && c.plan.lineTermMode && strings.ContainsAny(s, "\r  ") {
		useRE2 = false
	}
	if !useRE2 && n.kind == "re2" {
		return mustJSON(map[string]string{"ce": "runtime fallback needed"})
	}
	if !useRE2 && c.v2 == nil && c.v1 == nil {
		if n.kind == "hybrid" {
			// hybrid compiles regexp2 lazily
			c.v2 = mustV2(p, f, c.u)
		}
	}
	out := struct {
		M []*rec `json:"m"`
	}{M: []*rec{}}
	last := 0
	for k := 0; k < 20; k++ {
		start := 0
		if c.g || c.y {
			start = last
		}
		if start > in.len16 {
			out.M = append(out.M, nil)
			break
		}
		var m *match
		var err error
		if useRE2 {
			m = in.execRE2(c.re2, start)
		} else {
			m, err = in.execRegexp2(c, start)
			if err != nil {
				return `{"to":true}`
			}
		}
		if m != nil && c.y && m.idx[0] != start {
			m = nil
		}
		if m == nil {
			out.M = append(out.M, nil)
			break
		}
		r := &rec{I: m.idx[0], C: m.text}
		if c.g || c.y {
			r.Li = m.idx[1]
		}
		if hasNamed(c.groups) {
			for gi, name := range c.groups {
				if name != "" {
					var v any
					if m.text[gi+1] != nil {
						v = *m.text[gi+1]
					}
					r.G = append(r.G, []any{name, v})
				}
			}
		}
		if c.d {
			r.D = [][]int{}
			for gi := 0; gi < len(m.text); gi++ {
				if m.idx[2*gi] < 0 {
					r.D = append(r.D, nil)
				} else {
					r.D = append(r.D, []int{m.idx[2*gi], m.idx[2*gi+1]})
				}
			}
		}
		out.M = append(out.M, r)
		if !(c.g || c.y) {
			break
		}
		last = m.idx[1]
		if m.idx[0] == m.idx[1] {
			last = in.advance(last, c.u)
		}
	}
	return mustJSON(out)
}

func mustV2(p, f string, u bool) *regexp2v2.Regexp {
	pat := p
	if !u {
		pat = astralToSurrogateEscapes(p)
	}
	o := regexp2v2.ECMAScript
	for _, ch := range f {
		switch ch {
		case 'i':
			o |= regexp2v2.IgnoreCase
		case 'm':
			o |= regexp2v2.Multiline
		case 's':
			o |= regexp2v2.Singleline
		case 'u':
			o |= regexp2v2.Unicode
		}
	}
	re, err := regexp2v2.Compile(pat, o, regexp2v2.OptionMaintainCaptureOrder())
	if err != nil {
		return nil
	}
	re.MatchTimeout = time.Second
	return re
}

// input holds a string with the offset maps a JavaScript wrapper needs.
type input struct {
	s         string
	units     []uint16 // UTF-16 code units
	runes     []rune   // what regexp2 sees: code units (non-u) or code points (u)
	byte16    []int    // byte offset -> utf16 offset (at rune starts and len)
	u16byte   []int    // utf16 offset -> byte offset (-1 inside a pair)
	rune16    []int    // rune index (u mode) -> utf16 offset
	u16rune   []int
	len16     int
	hasAstral bool
}

func newInput(s string, u bool) *input {
	in := &input{s: s}
	in.byte16 = make([]int, len(s)+1)
	for bi, r := range s {
		in.byte16[bi] = len(in.units)
		if r > 0xFFFF {
			in.hasAstral = true
			hi, lo := utf16.EncodeRune(r)
			in.units = append(in.units, uint16(hi), uint16(lo))
		} else {
			in.units = append(in.units, uint16(r))
		}
	}
	in.byte16[len(s)] = len(in.units)
	in.len16 = len(in.units)
	in.u16byte = make([]int, in.len16+1)
	for i := range in.u16byte {
		in.u16byte[i] = -1
	}
	for bi, r := range s {
		in.u16byte[in.byte16[bi]] = bi
		_ = r
	}
	in.u16byte[in.len16] = len(s)
	if u {
		in.runes = []rune(s)
		in.rune16 = make([]int, len(in.runes)+1)
		in.u16rune = make([]int, in.len16+1)
		pos := 0
		for i, r := range in.runes {
			in.rune16[i] = pos
			in.u16rune[pos] = i
			pos += utf16.RuneLen(r)
			if r > 0xFFFF {
				in.u16rune[pos-1] = i + 1
			}
		}
		in.rune16[len(in.runes)] = pos
		in.u16rune[pos] = len(in.runes)
	} else {
		in.runes = make([]rune, len(in.units))
		for i, cu := range in.units {
			in.runes[i] = rune(cu)
		}
	}
	return in
}

func (in *input) advance(i int, u bool) int {
	if !u || i+1 >= in.len16 {
		return i + 1
	}
	if utf16.IsSurrogate(rune(in.units[i])) && in.units[i] < 0xDC00 && in.units[i+1] >= 0xDC00 {
		return i + 2
	}
	return i + 1
}

func (in *input) execRE2(re *re2at.Regexp, start int) *match {
	bstart := in.u16byte[start]
	if bstart < 0 {
		return nil
	}
	loc := re.FindStringSubmatchIndexAt(in.s, bstart)
	if loc == nil {
		return nil
	}
	m := &match{idx: make([]int, len(loc)), text: make([]*string, len(loc)/2)}
	for k := 0; k < len(loc); k += 2 {
		if loc[k] < 0 {
			m.idx[k], m.idx[k+1] = -1, -1
			continue
		}
		m.idx[k], m.idx[k+1] = in.byte16[loc[k]], in.byte16[loc[k+1]]
		t := in.s[loc[k]:loc[k+1]]
		m.text[k/2] = &t
	}
	return m
}

func (in *input) execRegexp2(c *compiled, start int) (*match, error) {
	rstart := start
	if c.u {
		rstart = in.u16rune[start]
	}
	toU16 := func(r int) int {
		if c.u {
			return in.rune16[r]
		}
		return r
	}
	text := func(lo, n int) *string {
		var t string
		if c.u {
			t = string(in.runes[lo : lo+n])
		} else {
			t = string(utf16.Decode(in.units[lo : lo+n]))
		}
		if !utf8.ValidString(t) {
			t = strings.ToValidUTF8(t, "�")
		}
		return &t
	}
	ng := len(c.groups) + 1
	m := &match{idx: make([]int, 2*ng), text: make([]*string, ng)}
	// JS group k maps to regexp2 group by name (named) or by unnamed counter.
	if c.v1 != nil {
		mm, err := c.v1.FindRunesMatchStartingAt(in.runes, rstart)
		if err != nil || mm == nil {
			return nil, err
		}
		unnamed := 0
		for k := 0; k < ng; k++ {
			var g *regexp2.Group
			if k == 0 {
				g = &mm.Group
			} else if c.groups[k-1] != "" {
				g = mm.GroupByName(c.groups[k-1])
			} else {
				unnamed++
				g = mm.GroupByNumber(unnamed)
			}
			if g == nil || len(g.Captures) == 0 {
				m.idx[2*k], m.idx[2*k+1] = -1, -1
				continue
			}
			m.idx[2*k], m.idx[2*k+1] = toU16(g.Index), toU16(g.Index+g.Length)
			m.text[k] = text(g.Index, g.Length)
		}
		return m, nil
	}
	if c.v2 == nil {
		return nil, fmt.Errorf("no engine")
	}
	mm, err := c.v2.FindRunesMatchStartingAt(in.runes, rstart)
	if err != nil || mm == nil {
		return nil, err
	}
	for k := 0; k < ng; k++ {
		var g *regexp2v2.Group
		if k == 0 {
			g = &mm.Group
		} else {
			g = mm.GroupByNumber(k)
		}
		if g == nil || len(g.Captures) == 0 {
			m.idx[2*k], m.idx[2*k+1] = -1, -1
			continue
		}
		m.idx[2*k], m.idx[2*k+1] = toU16(g.RuneIndex), toU16(g.RuneIndex+g.RuneLength)
		m.text[k] = text(g.RuneIndex, g.RuneLength)
	}
	return m, nil
}

var _ = os.Stderr

// hybridLRE is the candidate design: the RE2 fast path when the translator and
// the runtime checks allow it, QuickJS libregexp otherwise.
type hybridLRE struct {
	re2 *nativeEngine
	lre *lreEngine
}

func (h *hybridLRE) useRE2(p, f, s string) (*compiled, bool) {
	c := h.re2.compile(p, f)
	if c.re2 == nil {
		return c, false
	}
	if c.plan.astralUnsafe && strings.IndexFunc(s, func(r rune) bool { return r > 0xFFFF }) >= 0 {
		return c, false
	}
	if c.plan.lineTermMode && strings.ContainsAny(s, "\r\u2028\u2029") {
		return c, false
	}
	return c, true
}

func (h *hybridLRE) Run(p, f, s string) string {
	if _, ok := h.useRE2(p, f, s); ok {
		return h.re2.Run(p, f, s)
	}
	return h.lre.Run(p, f, s)
}

func (h *hybridLRE) Close() { h.lre.Close() }
