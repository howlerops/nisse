package main

import (
	"math"
	"strconv"
	"strings"
)

// The Go-native baseline: what server JSX lowered to Go could produce.
// It writes byte-identical HTML to react-dom/server for the benchmark app.

type row struct {
	id                  int
	name, email, status string
	mrr                 float64
	tags                []string
	note                string
	hasNote             bool
}

// makeData mirrors app.jsx, including its float64 arithmetic.
func makeData(n int) []row {
	seed := 42.0
	rand := func() float64 {
		seed = math.Mod(seed*1103515245+12345, 2147483648)
		return seed / 2147483648
	}
	statuses := []string{"active", "paused", "archived"}
	rows := make([]row, 0, n)
	for i := 0; i < n; i++ {
		r := rand()
		rw := row{
			id:     i + 1,
			name:   "Customer " + strconv.Itoa(i+1),
			email:  "user" + strconv.Itoa(i+1) + "@example.com",
			status: statuses[int(math.Floor(r*3))],
			mrr:    math.Floor(rand()*100000) / 100,
		}
		switch {
		case i%7 == 0:
			rw.tags = []string{"vip", "beta"}
		case i%3 == 0:
			rw.tags = []string{"beta"}
		}
		if i%11 == 0 {
			rw.note, rw.hasNote = `Needs <review> & "follow-up"`, true
		}
		rows = append(rows, rw)
	}
	return rows
}

var escaper = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;", "'", "&#x27;")

type nativeEngine struct{ rows []row }

func (e *nativeEngine) Close() {}

func (e *nativeEngine) Render() (string, error) {
	var b strings.Builder
	b.Grow(240 << 10)
	w := b.WriteString
	w(`<div class="app"><header><h1>Customers</h1><span class="user">Signed in as <!-- -->Ada</span></header>`)
	w(`<form class="filter"><label for="_R_2_">Search</label><input id="_R_2_" type="search" placeholder="Name or email" value=""/><input type="checkbox" aria-label="Active only" checked=""/></form>`)
	var active, paused, archived int
	var mrr float64
	for _, r := range e.rows {
		switch r.status {
		case "active":
			active++
		case "paused":
			paused++
		default:
			archived++
		}
		mrr += r.mrr
	}
	w(`<section class="summary"><h2>Summary</h2><p>Active: <!-- -->`)
	w(strconv.Itoa(active))
	w(`<!-- -->, paused: <!-- -->`)
	w(strconv.Itoa(paused))
	w(`<!-- -->, archived: <!-- -->`)
	w(strconv.Itoa(archived))
	w(`</p><p>Total MRR: $<!-- -->`)
	w(strconv.FormatFloat(mrr, 'f', 2, 64))
	w(`</p></section><table class="customers"><thead><tr><th>ID</th><th>Name</th><th>Email</th><th>Status</th><th>MRR</th><th>Notes</th></tr></thead><tbody>`)
	for i, r := range e.rows {
		id := strconv.Itoa(r.id)
		if i%2 == 0 {
			w(`<tr class="even" data-id="`)
		} else {
			w(`<tr class="odd" data-id="`)
		}
		w(id)
		w(`"><td>`)
		w(id)
		w(`</td><td><a href="/customers/`)
		w(id)
		w(`">`)
		w(escaper.Replace(r.name))
		w(`</a>`)
		if len(r.tags) > 0 {
			w(`<ul class="tags">`)
			for _, t := range r.tags {
				w(`<li>`)
				w(escaper.Replace(t))
				w(`</li>`)
			}
			w(`</ul>`)
		}
		w(`</td><td>`)
		w(escaper.Replace(r.email))
		w(`</td><td><span class="badge badge-`)
		w(r.status)
		w(`" style="border-color:#f60">`)
		w(r.status)
		w(`</span></td><td class="num">$<!-- -->`)
		w(strconv.FormatFloat(r.mrr, 'f', 2, 64))
		w(`</td><td>`)
		if r.hasNote {
			w(`<em>`)
			w(escaper.Replace(r.note))
			w(`</em>`)
		} else {
			w("—")
		}
		if i%50 == 0 {
			w(`<details open=""><summary>More</summary>Row <!-- -->`)
			w(strconv.Itoa(i))
			w(`<!-- --> details</details>`)
		}
		w(`</td></tr>`)
	}
	w(`</tbody></table><footer>`)
	w(strconv.Itoa(len(e.rows)))
	w(`<!-- --> customers</footer></div>`)
	return b.String(), nil
}
