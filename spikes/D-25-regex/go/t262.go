package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"

	"github.com/dop251/goja"
	"github.com/fastschema/qjs"
	"modernc.org/quickjs"
)

type t262Meta struct {
	includes []string
	flags    []string
	features []string
	negType  string
}

var (
	reFront = regexp.MustCompile(`(?s)/\*---(.*?)---\*/`)
	reList  = func(key string) *regexp.Regexp {
		return regexp.MustCompile(`(?m)^` + key + `:\s*\[([^\]]*)\]`)
	}
	reListBlock = func(key string) *regexp.Regexp {
		return regexp.MustCompile(`(?m)^` + key + `:\s*\n((?:\s+-\s+.*\n)+)`)
	}
	reNegType = regexp.MustCompile(`(?m)^negative:\s*\n(?:\s+.*\n)*?\s+type:\s*(\w+)`)
)

func parseList(front, key string) []string {
	var out []string
	if m := reList(key).FindStringSubmatch(front); m != nil {
		for _, x := range strings.Split(m[1], ",") {
			if x = strings.TrimSpace(x); x != "" {
				out = append(out, x)
			}
		}
	} else if m := reListBlock(key).FindStringSubmatch(front); m != nil {
		for _, l := range strings.Split(m[1], "\n") {
			if x := strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(l), "-")); x != "" {
				out = append(out, x)
			}
		}
	}
	return out
}

func parseMeta(src string) t262Meta {
	var m t262Meta
	f := reFront.FindStringSubmatch(src)
	if f == nil {
		return m
	}
	front := f[1] + "\n"
	m.includes = parseList(front, "includes")
	m.flags = parseList(front, "flags")
	m.features = parseList(front, "features")
	if n := reNegType.FindStringSubmatch(front); n != nil {
		m.negType = n[1]
	}
	return m
}

func t262Group(rel string, feats []string) string {
	for _, f := range feats {
		if strings.HasPrefix(f, "regexp-") {
			return f
		}
	}
	parts := strings.Split(filepath.ToSlash(rel), "/")
	// test/built-ins/RegExp/...
	if len(parts) > 3 && parts[2] == "RegExp" {
		if parts[3] == "prototype" && len(parts) > 4 {
			return "RegExp.prototype." + parts[4]
		}
		if strings.HasSuffix(parts[3], ".js") {
			return "RegExp (top-level)"
		}
		return "RegExp/" + parts[3]
	}
	if len(parts) > 4 && parts[2] == "String" {
		return "String.prototype." + parts[4]
	}
	return strings.Join(parts[1:3], "/") + "/" + parts[3]
}

// t262 runs the test262 RegExp-related tests through a JS engine (or prints the file list for Node).
func t262(engine, dir, out string) {
	var files []string
	filepath.Walk(filepath.Join(dir, "test"), func(p string, info os.FileInfo, err error) error {
		if err == nil && !info.IsDir() && strings.HasSuffix(p, ".js") && !strings.Contains(p, "_FIXTURE") {
			files = append(files, p)
		}
		return nil
	})
	sort.Strings(files)
	harnessCache := map[string]string{}
	readH := func(n string) string {
		if s, ok := harnessCache[n]; ok {
			return s
		}
		b, _ := os.ReadFile(filepath.Join(dir, "harness", n))
		harnessCache[n] = string(b)
		return string(b)
	}
	type res struct {
		Group  string `json:"group"`
		File   string `json:"file"`
		Result string `json:"result"` // pass, fail, skip
		Msg    string `json:"msg,omitempty"`
	}
	var results []res
	t0 := time.Now()
	for _, f := range files {
		b, _ := os.ReadFile(f)
		src := string(b)
		m := parseMeta(src)
		rel, _ := filepath.Rel(dir, f)
		g := t262Group(rel, m.features)
		skip := ""
		for _, fl := range m.flags {
			if fl == "module" || fl == "async" || fl == "raw" || fl == "CanBlockIsFalse" || fl == "CanBlockIsTrue" {
				skip = fl
			}
		}
		if skip != "" {
			results = append(results, res{g, rel, "skip", skip})
			continue
		}
		var sb strings.Builder
		for _, fl := range m.flags {
			if fl == "onlyStrict" {
				sb.WriteString("\"use strict\";\n")
			}
		}
		sb.WriteString(readH("assert.js") + "\n" + readH("sta.js") + "\n")
		for _, inc := range m.includes {
			sb.WriteString(readH(inc) + "\n")
		}
		sb.WriteString(src)
		errMsg := runT262(engine, sb.String())
		r := "pass"
		switch {
		case m.negType != "" && errMsg == "":
			r, errMsg = "fail", "expected "+m.negType
		case m.negType != "" && !strings.Contains(errMsg, m.negType):
			r = "fail"
		case m.negType == "" && errMsg != "":
			r = "fail"
		}
		if r == "pass" {
			errMsg = ""
		}
		if len(errMsg) > 160 {
			errMsg = errMsg[:160]
		}
		results = append(results, res{g, rel, r, errMsg})
	}
	ob, _ := json.Marshal(results)
	check(os.WriteFile(out, ob, 0o644))
	fmt.Fprintf(os.Stderr, "%s: %d test262 files in %s\n", engine, len(files), time.Since(t0).Round(time.Second))
}

// runT262 evaluates one test in a fresh engine and returns "" or the error text.
func runT262(engine, code string) (errMsg string) {
	defer func() {
		if r := recover(); r != nil {
			errMsg = fmt.Sprintf("Go panic: %v", r)
		}
	}()
	switch engine {
	case "goja":
		vm := goja.New()
		t := time.AfterFunc(10*time.Second, func() { vm.Interrupt("timeout") })
		defer t.Stop()
		vm.Set("print", func(goja.Value) {})
		if _, err := vm.RunString(code); err != nil {
			if ex, ok := err.(*goja.Exception); ok {
				if o := ex.Value().ToObject(vm); o != nil {
					return o.Get("name").String() + ": " + o.Get("message").String()
				}
			}
			return err.Error()
		}
	case "quickjs":
		vm, err := quickjs.NewVM()
		if err != nil {
			return err.Error()
		}
		defer vm.Close()
		vm.SetEvalTimeout(10 * time.Second)
		if _, err := vm.Eval("var print = function(){};\n"+code, quickjs.EvalGlobal); err != nil {
			return err.Error()
		}
	case "qjswasm":
		rt, err := qjs.New(qjs.Option{MaxExecutionTime: 10000})
		if err != nil {
			return err.Error()
		}
		defer rt.Close()
		v, err := rt.Context().Eval("t.js", qjs.Code("var print = function(){};\n"+code))
		if err != nil {
			return err.Error()
		}
		v.Free()
	default:
		return "unknown engine"
	}
	return ""
}

// t262Report prints a per-group pass-rate table from several engines' t262 outputs (name=path).
func t262Report(specs []string, out string) {
	type res struct{ Group, File, Result, Msg string }
	names := []string{}
	byEngine := map[string]map[string][2]int{}
	nodePass := map[string]bool{}
	groups := map[string]bool{}
	all := map[string][]res{}
	for _, sp := range specs {
		name, path, _ := strings.Cut(sp, "=")
		names = append(names, name)
		var rs []res
		b, err := os.ReadFile(path)
		check(err)
		check(json.Unmarshal(b, &rs))
		all[name] = rs
		if name == "node" {
			for _, r := range rs {
				if r.Result == "pass" {
					nodePass[r.File] = true
				}
			}
		}
	}
	for _, name := range names {
		m := map[string][2]int{}
		for _, r := range all[name] {
			if r.Result == "skip" {
				continue
			}
			r.Group = tidyGroup(r.Group)
			groups[r.Group] = true
			c := m[r.Group]
			c[1]++
			if r.Result == "pass" {
				c[0]++
			}
			m[r.Group] = c
			// "Node-passing subset": only tests Node 22 passes
			c2 := m["~ tests Node 22 passes"]
			if nodePass[r.File] {
				c2[1]++
				if r.Result == "pass" {
					c2[0]++
				}
			}
			m["~ tests Node 22 passes"] = c2
			t := m["~ all"]
			t[1]++
			if r.Result == "pass" {
				t[0]++
			}
			m["~ all"] = t
		}
		byEngine[name] = m
	}
	var gl []string
	for g := range groups {
		gl = append(gl, g)
	}
	sort.Strings(gl)
	gl = append(gl, "~ all", "~ tests Node 22 passes")
	fh, err := os.Create(out)
	check(err)
	defer fh.Close()
	fmt.Fprintf(fh, "| test262 group |")
	for _, n := range names {
		fmt.Fprintf(fh, " %s |", n)
	}
	fmt.Fprintf(fh, "\n| --- |%s\n", strings.Repeat(" --- |", len(names)))
	for _, g := range gl {
		fmt.Fprintf(fh, "| %s |", g)
		for _, n := range names {
			c := byEngine[n][g]
			if c[1] == 0 {
				fmt.Fprintf(fh, " — |")
			} else {
				fmt.Fprintf(fh, " %d/%d (%.0f%%) |", c[0], c[1], 100*float64(c[0])/float64(c[1]))
			}
		}
		fmt.Fprintln(fh)
	}
}

// tidyGroup folds per-file groups for loose top-level test files into one row.
func tidyGroup(g string) string {
	switch {
	case strings.HasPrefix(g, "built-ins/String/") || g == "String.prototype.undefined" || (strings.HasPrefix(g, "String.prototype.") && strings.HasSuffix(g, ".js")):
		return "String (top-level, not regex-specific)"
	case strings.HasPrefix(g, "RegExp.prototype.") && strings.HasSuffix(g, ".js"):
		return "RegExp.prototype (top-level)"
	}
	return g
}
