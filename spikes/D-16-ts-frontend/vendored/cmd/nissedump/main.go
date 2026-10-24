// Command nissedump is option B of the D-16 spike: TypeScript's Go compiler packages,
// vendored into our module, used as a library. Same JSONL schema as dump-ts6.mjs.
//
// Usage: nissedump [-j N] <tsconfig.json> <out.jsonl>
//
// -j N dumps files on N goroutines, each file through the checker the program
// assigned it to (strings only cross goroutines, never types).
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/ast"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/bundled"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/checker"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/compiler"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/core"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/scanner"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/tsoptions"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/tspath"
	"github.com/howlerops/nisse/spikes/d16vendored/internal/tsgo/vfs/osvfs"
)

type record struct {
	F   string  `json:"f"`
	N   string  `json:"n"`
	K   int     `json:"k"`
	T   *string `json:"t"`
	S   *string `json:"s"`
	D   *string `json:"d"`
	Sig *string `json:"sig"`
}

func main() {
	jobs := flag.Int("j", 1, "files dumped in parallel")
	flag.Parse()
	configPath, _ := filepath.Abs(flag.Arg(0))
	outPath := flag.Arg(1)
	root := filepath.Dir(configPath)
	ctx := context.Background()

	t0 := time.Now()
	fs := bundled.WrapFS(osvfs.FS())
	host := compiler.NewCompilerHost(fs, bundled.LibPath(), nil, nil, nil)
	parsed, errs := tsoptions.GetParsedCommandLineOfConfigFile(tspath.RootedFilePath(configPath), &core.CompilerOptions{}, nil, fs, nil)
	if parsed == nil {
		fmt.Fprintln(os.Stderr, "config errors:", len(errs))
		os.Exit(1)
	}
	program := compiler.NewProgram(compiler.ProgramOptions{Config: parsed, Host: host})
	program.BindSourceFiles()
	tLoad := time.Now()
	diags := len(program.GetSyntacticDiagnostics(ctx, nil)) + len(program.GetSemanticDiagnostics(ctx, nil))
	tCheck := time.Now()

	var files []*ast.SourceFile
	for _, sf := range program.SourceFiles() {
		if !sf.IsDeclarationFile && !program.IsSourceFileFromExternalLibrary(sf) {
			files = append(files, sf)
		}
	}
	results := make([][]byte, len(files))
	counts := make([]int, len(files))
	var wg sync.WaitGroup
	next := make(chan int)
	for w := 0; w < *jobs; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := range next {
				c, done := program.GetTypeCheckerForFileExclusive(ctx, files[i])
				results[i], counts[i] = dumpFile(c, files[i], root)
				done()
			}
		}()
	}
	for i := range files {
		next <- i
	}
	close(next)
	wg.Wait()

	out, err := os.Create(outPath)
	if err != nil {
		panic(err)
	}
	w := bufio.NewWriter(out)
	ids := 0
	for i := range files {
		w.Write(results[i])
		ids += counts[i]
	}
	w.Flush()
	out.Close()
	tEnd := time.Now()
	stats, _ := json.Marshal(map[string]any{
		"option": "B-vendored", "jobs": *jobs, "files": len(program.SourceFiles()), "ids": ids, "diagnostics": diags,
		"loadMs": tLoad.Sub(t0).Milliseconds(), "checkMs": tCheck.Sub(tLoad).Milliseconds(),
		"dumpMs": tEnd.Sub(tCheck).Milliseconds(), "totalMs": tEnd.Sub(t0).Milliseconds(),
	})
	fmt.Fprintln(os.Stderr, string(stats))
}

func dumpFile(c *checker.Checker, sf *ast.SourceFile, root string) ([]byte, int) {
	rel := relPath(root, string(sf.FileName()))
	typeCache := map[*checker.Type]*string{}
	typeStr := func(t *checker.Type) *string {
		if t == nil {
			return nil
		}
		if s, ok := typeCache[t]; ok {
			return s
		}
		s := c.TypeToStringEx(t, nil, checker.TypeFormatFlagsNoTruncation, nil)
		typeCache[t] = &s
		return &s
	}
	declOf := func(sym *ast.Symbol) *string {
		if sym == nil || len(sym.Declarations()) == 0 {
			return nil
		}
		d := sym.Declarations()[0]
		dsf := ast.GetSourceFileOfNode(d)
		if dsf == nil {
			return nil
		}
		line, col := scanner.GetECMALineAndUTF16CharacterOfPosition(dsf, scanner.GetTokenPosOfNode(d, dsf, false))
		s := fmt.Sprintf("%s:%d:%d", relPath(root, string(dsf.FileName())), line+1, int(col)+1)
		return &s
	}
	sigStr := func(sig *checker.Signature) *string {
		if sig == nil {
			return nil
		}
		s := "("
		for i, p := range sig.Parameters() {
			if i > 0 {
				s += ", "
			}
			if t := typeStr(c.GetTypeOfSymbol(p)); t != nil {
				s += *t
			} else {
				s += "null"
			}
		}
		s += ") => "
		if t := typeStr(c.GetReturnTypeOfSignature(sig)); t != nil {
			s += *t
		}
		return &s
	}
	isCallLike := func(n *ast.Node) bool { return n != nil && (ast.IsCallExpression(n) || ast.IsNewExpression(n)) }

	var buf []byte
	seen := map[string]int{}
	n := 0
	var visit func(node *ast.Node) bool
	visit = func(node *ast.Node) bool {
		if ast.IsIdentifier(node) {
			name := node.Text()
			seen[name]++
			r := record{F: rel, N: name, K: seen[name]}
			r.T = typeStr(c.GetTypeAtLocation(node))
			if sym := c.GetSymbolAtLocation(node); sym != nil {
				// Internal symbol names start with the byte 0xFE; the API shows them as "__name".
				name := strings.Replace(sym.Name(), "\xfe", "__", 1)
				r.S = &name
				r.D = declOf(sym)
			}
			p := node.Parent
			var callee *ast.Node
			if isCallLike(p) && p.Expression() == node {
				callee = p
			} else if p != nil && ast.IsPropertyAccessExpression(p) && p.Name() == node && isCallLike(p.Parent) && p.Parent.Expression() == p {
				callee = p.Parent
			}
			if callee != nil {
				r.Sig = sigStr(c.GetResolvedSignature(callee))
			}
			line, _ := json.Marshal(r)
			buf = append(append(buf, line...), '\n')
			n++
		}
		node.ForEachChild(visit)
		return false
	}
	visit(sf.AsNode())
	return buf, n
}

// relPath makes paths comparable with the other dumps: project files relative to the
// tsconfig directory, embedded default libs as "bundled/lib/<name>".
func relPath(root, p string) string {
	if strings.HasPrefix(p, "bundled:") {
		return "bundled/lib/" + filepath.Base(p)
	}
	r, err := filepath.Rel(root, p)
	if err != nil {
		return p
	}
	return r
}
