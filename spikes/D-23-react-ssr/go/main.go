// Command bench measures React SSR in embedded JS engines and a Go-native baseline.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"runtime"
	"runtime/debug"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

type result struct {
	Engine           string  `json:"engine"`
	Bundle           string  `json:"bundle"`
	Rows             int     `json:"rows"`
	HTMLBytes        int     `json:"htmlBytes"`
	RendersPerSec    float64 `json:"rendersPerSec"`
	P50ms            float64 `json:"p50ms"`
	P99ms            float64 `json:"p99ms"`
	StartupMs        float64 `json:"startupMs"`
	MemPerInstanceMB float64 `json:"memPerInstanceMB"`
	Pool4PerSec      float64 `json:"pool4RendersPerSec"`
	Stream           string  `json:"stream"`
}

func main() {
	engine := flag.String("engine", "goja", "goja | qjs-wazero | modernc | native")
	dist := flag.String("dist", "", "directory with ssr.js and ssr-stream.js")
	dur := flag.Duration("dur", 10*time.Second, "measurement time per phase")
	dump := flag.String("dump", "", "write one render's HTML here")
	rows := flag.Int("rows", 800, "table rows: 800 (~220 KB page) or 100 (~28 KB page)")
	bundle := flag.String("bundle", "ssr.js", "bundle in dist to render: ssr.js (stock) or ssr-join.js (join patch)")
	poolOnly := flag.Int("pool-only", 0, "if > 0, only measure a pool of this many instances (scaling check)")
	flag.Parse()
	if *poolOnly > 0 {
		f, err := newFactory(*engine, *dist+"/"+*bundle, *rows)
		check(err)
		fmt.Printf("{\"engine\":%q,\"bundle\":%q,\"rows\":%d,\"pool\":%d,\"rendersPerSec\":%.1f}\n", *engine, *bundle, *rows, *poolOnly, pool(f, *poolOnly, *dur))
		return
	}

	f, err := newFactory(*engine, *dist+"/"+*bundle, *rows)
	check(err)
	sf, err := newFactory(*engine, *dist+"/ssr-stream.js", *rows)
	check(err)
	res := result{Engine: *engine, Bundle: *bundle, Rows: *rows}

	// Startup: new instance including bundle evaluation, median of 5.
	var starts []float64
	for i := 0; i < 5; i++ {
		t := time.Now()
		e, err := f()
		check(err)
		starts = append(starts, ms(time.Since(t)))
		e.Close()
	}
	sort.Float64s(starts)
	res.StartupMs = starts[2]

	// Memory: RSS growth over 4 live instances after one render each.
	runtime.GC()
	debug.FreeOSMemory()
	rss0 := rss()
	var keep []Engine
	for i := 0; i < 4; i++ {
		e, err := f()
		check(err)
		_, err = e.Render()
		check(err)
		keep = append(keep, e)
	}
	runtime.GC()
	res.MemPerInstanceMB = float64(rss()-rss0) / 4 / (1 << 20)
	for _, e := range keep {
		e.Close()
	}
	keep = nil

	// Single-threaded throughput and latency.
	e, err := f()
	check(err)
	html, err := e.Render()
	check(err)
	res.HTMLBytes = len(html)
	if *dump != "" {
		check(os.WriteFile(*dump, []byte(html), 0o644))
	}
	warm := time.Now().Add(*dur / 5)
	for time.Now().Before(warm) {
		_, err = e.Render()
		check(err)
	}
	lat := loop(e, *dur)
	sort.Float64s(lat)
	res.RendersPerSec = float64(len(lat)) / dur.Seconds()
	res.P50ms = lat[len(lat)/2]
	res.P99ms = lat[min(len(lat)-1, len(lat)*99/100)]
	e.Close()

	// Streaming: renderToReadableStream to completion, compared with renderToString.
	if *engine == "native" {
		res.Stream = "n/a"
	} else if se, err := sf(); err != nil {
		res.Stream = "load error: " + firstLine(err)
	} else {
		shtml, chunks, err := runStream(se)
		switch {
		case err != nil:
			res.Stream = "error: " + firstLine(err)
		case shtml != html:
			res.Stream = fmt.Sprintf("mismatch (%d chunks, %d bytes)", chunks, len(shtml))
		default:
			res.Stream = fmt.Sprintf("ok (%d chunks)", chunks)
		}
		se.Close()
	}

	// Pool: 4 goroutines, one instance each.
	res.Pool4PerSec = pool(f, 4, *dur)

	out, _ := json.Marshal(res)
	fmt.Println(string(out))
}

// pool runs n instances on n goroutines and returns total renders per second.
func pool(f factory, n int, d time.Duration) float64 {
	var wg sync.WaitGroup
	counts := make([]int, n)
	for i := range counts {
		e, err := f()
		check(err)
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer e.Close()
			loop(e, d/5) // warm-up
			counts[i] = len(loop(e, d))
		}()
	}
	wg.Wait()
	total := 0
	for _, c := range counts {
		total += c
	}
	return float64(total) / d.Seconds()
}

func loop(e Engine, d time.Duration) []float64 {
	var lat []float64
	end := time.Now().Add(d)
	for time.Now().Before(end) {
		t := time.Now()
		if _, err := e.Render(); err != nil {
			check(err)
		}
		lat = append(lat, ms(time.Since(t)))
	}
	return lat
}

func rss() int64 {
	b, err := os.ReadFile("/proc/self/statm")
	check(err)
	f := strings.Fields(string(b))
	pages, _ := strconv.ParseInt(f[1], 10, 64)
	return pages * int64(os.Getpagesize())
}

func ms(d time.Duration) float64 { return float64(d.Microseconds()) / 1000 }

func firstLine(err error) string { s, _, _ := strings.Cut(err.Error(), "\n"); return s }

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}
