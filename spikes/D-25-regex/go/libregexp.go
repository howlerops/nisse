package main

import (
	"fmt"
	"strings"
	"sync/atomic"
	"time"
	"unicode/utf16"
	"unsafe"

	"modernc.org/libc"
	lib "modernc.org/libquickjs"
)

// lreEngine uses QuickJS's libregexp on its own, without the JS VM: the C code
// is transpiled to Go by modernc.org/libquickjs, so this stays cgo-free.
// A bare JSContext is still needed because libregexp allocates through it and
// polls it for interrupts.
type lreEngine struct {
	tls   *libc.TLS
	rt    uintptr
	ctx   uintptr
	intr  uintptr // *lreInterrupt in C memory
	cache map[string]*lreCompiled
}

type lreInterrupt struct {
	stop      int32
	deadlineN int64
}

type lreCompiled struct {
	bc     uintptr
	ncap   int
	nalloc int // capture-array slots libregexp needs (captures plus internal registers)
	groups []string
	err    string
	g, y   bool
	u, d   bool
}

func lreInterruptHandler(tls *libc.TLS, rt, opaque uintptr) int32 {
	d := (*lreInterrupt)(unsafe.Pointer(opaque))
	if atomic.LoadInt32(&d.stop) != 0 || (d.deadlineN != 0 && time.Now().UnixNano() > d.deadlineN) {
		return 1
	}
	return 0
}

// fp converts a Go func to the function-pointer form ccgo-generated code expects.
func fp(f any) uintptr {
	type iface [2]uintptr
	return (*iface)(unsafe.Pointer(&f))[1]
}

func newLRE() *lreEngine {
	tls := libc.NewTLS()
	rt := lib.XJS_NewRuntime(tls)
	ctx := lib.XJS_NewContext(tls, rt)
	intr := libc.Xcalloc(tls, 1, libc.Tsize_t(unsafe.Sizeof(lreInterrupt{})))
	lib.XJS_SetInterruptHandler(tls, rt, fp(lreInterruptHandler), intr)
	return &lreEngine{tls: tls, rt: rt, ctx: ctx, intr: intr, cache: map[string]*lreCompiled{}}
}

func (e *lreEngine) Close() {
	for _, c := range e.cache {
		if c.bc != 0 {
			lib.Xjs_free(e.tls, e.ctx, c.bc)
		}
	}
	lib.XJS_FreeContext(e.tls, e.ctx)
	lib.XJS_FreeRuntime(e.tls, e.rt)
	e.tls.Close()
}

func (e *lreEngine) compile(p, f string) *lreCompiled {
	key := f + "/" + p
	if c, ok := e.cache[key]; ok {
		return c
	}
	c := &lreCompiled{groups: scanGroups(p, strings.Contains(f, "v")),
		g: strings.Contains(f, "g"), y: strings.Contains(f, "y"), u: strings.ContainsAny(f, "uv"), d: strings.Contains(f, "d")}
	e.cache[key] = c
	var flags int32
	for _, ch := range f {
		switch ch {
		case 'g':
			flags |= lib.MLRE_FLAG_GLOBAL
		case 'i':
			flags |= lib.MLRE_FLAG_IGNORECASE
		case 'm':
			flags |= lib.MLRE_FLAG_MULTILINE
		case 's':
			flags |= lib.MLRE_FLAG_DOTALL
		case 'u':
			flags |= lib.MLRE_FLAG_UNICODE
		case 'v':
			flags |= lib.MLRE_FLAG_UNICODE_SETS
		case 'y':
			flags |= lib.MLRE_FLAG_STICKY
		case 'd':
			flags |= lib.MLRE_FLAG_INDICES
		default:
			c.err = "invalid flag"
			return c
		}
	}
	pat, _ := libc.CString(p)
	defer libc.Xfree(e.tls, pat)
	errbuf := libc.Xmalloc(e.tls, 128)
	defer libc.Xfree(e.tls, errbuf)
	plen := libc.Xmalloc(e.tls, 8)
	defer libc.Xfree(e.tls, plen)
	bc := lib.Xlre_compile(e.tls, plen, errbuf, 128, pat, libc.Tsize_t(len(p)), flags, e.ctx)
	if bc == 0 {
		c.err = libc.GoString(errbuf)
		if c.err == "" {
			c.err = "compile failed"
		}
		return c
	}
	c.bc = bc
	c.ncap = int(lib.Xlre_get_capture_count(e.tls, bc))
	c.nalloc = max(int(lib.Xlre_get_alloc_count(e.tls, bc)), 2*c.ncap)
	return c
}

// exec runs one match at utf16 offset start; it returns per-group utf16 offsets or nil.
func (e *lreEngine) exec(c *lreCompiled, buf uintptr, n, start int) ([]int, error) {
	capv := libc.Xcalloc(e.tls, libc.Tsize_t(c.nalloc), 8)
	defer libc.Xfree(e.tls, capv)
	d := (*lreInterrupt)(unsafe.Pointer(e.intr))
	d.deadlineN = time.Now().Add(time.Second).UnixNano()
	r := lib.Xlre_exec(e.tls, capv, c.bc, buf, int32(start), int32(n), 1, e.ctx)
	if r < 0 {
		return nil, fmt.Errorf("timeout or error %d", r)
	}
	if r == 0 {
		return nil, nil
	}
	idx := make([]int, 2*c.ncap)
	ptrs := unsafe.Slice((*uintptr)(unsafe.Pointer(capv)), 2*c.ncap)
	for k := range ptrs {
		if ptrs[k] == 0 {
			idx[k] = -1
		} else {
			idx[k] = int(ptrs[k]-buf) / 2
		}
	}
	// a group whose start or end is unset is unmatched
	for k := 0; k < len(idx); k += 2 {
		if idx[k] < 0 || idx[k+1] < 0 {
			idx[k], idx[k+1] = -1, -1
		}
	}
	return idx, nil
}

func (e *lreEngine) Run(p, f, s string) string {
	c := e.compile(p, f)
	if c.err != "" {
		return mustJSON(map[string]string{"ce": c.err})
	}
	units := utf16.Encode([]rune(s))
	n := len(units)
	buf := libc.Xmalloc(e.tls, libc.Tsize_t(2*n+2))
	defer libc.Xfree(e.tls, buf)
	copy(unsafe.Slice((*uint16)(unsafe.Pointer(buf)), n), units)
	in := &input{units: units, len16: n}
	out := struct {
		M []*rec `json:"m"`
	}{M: []*rec{}}
	last := 0
	for k := 0; k < 20; k++ {
		start := 0
		if c.g || c.y {
			start = last
		}
		if start > n {
			out.M = append(out.M, nil)
			break
		}
		idx, err := e.exec(c, buf, n, start)
		if err != nil {
			return `{"to":true}`
		}
		if idx == nil {
			out.M = append(out.M, nil)
			break
		}
		r := &rec{I: idx[0]}
		for gi := 0; gi < c.ncap; gi++ {
			if idx[2*gi] < 0 {
				r.C = append(r.C, nil)
				continue
			}
			t := string(utf16.Decode(units[idx[2*gi]:idx[2*gi+1]]))
			r.C = append(r.C, &t)
		}
		if c.g || c.y {
			r.Li = idx[1]
		}
		if hasNamed(c.groups) {
			for gi, name := range c.groups {
				if name != "" && gi+1 < len(r.C) {
					var v any
					if r.C[gi+1] != nil {
						v = *r.C[gi+1]
					}
					r.G = append(r.G, []any{name, v})
				}
			}
		}
		if c.d {
			r.D = [][]int{}
			for gi := 0; gi < c.ncap; gi++ {
				if idx[2*gi] < 0 {
					r.D = append(r.D, nil)
				} else {
					r.D = append(r.D, []int{idx[2*gi], idx[2*gi+1]})
				}
			}
		}
		out.M = append(out.M, r)
		if !(c.g || c.y) {
			break
		}
		last = idx[1]
		if idx[0] == idx[1] {
			last = in.advance(last, c.u)
		}
	}
	return mustJSON(out)
}

// countMatchesLRE emulates s.match(re) for a global regex.
func (e *lreEngine) countMatches(c *lreCompiled, units []uint16, buf uintptr) int {
	in := &input{units: units, len16: len(units)}
	last, n := 0, 0
	for last <= len(units) && n < 100000 {
		idx, err := e.exec(c, buf, len(units), last)
		if err != nil {
			return -1
		}
		if idx == nil {
			break
		}
		n++
		last = idx[1]
		if idx[0] == idx[1] {
			last = in.advance(last, c.u)
		}
	}
	return n
}
