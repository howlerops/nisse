// Command apidiff lists the exported API of Go packages (functions, methods, types,
// vars, consts, with signatures) so two revisions can be compared.
// Usage: apidiff <old internal dir> <new internal dir> pkg...
package main

import (
	"bytes"
	"fmt"
	"go/ast"
	"go/parser"
	"go/printer"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

func api(dir string) map[string]string {
	fset := token.NewFileSet()
	out := map[string]string{}
	pkgs, err := parser.ParseDir(fset, dir, func(fi os.FileInfo) bool { return !strings.HasSuffix(fi.Name(), "_test.go") }, 0)
	if err != nil {
		return out
	}
	str := func(n any) string {
		var b bytes.Buffer
		printer.Fprint(&b, fset, n)
		return strings.Join(strings.Fields(b.String()), " ")
	}
	for _, p := range pkgs {
		for _, f := range p.Files {
			for _, d := range f.Decls {
				switch d := d.(type) {
				case *ast.FuncDecl:
					if !d.Name.IsExported() {
						continue
					}
					key := d.Name.Name
					if d.Recv != nil {
						recv := str(d.Recv.List[0].Type)
						if base := strings.TrimLeft(strings.Split(recv, "[")[0], "*"); !ast.IsExported(base) {
							continue
						}
						key = recv + "." + key
					}
					out["func "+key] = str(d.Type)
				case *ast.GenDecl:
					for _, s := range d.Specs {
						switch s := s.(type) {
						case *ast.TypeSpec:
							if s.Name.IsExported() {
								sig := ""
								if st, ok := s.Type.(*ast.StructType); ok {
									for _, fl := range st.Fields.List {
										for _, n := range fl.Names {
											if n.IsExported() {
												out["field "+s.Name.Name+"."+n.Name] = str(fl.Type)
											}
										}
									}
									sig = "struct"
								} else {
									sig = str(s.Type)
								}
								out["type "+s.Name.Name] = sig
							}
						case *ast.ValueSpec:
							for _, n := range s.Names {
								if n.IsExported() {
									out[d.Tok.String()+" "+n.Name] = ""
								}
							}
						}
					}
				}
			}
		}
	}
	return out
}

func main() {
	oldDir, newDir := os.Args[1], os.Args[2]
	fmt.Println("package\told\tnew\tremoved\tsignature changed\tadded")
	for _, pkg := range os.Args[3:] {
		o, n := api(filepath.Join(oldDir, pkg)), api(filepath.Join(newDir, pkg))
		var removed, changed, added []string
		for k, v := range o {
			if nv, ok := n[k]; !ok {
				removed = append(removed, k)
			} else if nv != v {
				changed = append(changed, k)
			}
		}
		for k := range n {
			if _, ok := o[k]; !ok {
				added = append(added, k)
			}
		}
		sort.Strings(removed)
		sort.Strings(changed)
		fmt.Printf("%s\t%d\t%d\t%d\t%d\t%d\n", pkg, len(o), len(n), len(removed), len(changed), len(added))
		if os.Getenv("VERBOSE") != "" {
			for _, k := range removed {
				fmt.Println("  - " + k)
			}
			for _, k := range changed {
				fmt.Println("  ~ " + k)
			}
		}
	}
}
