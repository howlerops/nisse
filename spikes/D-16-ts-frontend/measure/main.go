// Command measure runs a command and prints its wall time and the peak resident memory
// of its whole process tree (sampled from /proc every 20 ms), as one JSON line on stderr.
// Usage: measure <label> <cmd> [args...]
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// treeRSS sums VmRSS (kB) over pid and all its descendants.
func treeRSS(root int) int {
	children := map[int][]int{}
	rss := map[int]int{}
	entries, _ := os.ReadDir("/proc")
	for _, e := range entries {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		b, err := os.ReadFile("/proc/" + e.Name() + "/status")
		if err != nil {
			continue
		}
		for _, line := range strings.Split(string(b), "\n") {
			f := strings.Fields(line)
			if len(f) < 2 {
				continue
			}
			switch f[0] {
			case "PPid:":
				ppid, _ := strconv.Atoi(f[1])
				children[ppid] = append(children[ppid], pid)
			case "VmRSS:":
				rss[pid], _ = strconv.Atoi(f[1])
			}
		}
	}
	total := 0
	stack := []int{root}
	for len(stack) > 0 {
		p := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		total += rss[p]
		stack = append(stack, children[p]...)
	}
	return total
}

func main() {
	cmd := exec.Command(os.Args[2], os.Args[3:]...)
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	start := time.Now()
	if err := cmd.Start(); err != nil {
		panic(err)
	}
	done := make(chan error)
	go func() { done <- cmd.Wait() }()
	peak := 0
	tick := time.NewTicker(20 * time.Millisecond)
	var err error
loop:
	for {
		select {
		case err = <-done:
			break loop
		case <-tick.C:
			if r := treeRSS(cmd.Process.Pid); r > peak {
				peak = r
			}
		}
	}
	out, _ := json.Marshal(map[string]any{"label": os.Args[1], "wallMs": time.Since(start).Milliseconds(), "peakTreeRssMB": peak / 1024, "ok": err == nil})
	fmt.Fprintln(os.Stderr, "MEASURE "+string(out))
}
