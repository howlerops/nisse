package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/dop251/goja"
	"github.com/fastschema/qjs"
	"modernc.org/quickjs"
)

// Engine renders the benchmark page. Each instance is single-threaded.
type Engine interface {
	Render() (string, error)
	Close()
}

// StreamEngine can also run renderToReadableStream to completion.
type StreamEngine interface {
	StartStream() error
	PollStream() (string, error)
}

// factory loads the bundle once and creates instances from it.
type factory func() (Engine, error)

func newFactory(name, bundle string) (factory, error) {
	src, err := os.ReadFile(bundle)
	if err != nil {
		return nil, err
	}
	switch name {
	case "goja":
		prg, err := goja.Compile("ssr.js", string(src), false)
		if err != nil {
			return nil, err
		}
		return func() (Engine, error) { return newGoja(prg) }, nil
	case "qjs-wazero":
		return func() (Engine, error) { return newQJS(string(src)) }, nil
	case "modernc":
		return func() (Engine, error) { return newModernc(string(src)) }, nil
	case "native":
		return func() (Engine, error) { return &nativeEngine{rows: makeData(800)}, nil }, nil
	}
	return nil, fmt.Errorf("unknown engine %q", name)
}

// --- goja ---

type gojaEngine struct {
	vm                  *goja.Runtime
	render, start, poll goja.Callable
}

func newGoja(prg *goja.Program) (*gojaEngine, error) {
	vm := goja.New()
	if _, err := vm.RunProgram(prg); err != nil {
		return nil, err
	}
	e := &gojaEngine{vm: vm}
	e.render, _ = goja.AssertFunction(vm.Get("renderPage"))
	e.start, _ = goja.AssertFunction(vm.Get("startStream"))
	e.poll, _ = goja.AssertFunction(vm.Get("pollStream"))
	return e, nil
}

func (e *gojaEngine) Render() (string, error) {
	v, err := e.render(goja.Undefined())
	if err != nil {
		return "", err
	}
	return v.String(), nil
}
func (e *gojaEngine) StartStream() error { _, err := e.start(goja.Undefined()); return err }
func (e *gojaEngine) PollStream() (string, error) {
	v, err := e.poll(goja.Undefined())
	if err != nil {
		return "", err
	}
	return v.String(), nil
}
func (e *gojaEngine) Close() {}

// --- QuickJS-NG compiled to WASM, run by wazero (fastschema/qjs) ---

type qjsEngine struct{ rt *qjs.Runtime }

func newQJS(src string) (*qjsEngine, error) {
	rt, err := qjs.New(qjs.Option{MaxStackSize: 8 << 20})
	if err != nil {
		return nil, err
	}
	v, err := rt.Context().Eval("ssr.js", qjs.Code(src), qjs.TypeGlobal())
	if err != nil {
		rt.Close()
		return nil, err
	}
	v.Free()
	return &qjsEngine{rt: rt}, nil
}

func (e *qjsEngine) eval(code string) (string, error) {
	v, err := e.rt.Context().Eval("call.js", qjs.Code(code), qjs.TypeGlobal())
	if err != nil {
		return "", err
	}
	defer v.Free()
	return v.String(), nil
}
func (e *qjsEngine) Render() (string, error)     { return e.eval("renderPage()") }
func (e *qjsEngine) StartStream() error          { _, err := e.eval("startStream()"); return err }
func (e *qjsEngine) PollStream() (string, error) { return e.eval("pollStream()") }
func (e *qjsEngine) Close()                      { e.rt.Close() }

// --- QuickJS transpiled from C to pure Go (modernc.org/quickjs) ---

type moderncEngine struct{ vm *quickjs.VM }

func newModernc(src string) (*moderncEngine, error) {
	vm, err := quickjs.NewVM()
	if err != nil {
		return nil, err
	}
	vm.SetMaxStackSize(8 << 20)
	if _, err := vm.Eval(src, quickjs.EvalGlobal); err != nil {
		vm.Close()
		return nil, err
	}
	return &moderncEngine{vm: vm}, nil
}

func (e *moderncEngine) call(fn string) (string, error) {
	v, err := e.vm.Call(fn)
	if err != nil {
		return "", err
	}
	if _, err := e.vm.ExecutePendingJobs(); err != nil {
		return "", err
	}
	s, _ := v.(string)
	return s, nil
}
func (e *moderncEngine) Render() (string, error)     { return e.call("renderPage") }
func (e *moderncEngine) StartStream() error          { _, err := e.call("startStream"); return err }
func (e *moderncEngine) PollStream() (string, error) { return e.call("pollStream") }
func (e *moderncEngine) Close()                      { e.vm.Close() }

// runStream drives renderToReadableStream to completion; returns html and chunk count.
func runStream(e Engine) (string, int, error) {
	se, ok := e.(StreamEngine)
	if !ok {
		return "", 0, fmt.Errorf("no streaming")
	}
	if err := se.StartStream(); err != nil {
		return "", 0, err
	}
	for i := 0; i < 1_000_000; i++ {
		s, err := se.PollStream()
		if err != nil {
			return "", 0, err
		}
		if s != "" {
			n, html, _ := strings.Cut(s, "\n")
			c, _ := strconv.Atoi(n)
			return html, c, nil
		}
	}
	return "", 0, fmt.Errorf("stream did not finish")
}
