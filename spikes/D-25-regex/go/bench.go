package main

import (
	"encoding/json"
	"unicode/utf16"
	"unsafe"

	"fmt"
	"modernc.org/libc"
	"os"
	"os/exec"
	"sort"
	"strings"
	"time"
)

type evaler interface {
	Eval(code string) (string, error)
}

type benchPat struct {
	P string `json:"p"`
	F string `json:"f"`
}

type benchOut struct {
	Engine    string             `json:"engine"`
	CompileMs float64            `json:"compileMs"` // all corpus patterns, compile + one test
	Compiled  int                `json:"compiled"`
	PerPat    map[string]float64 `json:"perPatternUs"` // median µs per full global match pass over the text
	PerLine   map[string]float64 `json:"perLineUs"`    // median µs for one non-global exec on each line of the text
}

// bench measures compile time over every corpus pattern and global-match time
// over a fixed text for a sample of patterns. casesPath is bench-patterns.json;
// compile-list.json sits next to it.
func bench(engine, casesPath, harness, textPath string, runs int, out string) {
	var pats []benchPat
	b, err := os.ReadFile(casesPath)
	check(err)
	check(json.Unmarshal(b, &pats))
	var clist [][2]string
	b, err = os.ReadFile(strings.Replace(casesPath, "bench-patterns.json", "compile-list.json", 1))
	check(err)
	check(json.Unmarshal(b, &clist))
	tb, err := os.ReadFile(textPath)
	check(err)
	text := string(tb)
	h := readHarness(harness)
	res := benchOut{Engine: engine, PerPat: map[string]float64{}, PerLine: map[string]float64{}}
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")

	var compileTimes []float64
	for r := 0; r < runs; r++ {
		e, err := newEngine(engine, h)
		check(err)
		t0 := time.Now()
		n := 0
		if ev, ok := e.(evaler); ok {
			js, _ := json.Marshal(clist)
			v, err := ev.Eval("__compileAll(" + string(js) + ")")
			check(err)
			fmt.Sscan(v, &n)
		} else {
			for _, c := range clist {
				switch ee := e.(type) {
				case *nativeEngine:
					if cc := ee.compile(c[0], c[1]); cc.err == "" {
						n++
					}
				case *lreEngine:
					if cc := ee.compile(c[0], c[1]); cc.err == "" {
						n++
					}
				case *hybridLRE:
					if cc := ee.re2.compile(c[0], c[1]); cc.re2 != nil {
						n++
					} else if cc := ee.lre.compile(c[0], c[1]); cc.err == "" {
						n++
					}
				}
			}
		}
		compileTimes = append(compileTimes, float64(time.Since(t0).Microseconds())/1000)
		res.Compiled = n
		e.Close()
	}
	res.CompileMs = median(compileTimes)

	e, err := newEngine(engine, h)
	check(err)
	defer e.Close()
	samples := map[int][]float64{}
	for r := 0; r < runs; r++ {
		for i, p := range pats {
			var us float64
			const n = 3
			if ev, ok := e.(evaler); ok {
				t0 := time.Now()
				if _, err := ev.Eval(jsCall("__benchMatch", p.P, p.F, text, n)); err != nil {
					continue
				}
				us = float64(time.Since(t0).Nanoseconds()) / 1000 / n
			} else if le, ok := e.(*lreEngine); ok {
				us = benchLRE(le, p, text, n)
				if us < 0 {
					continue
				}
			} else if he, ok := e.(*hybridLRE); ok {
				if c, ok := he.useRE2(p.P, p.F, text); ok {
					in := newInput(text, c.u)
					t0 := time.Now()
					for k := 0; k < n; k++ {
						countMatches(he.re2, c, in)
					}
					us = float64(time.Since(t0).Nanoseconds()) / 1000 / n
				} else if us = benchLRE(he.lre, p, text, n); us < 0 {
					continue
				}
			} else {
				ne := e.(*nativeEngine)
				c := ne.compile(p.P, p.F)
				if c.err != "" {
					continue
				}
				in := newInput(text, c.u)
				if ne.kind == "re2" && (c.re2 == nil || (c.plan.astralUnsafe && in.hasAstral) || (c.plan.lineTermMode && strings.ContainsAny(text, "\r  "))) {
					continue
				}
				t0 := time.Now()
				for k := 0; k < n; k++ {
					if countMatches(ne, c, in) < 0 {
						break
					}
				}
				us = float64(time.Since(t0).Nanoseconds()) / 1000 / n
			}
			samples[i] = append(samples[i], us)
		}
	}
	for i, xs := range samples {
		if len(xs) == runs {
			res.PerPat[fmt.Sprint(i)] = median(xs)
		}
	}
	lineSamples := map[int][]float64{}
	for r := 0; r < runs; r++ {
		for i, p := range pats {
			f := strings.ReplaceAll(p.F, "g", "")
			if us, ok := benchLines(e, p.P, f, lines); ok {
				lineSamples[i] = append(lineSamples[i], us)
			}
		}
	}
	for i, xs := range lineSamples {
		if len(xs) == runs {
			res.PerLine[fmt.Sprint(i)] = median(xs)
		}
	}
	ob, _ := json.Marshal(res)
	check(os.WriteFile(out, ob, 0o644))
	fmt.Fprintf(os.Stderr, "%s: compile %.0fms (%d ok), %d patterns timed\n", engine, res.CompileMs, res.Compiled, len(res.PerPat))
}

// countMatches emulates s.match(re) for a global regex.
func countMatches(ne *nativeEngine, c *compiled, in *input) int {
	useRE2 := c.re2 != nil && !(c.plan.astralUnsafe && in.hasAstral) && !(c.plan.lineTermMode && strings.ContainsAny(in.s, "\r  "))
	last, n := 0, 0
	for last <= in.len16 && n < 100000 {
		var m *match
		var err error
		if useRE2 {
			m = in.execRE2(c.re2, last)
		} else {
			m, err = in.execRegexp2(c, last)
			if err != nil {
				return -1
			}
		}
		if m == nil {
			break
		}
		n++
		last = m.idx[1]
		if m.idx[0] == m.idx[1] {
			last = in.advance(last, c.u)
		}
	}
	return n
}

func median(xs []float64) float64 {
	if len(xs) == 0 {
		return 0
	}
	s := append([]float64(nil), xs...)
	sort.Float64s(s)
	return s[len(s)/2]
}

// ---- catastrophic backtracking ----

var redosCases = []struct{ name, p, f, s string }{
	{"nested quantifier", `^(a+)+$`, "", strings.Repeat("a", 30) + "!"},
	{"overlapping alternation", `^(a|aa)+$`, "", strings.Repeat("a", 40) + "!"},
	{"ambiguous adjacent", `(x+x+)+y`, "", strings.Repeat("x", 30)},
	{"email-style (real CVE shape)", `^([a-zA-Z0-9])(([\-.]|[_]+)?([a-zA-Z0-9]+))*(@){1}[a-z0-9]+[.]{1}(([a-z]{2,3})|([a-z]{2,3}[.]{1}[a-z]{2,3}))$`, "", strings.Repeat("a", 32) + "!"},
}

func redos(engine, harness string) {
	if engine != "" && os.Getenv("D25_REDOS_CHILD") != "" {
		var i int
		fmt.Sscan(os.Getenv("D25_REDOS_CHILD"), &i)
		c := redosCases[i]
		e, err := newEngine(engine, readHarness(harness))
		check(err)
		t0 := time.Now()
		r := e.Run(c.p, c.f, c.s)
		fmt.Printf("%.0f %s\n", float64(time.Since(t0).Milliseconds()), r)
		return
	}
	for i, c := range redosCases {
		cmd := exec.Command(os.Args[0], "redos", "-engine", engine, "-harness", harness)
		cmd.Env = append(os.Environ(), fmt.Sprintf("D25_REDOS_CHILD=%d", i))
		done := make(chan []byte, 1)
		go func() { b, _ := cmd.Output(); done <- b }()
		select {
		case b := <-done:
			var ms float64
			var r string
			fmt.Sscan(string(b), &ms, &r)
			verdict := "finished"
			if strings.Contains(string(b), `"to":true`) {
				verdict = "aborted by timeout"
			} else if strings.Contains(string(b), "ce") {
				verdict = "rejected"
			}
			fmt.Printf("%s\t%s\t%.0fms\t%s\n", engine, c.name, ms, verdict)
		case <-time.After(15 * time.Second):
			cmd.Process.Kill()
			fmt.Printf("%s\t%s\t>15000ms\thung (killed; no timeout honoured)\n", engine, c.name)
		}
	}
}

func benchLRE(le *lreEngine, p benchPat, text string, n int) float64 {
	c := le.compile(p.P, p.F)
	if c.err != "" {
		return -1
	}
	units := utf16.Encode([]rune(text))
	buf := libc.Xmalloc(le.tls, libc.Tsize_t(2*len(units)+2))
	defer libc.Xfree(le.tls, buf)
	copy(unsafe.Slice((*uint16)(unsafe.Pointer(buf)), len(units)), units)
	t0 := time.Now()
	for k := 0; k < n; k++ {
		if le.countMatches(c, units, buf) < 0 {
			return -1
		}
	}
	return float64(time.Since(t0).Nanoseconds()) / 1000 / float64(n)
}

// benchLines times one non-global exec on every line; it returns µs per line.
func benchLines(e Engine, p, f string, lines []string) (float64, bool) {
	const n = 3
	per := func(d time.Duration) float64 { return float64(d.Nanoseconds()) / 1000 / float64(n*len(lines)) }
	switch ee := e.(type) {
	case evaler:
		js, _ := json.Marshal(lines)
		code := "__benchLines(" + mustJSON(p) + "," + mustJSON(f) + "," + string(js) + "," + fmt.Sprint(n) + ")"
		t0 := time.Now()
		if _, err := ee.Eval(code); err != nil {
			return 0, false
		}
		return per(time.Since(t0)), true
	case *lreEngine:
		c := ee.compile(p, f)
		if c.err != "" {
			return 0, false
		}
		bufs, lens := lreBuffers(ee, lines)
		defer func() {
			for _, b := range bufs {
				libc.Xfree(ee.tls, b)
			}
		}()
		t0 := time.Now()
		for k := 0; k < n; k++ {
			for j := range bufs {
				ee.exec(c, bufs[j], lens[j], 0)
			}
		}
		return per(time.Since(t0)), true
	case *hybridLRE:
		c := ee.re2.compile(p, f)
		lc := ee.lre.compile(p, f)
		bufs, lens := lreBuffers(ee.lre, lines)
		defer func() {
			for _, b := range bufs {
				libc.Xfree(ee.lre.tls, b)
			}
		}()
		ins := make([]*input, len(lines))
		for j, l := range lines {
			ins[j] = newInput(l, c.u)
		}
		t0 := time.Now()
		for k := 0; k < n; k++ {
			for j, l := range lines {
				if _, ok := ee.useRE2(p, f, l); ok {
					ins[j].execRE2(c.re2, 0)
				} else if lc.err == "" {
					ee.lre.exec(lc, bufs[j], lens[j], 0)
				}
			}
		}
		return per(time.Since(t0)), true
	case *nativeEngine:
		c := ee.compile(p, f)
		if c.err != "" {
			return 0, false
		}
		ins := make([]*input, len(lines))
		for j, l := range lines {
			ins[j] = newInput(l, c.u)
		}
		if ee.kind == "re2" && c.re2 == nil {
			return 0, false
		}
		t0 := time.Now()
		for k := 0; k < n; k++ {
			for _, in := range ins {
				if c.re2 != nil && !(c.plan.astralUnsafe && in.hasAstral) && !(c.plan.lineTermMode && strings.ContainsAny(in.s, "\r\u2028\u2029")) {
					in.execRE2(c.re2, 0)
				} else if ee.kind != "re2" {
					in.execRegexp2(c, 0)
				}
			}
		}
		return per(time.Since(t0)), true
	}
	return 0, false
}

func lreBuffers(e *lreEngine, lines []string) ([]uintptr, []int) {
	bufs := make([]uintptr, len(lines))
	lens := make([]int, len(lines))
	for j, l := range lines {
		u := utf16.Encode([]rune(l))
		bufs[j] = libc.Xmalloc(e.tls, libc.Tsize_t(2*len(u)+2))
		copy(unsafe.Slice((*uint16)(unsafe.Pointer(bufs[j])), len(u)), u)
		lens[j] = len(u)
	}
	return bufs, lens
}
