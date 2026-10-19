package main

import (
	"fmt"
	"os"
	"sort"
	"strings"

	gojaparser "github.com/dop251/goja/parser"

	"nisse-spike/d25/re2at"
)

// re2cov reports how many corpus patterns the RE2 fast path accepts, why the
// rest fall back, and how goja's own RE2 translation rule compares.
func re2cov(casesPath, out string) {
	cs := loadCases(casesPath)
	reasons := map[string]int{}
	var npm, ok, gojaOK, both, inputs, fastInputs int
	for _, c := range cs {
		if !strings.HasPrefix(c.Src, "npm:") {
			continue
		}
		npm++
		plan := translateRE2(c.P, c.F)
		if plan.ok {
			if _, err := compileRE2(plan.goPattern); err != nil {
				plan.ok, plan.reason = false, "go compile error"
			}
		}
		_, gerr := gojaparser.TransformRegExp(c.P, strings.Contains(c.F, "s"), strings.Contains(c.F, "u"))
		if gerr == nil {
			gojaOK++
		}
		if plan.ok {
			ok++
			if gerr == nil {
				both++
			}
			for _, s := range c.In {
				inputs++
				astral := strings.IndexFunc(s, func(r rune) bool { return r > 0xFFFF }) >= 0
				lt := strings.ContainsAny(s, "\r  ")
				if !(plan.astralUnsafe && astral) && !(plan.lineTermMode && lt) {
					fastInputs++
				}
			}
		} else {
			r := plan.reason
			if i := strings.Index(r, ":"); i > 0 {
				r = r[:i]
			}
			reasons[r]++
		}
	}
	fh, err := os.Create(out)
	check(err)
	defer fh.Close()
	pc := func(a, b int) string { return fmt.Sprintf("%d / %d (%.1f%%)", a, b, 100*float64(a)/float64(max(b, 1))) }
	fmt.Fprintf(fh, "| Measure | Value |\n| --- | --- |\n")
	fmt.Fprintf(fh, "| npm patterns accepted by the Nisse RE2 rule | %s |\n", pc(ok, npm))
	fmt.Fprintf(fh, "| npm patterns accepted by goja's RE2 translation | %s |\n", pc(gojaOK, npm))
	fmt.Fprintf(fh, "| accepted by both | %s |\n", pc(both, npm))
	fmt.Fprintf(fh, "| inputs to accepted patterns that pass the runtime checks | %s |\n\n", pc(fastInputs, inputs))
	type kv struct {
		k string
		v int
	}
	var kvs []kv
	for k, v := range reasons {
		kvs = append(kvs, kv{k, v})
	}
	sort.Slice(kvs, func(i, j int) bool { return kvs[i].v > kvs[j].v })
	fmt.Fprintf(fh, "| Why a pattern falls back to the backtracking engine | Patterns |\n| --- | --- |\n")
	for _, x := range kvs {
		fmt.Fprintf(fh, "| %s | %d |\n", x.k, x.v)
	}
}

func compileRE2(p string) (any, error) { return re2at.Compile(p) }
