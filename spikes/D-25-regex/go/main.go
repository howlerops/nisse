// Command d25 is the D-25 regex spike harness: it runs a corpus of JavaScript
// regex cases through candidate engines and diffs them against Node's results.
package main

import (
	"bufio"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"reflect"
	"sort"
	"strings"
	"time"
)

type Case struct {
	Src string   `json:"src"`
	P   string   `json:"p"`
	F   string   `json:"f"`
	In  []string `json:"in"`
}

func loadCases(path string) []Case {
	var cs []Case
	b, err := os.ReadFile(path)
	check(err)
	check(json.Unmarshal(b, &cs))
	return cs
}

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: d25 worker|drive|diff|bench|redos ...")
		os.Exit(2)
	}
	cmd, args := os.Args[1], os.Args[2:]
	fs := flag.NewFlagSet(cmd, flag.ExitOnError)
	engine := fs.String("engine", "", "engine name")
	cases := fs.String("cases", "", "cases.json")
	harness := fs.String("harness", "../harness.js", "harness.js path")
	from := fs.Int("from", 0, "flattened case index to start at")
	out := fs.String("out", "", "output file")
	truth := fs.String("truth", "", "Node results")
	text := fs.String("text", "", "bench text file")
	runs := fs.Int("runs", 5, "bench runs")
	fs.Parse(args)
	switch cmd {
	case "worker":
		worker(*engine, *cases, *harness, *from)
	case "drive":
		drive(*engine, *cases, *harness, *out)
	case "diff":
		diff(*cases, *truth, fs.Args(), *out)
	case "bench":
		bench(*engine, *cases, *harness, *text, *runs, *out)
	case "redos":
		redos(*engine, *harness)
	default:
		fmt.Fprintln(os.Stderr, "unknown command", cmd)
		os.Exit(2)
	}
}

func readHarness(path string) string {
	b, err := os.ReadFile(path)
	check(err)
	return string(b)
}

// worker prints one result line per flattened case, starting at from.
func worker(engine, casesPath, harness string, from int) {
	cs := loadCases(casesPath)
	e, err := newEngine(engine, readHarness(harness))
	check(err)
	w := bufio.NewWriter(os.Stdout)
	k := 0
	for _, c := range cs {
		for _, s := range c.In {
			if k >= from {
				r := e.Run(c.P, c.F, s)
				fmt.Fprintln(w, strings.ReplaceAll(r, "\n", `\n`))
				w.Flush()
			}
			k++
		}
	}
}

// drive runs a worker subprocess and restarts it past any case that hangs.
func drive(engine, casesPath, harness, out string) {
	cs := loadCases(casesPath)
	total := 0
	for _, c := range cs {
		total += len(c.In)
	}
	flat := make([]string, 0, total)
	t0 := time.Now()
	hangs := 0
	for len(flat) < total {
		cmd := exec.Command(os.Args[0], "worker", "-engine", engine, "-cases", casesPath, "-harness", harness, "-from", fmt.Sprint(len(flat)))
		stdout, _ := cmd.StdoutPipe()
		cmd.Stderr = os.Stderr
		check(cmd.Start())
		lines := make(chan string)
		go func() {
			sc := bufio.NewScanner(stdout)
			sc.Buffer(make([]byte, 1<<20), 1<<26)
			for sc.Scan() {
				lines <- sc.Text()
			}
			close(lines)
		}()
		dead := false
		for !dead && len(flat) < total {
			select {
			case l, ok := <-lines:
				if !ok {
					// worker crashed: record the case it died on and restart
					flat = append(flat, `{"crash":true}`)
					dead = true
					break
				}
				flat = append(flat, l)
			case <-time.After(5 * time.Second):
				cmd.Process.Kill()
				flat = append(flat, `{"to":true,"hang":true}`)
				hangs++
				dead = true
			}
		}
		cmd.Process.Kill()
		cmd.Wait()
		go func() {
			for range lines {
			}
		}()
	}
	res := make([][]string, len(cs))
	k := 0
	for i, c := range cs {
		res[i] = flat[k : k+len(c.In)]
		k += len(c.In)
	}
	b, _ := json.Marshal(res)
	check(os.WriteFile(out, b, 0o644))
	fmt.Fprintf(os.Stderr, "%s: %d cases in %s, hard hangs=%d\n", engine, total, time.Since(t0).Round(time.Millisecond), hangs)
}

// ---- diff ----

type tally struct{ pass, fail, to, ce, na int }

func (t *tally) add(o tally) {
	t.pass += o.pass
	t.fail += o.fail
	t.to += o.to
	t.ce += o.ce
	t.na += o.na
}

func classify(engine, truthJSON, gotJSON string) string {
	var tv, gv map[string]any
	json.Unmarshal([]byte(truthJSON), &tv)
	if err := json.Unmarshal([]byte(gotJSON), &gv); err != nil {
		return "fail"
	}
	if _, ok := tv["to"]; ok {
		return "skip" // Node itself timed out
	}
	if ce, ok := gv["ce"]; ok {
		if s, _ := ce.(string); strings.HasPrefix(s, "ineligible") || strings.HasPrefix(s, "runtime fallback") {
			return "na"
		}
		return "ce"
	}
	if _, ok := gv["to"]; ok {
		return "to"
	}
	if _, ok := gv["err"]; ok {
		return "fail"
	}
	native := engine == "regexp2v1" || engine == "regexp2v2" || engine == "re2fast" || engine == "hybrid"
	if native {
		if reflect.DeepEqual(tv["m"], gv["m"]) {
			return "pass"
		}
		return "fail"
	}
	if reflect.DeepEqual(tv, gv) {
		return "pass"
	}
	return "fail"
}

func diff(casesPath, truthPath string, engines []string, out string) {
	cs := loadCases(casesPath)
	var truth [][]string
	b, err := os.ReadFile(truthPath)
	check(err)
	check(json.Unmarshal(b, &truth))
	type row struct {
		engine string
		byFeat map[string]*tally
		pats   map[string][2]int // feature -> [patterns all-pass, patterns]
		total  tally
		fails  []string
	}
	var rows []*row
	for _, spec := range engines {
		name, path, _ := strings.Cut(spec, "=")
		var got [][]string
		b, err := os.ReadFile(path)
		check(err)
		check(json.Unmarshal(b, &got))
		r := &row{engine: name, byFeat: map[string]*tally{}, pats: map[string][2]int{}}
		for i, c := range cs {
			var feats []string
			if strings.HasPrefix(c.Src, "probe:") {
				feats = []string{"probe/" + strings.TrimPrefix(c.Src, "probe:")}
			} else {
				feats = []string{"npm (all)"}
				for _, f := range features(c.P, c.F) {
					feats = append(feats, "npm/"+f)
				}
			}
			var pt tally
			for j := range c.In {
				switch classify(name, truth[i][j], got[i][j]) {
				case "pass":
					pt.pass++
				case "fail":
					pt.fail++
					if len(r.fails) < 400 {
						r.fails = append(r.fails, fmt.Sprintf("%s /%s/%s input=%q\n  node: %s\n  got:  %s", c.Src, c.P, c.F, c.In[j], truth[i][j], got[i][j]))
					}
				case "to":
					pt.to++
				case "ce":
					pt.ce++
				case "na":
					pt.na++
				}
			}
			for _, f := range feats {
				if r.byFeat[f] == nil {
					r.byFeat[f] = &tally{}
				}
				r.byFeat[f].add(pt)
				pp := r.pats[f]
				applicable := pt.pass + pt.fail + pt.to + pt.ce
				if applicable > 0 {
					pp[1]++
					if pt.fail+pt.to+pt.ce == 0 {
						pp[0]++
					}
				}
				r.pats[f] = pp
			}
			r.total.add(pt)
		}
		rows = append(rows, r)
	}
	// feature order
	featSet := map[string]bool{}
	for _, r := range rows {
		for f := range r.byFeat {
			featSet[f] = true
		}
	}
	var featList []string
	for f := range featSet {
		featList = append(featList, f)
	}
	sort.Strings(featList)
	var w io.Writer = os.Stdout
	if out != "" {
		fh, err := os.Create(out)
		check(err)
		defer fh.Close()
		w = io.MultiWriter(os.Stdout, fh)
	}
	fmt.Fprintf(w, "| Feature (cases) |")
	for _, r := range rows {
		fmt.Fprintf(w, " %s |", r.engine)
	}
	fmt.Fprintf(w, "\n| --- |%s\n", strings.Repeat(" --- |", len(rows)))
	pct := func(t tally) string {
		app := t.pass + t.fail + t.to + t.ce
		if app == 0 {
			return "n/a"
		}
		s := fmt.Sprintf("%.1f%%", 100*float64(t.pass)/float64(app))
		if t.na > 0 {
			s += fmt.Sprintf(" (covers %.0f%%)", 100*float64(app)/float64(app+t.na))
		}
		return s
	}
	for _, f := range featList {
		n := 0
		for _, r := range rows {
			if t := r.byFeat[f]; t != nil {
				n = t.pass + t.fail + t.to + t.ce + t.na
			}
		}
		fmt.Fprintf(w, "| %s (%d) |", f, n)
		for _, r := range rows {
			if t := r.byFeat[f]; t != nil {
				fmt.Fprintf(w, " %s |", pct(*t))
			} else {
				fmt.Fprintf(w, " — |")
			}
		}
		fmt.Fprintln(w)
	}
	fmt.Fprintf(w, "| **All cases** |")
	for _, r := range rows {
		fmt.Fprintf(w, " **%s** |", pct(r.total))
	}
	fmt.Fprintf(w, "\n| Patterns fully correct (npm) |")
	for _, r := range rows {
		pp := r.pats["npm (all)"]
		fmt.Fprintf(w, " %d / %d (%.1f%%) |", pp[0], pp[1], 100*float64(pp[0])/float64(max(pp[1], 1)))
	}
	fmt.Fprintf(w, "\n| Timeouts / compile errors |")
	for _, r := range rows {
		fmt.Fprintf(w, " %d / %d |", r.total.to, r.total.ce)
	}
	fmt.Fprintln(w)
	if out != "" {
		for _, r := range rows {
			fh, _ := os.Create(strings.TrimSuffix(out, ".md") + "-fails-" + r.engine + ".txt")
			fmt.Fprintln(fh, strings.Join(r.fails, "\n"))
			fh.Close()
		}
	}
}
