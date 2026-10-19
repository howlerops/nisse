package main

import (
	"fmt"

	lib "modernc.org/libquickjs"
	"modernc.org/libc"
)

func main() {
	tls := libc.NewTLS()
	rt := lib.XJS_NewRuntime(tls)
	ctx := lib.XJS_NewContextRaw(tls, rt)
	pat, _ := libc.CString(`(?<=a)b`)
	errbuf := libc.Xmalloc(tls, 64)
	plen := libc.Xmalloc(tls, 8)
	bc := lib.Xlre_compile(tls, plen, errbuf, 64, pat, 7, 0, ctx)
	fmt.Println(bc != 0)
}
