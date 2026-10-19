package main

import (
	"os"
	"strings"
	"testing"
	"time"
)

// TestQuick prints one-render timings and the streaming check per engine.
// DIST=<dist dir> go test -run TestQuick -v .
func TestQuick(t *testing.T) {
	dist := os.Getenv("DIST")
	if dist == "" {
		t.Skip("DIST not set")
	}
	bundle := os.Getenv("BUNDLE")
	if bundle == "" {
		bundle = "ssr.js"
	}
	names := []string{"modernc", "qjs-wazero", "goja"}
	if s := os.Getenv("ENGINES"); s != "" {
		names = strings.Fields(s)
	}
	for _, name := range names {
		f, err := newFactory(name, dist+"/"+bundle, 800)
		if err != nil {
			t.Fatal(err)
		}
		t0 := time.Now()
		e, err := f()
		if err != nil {
			t.Fatal(name, err)
		}
		load := time.Since(t0)
		var times []time.Duration
		for i := 0; i < 3; i++ {
			t1 := time.Now()
			if _, err := e.Render(); err != nil {
				t.Fatal(name, err)
			}
			times = append(times, time.Since(t1))
		}
		t.Logf("%s: load %v renders %v", name, load, times)
		h, _ := e.Render()
		sf, _ := newFactory(name, dist+"/ssr-stream.js", 800)
		se, err := sf()
		if err != nil {
			t.Logf("%s: stream load error %v", name, err)
			continue
		}
		t1 := time.Now()
		sh, chunks, err := runStream(se)
		t.Logf("%s: stream identical=%v chunks=%d err=%v in %v", name, sh == h, chunks, err, time.Since(t1))
	}
}
