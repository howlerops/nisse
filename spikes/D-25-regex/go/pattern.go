package main

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"unicode"
)

// scanGroups lists capture groups in JavaScript numbering order; each entry is
// the group name, or "" for an unnamed group.
func scanGroups(p string, vflag bool) []string {
	rs := []rune(p)
	var out []string
	depth := 0
	for i := 0; i < len(rs); i++ {
		c := rs[i]
		switch {
		case c == '\\':
			i++
		case c == '[':
			if depth == 0 || vflag {
				depth++
			}
			// "[]" and "[^]" close immediately in JavaScript, unlike Perl.
		case c == ']' && depth > 0:
			depth--
		case depth > 0:
		case c == '(':
			if i+1 < len(rs) && rs[i+1] == '?' {
				if i+2 < len(rs) && rs[i+2] == '<' && i+3 < len(rs) && rs[i+3] != '=' && rs[i+3] != '!' {
					j := i + 3
					for j < len(rs) && rs[j] != '>' {
						j++
					}
					out = append(out, string(rs[i+3:j]))
				}
			} else {
				out = append(out, "")
			}
		}
	}
	return out
}

// features reports which regex features a pattern uses, for grouping results.
func features(p, f string) []string {
	var fs []string
	add := func(cond bool, name string) {
		if cond {
			fs = append(fs, name)
		}
	}
	add(strings.Contains(p, "(?<=") || strings.Contains(p, "(?<!"), "lookbehind")
	add(strings.Contains(p, "(?=") || strings.Contains(p, "(?!"), "lookahead")
	hasNamed := false
	for _, g := range scanGroups(p, strings.Contains(f, "v")) {
		if g != "" {
			hasNamed = true
		}
	}
	add(hasNamed, "named-groups")
	add(backrefRe(p), "backreference")
	add(strings.Contains(f, "u"), "flag-u")
	add(strings.Contains(f, "v"), "flag-v")
	add(strings.Contains(f, "i"), "flag-i")
	add(strings.Contains(f, "m"), "flag-m")
	add(strings.Contains(f, "s"), "flag-s")
	add(strings.Contains(f, "y"), "flag-y")
	add(strings.Contains(f, "g"), "flag-g")
	add(strings.Contains(p, `\p{`) || strings.Contains(p, `\P{`), "unicode-property")
	if len(fs) == 0 || (len(fs) == 1 && fs[0] == "flag-g") {
		fs = append(fs, "plain")
	}
	return fs
}

func backrefRe(p string) bool {
	rs := []rune(p)
	inClass := false
	for i := 0; i < len(rs)-1; i++ {
		switch rs[i] {
		case '\\':
			n := rs[i+1]
			if !inClass && ((n >= '1' && n <= '9') || n == 'k') {
				return true
			}
			i++
		case '[':
			inClass = true
		case ']':
			inClass = false
		}
	}
	return false
}

// ---- RE2 fast-path translator ----

// re2Plan is the result of translating a JavaScript pattern to Go RE2 syntax.
type re2Plan struct {
	goPattern    string
	ok           bool
	reason       string
	groups       []string
	astralUnsafe bool // non-u pattern: fall back when the input has astral characters
	lineTermMode bool // m flag with ^/$: fall back when the input has \r, U+2028 or U+2029
}

type tr struct {
	rs     []rune
	i      int
	ic, u  bool // i flag without u (explicit case expansion) / u flag
	iu     bool
	dotAll bool
	multi  bool
	out    strings.Builder
	plan   *re2Plan
	// group stack: number of captures seen when the group opened
	stack []frame
	// last atom, for quantifier checks
	lastNestedCaps int
	lastAstral     bool
	lastOK         bool
	lastCapNull    bool // last atom is a capturing group whose body can match empty
	ncaps          int
}

type frame struct {
	capsAtOpen int
	capturing  bool
}

type ineligible string

const jsSpace = `\t\n\x{0B}\x{0C}\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}`

var longGC = map[string]string{
	"Letter": "L", "Uppercase_Letter": "Lu", "Lowercase_Letter": "Ll", "Titlecase_Letter": "Lt", "Modifier_Letter": "Lm", "Other_Letter": "Lo",
	"Mark": "M", "Combining_Mark": "M", "Nonspacing_Mark": "Mn", "Spacing_Mark": "Mc", "Enclosing_Mark": "Me",
	"Number": "N", "Decimal_Number": "Nd", "digit": "Nd", "Letter_Number": "Nl", "Other_Number": "No",
	"Punctuation": "P", "punct": "P", "Connector_Punctuation": "Pc", "Dash_Punctuation": "Pd", "Open_Punctuation": "Ps", "Close_Punctuation": "Pe",
	"Initial_Punctuation": "Pi", "Final_Punctuation": "Pf", "Other_Punctuation": "Po",
	"Symbol": "S", "Math_Symbol": "Sm", "Currency_Symbol": "Sc", "Modifier_Symbol": "Sk", "Other_Symbol": "So",
	"Separator": "Z", "Space_Separator": "Zs", "Line_Separator": "Zl", "Paragraph_Separator": "Zp",
	"Other": "C", "Control": "Cc", "cntrl": "Cc", "Format": "Cf", "Private_Use": "Co", "Surrogate": "Cs",
}

func translateRE2(p, f string) (plan re2Plan) {
	plan.groups = scanGroups(p, strings.Contains(f, "v"))
	defer func() {
		if r := recover(); r != nil {
			if why, ok := r.(ineligible); ok {
				plan.ok, plan.reason, plan.goPattern = false, string(why), ""
				return
			}
			panic(r)
		}
	}()
	t := &tr{rs: []rune(p), plan: &plan}
	for _, c := range f {
		switch c {
		case 'i':
			t.ic = true
		case 'u':
			t.u = true
		case 's':
			t.dotAll = true
		case 'm':
			t.multi = true
		case 'v':
			panic(ineligible("flag v"))
		}
	}
	if t.ic && t.u {
		t.ic, t.iu = false, true
		t.out.WriteString("(?i)")
	}
	if !t.u {
		plan.astralUnsafe = true
	}
	for _, g := range plan.groups {
		if g != "" && !isGoName(g) {
			panic(ineligible("group name not a Go identifier"))
		}
	}
	seen := map[string]bool{}
	for _, g := range plan.groups {
		if g != "" && seen[g] {
			panic(ineligible("duplicate group name"))
		}
		seen[g] = true
	}
	t.alternation()
	plan.goPattern = t.out.String()
	plan.ok = true
	return
}

func isGoName(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if !(c == '_' || c < 128 && (unicode.IsLetter(c) || unicode.IsDigit(c))) {
			return false
		}
	}
	return true
}

func (t *tr) peek(k int) rune {
	if t.i+k < len(t.rs) {
		return t.rs[t.i+k]
	}
	return -1
}

func (t *tr) alternation() {
	for t.i < len(t.rs) {
		c := t.rs[t.i]
		switch c {
		case '|':
			t.out.WriteByte('|')
			t.i++
			t.lastOK = false
		case ')':
			return
		default:
			t.term()
		}
	}
}

func (t *tr) term() {
	c := t.rs[t.i]
	switch c {
	case '^':
		t.i++
		if t.multi {
			t.plan.lineTermMode = true
			t.out.WriteString(`(?m:^)`)
		} else {
			t.out.WriteString(`\A`)
		}
		t.lastOK = false
		return
	case '$':
		t.i++
		if t.multi {
			t.plan.lineTermMode = true
			t.out.WriteString(`(?m:$)`)
		} else {
			t.out.WriteString(`\z`)
		}
		t.lastOK = false
		return
	case '(':
		t.group()
	case '[':
		t.class()
	case '.':
		t.i++
		if t.dotAll {
			t.out.WriteString(`[\x{0}-\x{10FFFF}]`)
		} else {
			t.out.WriteString(`[^\n\r\x{2028}\x{2029}]`)
		}
		t.setAtom(0, false)
	case '\\':
		t.escape()
	case '*', '+', '?':
		panic(ineligible("quantifier without atom"))
	case '{':
		if _, _, _, ok := t.braceQuant(); ok {
			panic(ineligible("quantifier without atom"))
		}
		t.i++
		t.out.WriteString(`\{`)
		t.setAtom(0, false)
	default:
		t.i++
		t.literal(c)
	}
	t.quantifier()
}

func (t *tr) setAtom(nested int, astral bool) {
	t.lastNestedCaps, t.lastAstral, t.lastOK, t.lastCapNull = nested, astral, true, false
}

func (t *tr) quantifier() {
	if t.i >= len(t.rs) {
		return
	}
	c := t.rs[t.i]
	var q string
	switch c {
	case '*', '+', '?':
		q = string(c)
		t.i++
	case '{':
		lo, hi, n, ok := t.braceQuant()
		if !ok {
			return
		}
		if lo > 1000 || hi > 1000 {
			panic(ineligible("repeat count over 1000"))
		}
		q = string(t.rs[t.i : t.i+n])
		t.i += n
	default:
		return
	}
	if !t.lastOK {
		panic(ineligible("quantified assertion"))
	}
	if t.lastNestedCaps > 0 {
		panic(ineligible("capture inside quantified group"))
	}
	if t.lastCapNull {
		panic(ineligible("quantified capturing group that can match empty"))
	}
	if t.lastAstral && !t.u {
		panic(ineligible("quantified astral literal without u"))
	}
	if t.peek(0) == '?' {
		q += "?"
		t.i++
	}
	t.out.WriteString(q)
	t.lastOK = false
}

// braceQuant parses {n}, {n,} or {n,m} at t.i without consuming it.
func (t *tr) braceQuant() (lo, hi, n int, ok bool) {
	j := t.i + 1
	start := j
	for j < len(t.rs) && t.rs[j] >= '0' && t.rs[j] <= '9' {
		j++
	}
	if j == start {
		return
	}
	lo, _ = strconv.Atoi(string(t.rs[start:j]))
	hi = lo
	if j < len(t.rs) && t.rs[j] == ',' {
		j++
		s2 := j
		for j < len(t.rs) && t.rs[j] >= '0' && t.rs[j] <= '9' {
			j++
		}
		if j > s2 {
			hi, _ = strconv.Atoi(string(t.rs[s2:j]))
		} else {
			hi = 0
		}
	}
	if j < len(t.rs) && t.rs[j] == '}' {
		return lo, hi, j + 1 - t.i, true
	}
	return
}

func (t *tr) group() {
	t.i++ // (
	capsBefore := t.ncaps
	outStart := t.out.Len()
	capturing := false
	switch {
	case t.peek(0) == '?' && t.peek(1) == ':':
		t.i += 2
		t.out.WriteString("(?:")
	case t.peek(0) == '?' && (t.peek(1) == '=' || t.peek(1) == '!'):
		panic(ineligible("lookahead"))
	case t.peek(0) == '?' && t.peek(1) == '<' && (t.peek(2) == '=' || t.peek(2) == '!'):
		panic(ineligible("lookbehind"))
	case t.peek(0) == '?' && t.peek(1) == '<':
		j := t.i + 2
		for t.rs[j] != '>' {
			j++
		}
		t.out.WriteString("(?P<" + string(t.rs[t.i+2:j]) + ">")
		t.i = j + 1
		capturing = true
	case t.peek(0) == '?':
		panic(ineligible("modifiers"))
	default:
		t.out.WriteByte('(')
		capturing = true
	}
	if capturing {
		t.ncaps++
	}
	t.alternation()
	if t.peek(0) != ')' {
		panic(ineligible("unbalanced group"))
	}
	t.i++
	t.out.WriteByte(')')
	nested := t.ncaps - capsBefore
	if capturing {
		nested--
	}
	t.setAtom(nested, false)
	if capturing {
		frag := t.out.String()[outStart:]
		re, err := regexp.Compile(`^(?:` + frag + `)`)
		t.lastCapNull = err != nil || re.MatchString("") || strings.Contains(frag, `\b`) || strings.Contains(frag, `\B`) ||
			strings.Contains(frag, `\A`) || strings.Contains(frag, `\z`) || strings.Contains(frag, `(?m:`)
	}
}

func (t *tr) literal(c rune) {
	astral := c > 0xFFFF
	if t.ic && c > 127 && unicode.SimpleFold(c) != c {
		panic(ineligible("non-ASCII cased letter with i and no u"))
	}
	if t.ic && ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) {
		fmt.Fprintf(&t.out, `[\x{%X}\x{%X}]`, unicode.ToLower(c), unicode.ToUpper(c))
	} else {
		fmt.Fprintf(&t.out, `\x{%X}`, c)
	}
	t.setAtom(0, astral)
}

// escapeChar decodes a character escape (after the backslash) shared by atoms and classes.
// It returns -1 if the escape is not a single character.
func (t *tr) escapeChar(inClass bool) rune {
	c := t.peek(0)
	switch c {
	case 't':
		t.i++
		return '\t'
	case 'n':
		t.i++
		return '\n'
	case 'v':
		t.i++
		return 0x0B
	case 'f':
		t.i++
		return 0x0C
	case 'r':
		t.i++
		return '\r'
	case '0':
		if n := t.peek(1); n >= '0' && n <= '9' {
			panic(ineligible("octal escape"))
		}
		t.i++
		return 0
	case 'c':
		n := t.peek(1)
		if (n >= 'a' && n <= 'z') || (n >= 'A' && n <= 'Z') {
			t.i += 2
			return n % 32
		}
		panic(ineligible("annex-b \\c"))
	case 'x':
		if isHex(t.peek(1)) && isHex(t.peek(2)) {
			v, _ := strconv.ParseUint(string(t.rs[t.i+1:t.i+3]), 16, 32)
			t.i += 3
			return rune(v)
		}
		panic(ineligible("annex-b \\x"))
	case 'u':
		if t.peek(1) == '{' && t.u {
			j := t.i + 2
			for t.rs[j] != '}' {
				j++
			}
			v, _ := strconv.ParseUint(string(t.rs[t.i+2:j]), 16, 32)
			t.i = j + 1
			return rune(v)
		}
		if isHex(t.peek(1)) && isHex(t.peek(2)) && isHex(t.peek(3)) && isHex(t.peek(4)) {
			v, _ := strconv.ParseUint(string(t.rs[t.i+1:t.i+5]), 16, 32)
			t.i += 5
			if v >= 0xD800 && v <= 0xDBFF && t.u && t.peek(0) == '\\' && t.peek(1) == 'u' {
				lo, _ := strconv.ParseUint(string(t.rs[t.i+2:t.i+6]), 16, 32)
				if lo >= 0xDC00 && lo <= 0xDFFF {
					t.i += 6
					return rune(0x10000 + (v-0xD800)<<10 + (lo - 0xDC00))
				}
			}
			if v >= 0xD800 && v <= 0xDFFF {
				panic(ineligible("surrogate escape"))
			}
			return rune(v)
		}
		panic(ineligible("annex-b \\u"))
	}
	if strings.ContainsRune(`^$\.*+?()[]{}|/-`, c) {
		t.i++
		return c
	}
	if !t.u && c >= 0 && !unicode.IsLetter(c) && !unicode.IsDigit(c) {
		t.i++
		return c // annex-b identity escape of punctuation
	}
	if !t.u && c >= 0 && unicode.IsLetter(c) && !strings.ContainsRune("bBdDsSwWkpP", c) {
		t.i++
		return c // annex-b identity escape of a letter, e.g. \a means "a"
	}
	return -1
}

func isHex(c rune) bool {
	return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')
}

func (t *tr) property() string {
	neg := t.peek(0) == 'P'
	if t.peek(1) != '{' {
		panic(ineligible("bad property"))
	}
	j := t.i + 2
	for t.rs[j] != '}' {
		j++
	}
	name := string(t.rs[t.i+2 : j])
	t.i = j + 1
	val := name
	if k := strings.IndexByte(name, '='); k >= 0 {
		key := name[:k]
		val = name[k+1:]
		switch key {
		case "Script", "sc":
			if _, ok := unicode.Scripts[val]; !ok {
				panic(ineligible("script not in Go tables: " + val))
			}
		case "General_Category", "gc":
			if l, ok := longGC[val]; ok {
				val = l
			}
			if _, ok := unicode.Categories[val]; !ok {
				panic(ineligible("category not in Go tables"))
			}
		default:
			panic(ineligible("property " + key))
		}
	} else {
		switch {
		case longGC[val] != "":
			val = longGC[val]
		case unicode.Categories[val] != nil:
		case val == "ASCII":
			if neg {
				return `[^\x{0}-\x{7F}]`
			}
			return `[\x{0}-\x{7F}]`
		case val == "Any":
			if neg {
				panic(ineligible("\\P{Any}"))
			}
			return `[\x{0}-\x{10FFFF}]`
		default:
			panic(ineligible("binary property " + val))
		}
	}
	if neg {
		return `\P{` + val + `}`
	}
	return `\p{` + val + `}`
}

func (t *tr) escape() {
	t.i++ // backslash
	c := t.peek(0)
	switch c {
	case 'd':
		t.i++
		t.out.WriteString(`[0-9]`)
	case 'D':
		t.i++
		t.out.WriteString(`[^0-9]`)
	case 'w', 'W':
		if t.iu {
			panic(ineligible("\\w with iu"))
		}
		t.i++
		if c == 'w' {
			t.out.WriteString(`[0-9A-Za-z_]`)
		} else {
			t.out.WriteString(`[^0-9A-Za-z_]`)
		}
	case 's':
		t.i++
		t.out.WriteString(`[` + jsSpace + `]`)
	case 'S':
		t.i++
		t.out.WriteString(`[^` + jsSpace + `]`)
	case 'b', 'B':
		if t.iu {
			panic(ineligible("\\b with iu"))
		}
		t.i++
		t.out.WriteString(`\` + string(c))
		t.lastOK = false
		return
	case 'k':
		if t.u || hasNamed(t.plan.groups) {
			panic(ineligible("backreference"))
		}
		t.i++
		t.literal('k')
		return
	case 'p', 'P':
		if !t.u {
			t.i++
			t.literal(c)
			return
		}
		t.out.WriteString(t.property())
	default:
		if c >= '1' && c <= '9' {
			panic(ineligible("backreference"))
		}
		ch := t.escapeChar(false)
		if ch < 0 {
			panic(ineligible("unknown escape"))
		}
		t.literal(ch)
		return
	}
	t.setAtom(0, false)
}

func hasNamed(g []string) bool {
	for _, n := range g {
		if n != "" {
			return true
		}
	}
	return false
}

type rng struct{ lo, hi rune }

func (t *tr) class() {
	t.i++ // [
	neg := false
	if t.peek(0) == '^' {
		neg = true
		t.i++
	}
	var ranges []rng
	var frags []string
	astral := false
	if t.peek(0) == ']' {
		t.i++
		if neg {
			t.out.WriteString(`[\x{0}-\x{10FFFF}]`)
			t.setAtom(0, false)
			return
		}
		panic(ineligible("empty class"))
	}
	// classAtom returns (char, isChar). Class escapes append to frags/ranges directly.
	classAtom := func() (rune, bool) {
		c := t.rs[t.i]
		if c != '\\' {
			t.i++
			return c, true
		}
		t.i++
		e := t.peek(0)
		switch e {
		case 'd':
			t.i++
			ranges = append(ranges, rng{'0', '9'})
			return 0, false
		case 'D':
			t.i++
			frags = append(frags, `\D`)
			return 0, false
		case 'w', 'W':
			if t.iu {
				panic(ineligible("\\w in class with iu"))
			}
			t.i++
			if e == 'w' {
				ranges = append(ranges, rng{'0', '9'}, rng{'A', 'Z'}, rng{'a', 'z'}, rng{'_', '_'})
			} else {
				frags = append(frags, `\W`)
			}
			return 0, false
		case 's':
			t.i++
			frags = append(frags, jsSpace)
			return 0, false
		case 'S':
			panic(ineligible("\\S in class"))
		case 'b':
			t.i++
			return 8, true
		case 'p', 'P':
			if !t.u {
				t.i++
				return e, true
			}
			switch pr := t.property(); pr {
			case `[\x{0}-\x{7F}]`:
				ranges = append(ranges, rng{0, 0x7F})
			case `[\x{0}-\x{10FFFF}]`:
				ranges = append(ranges, rng{0, 0x10FFFF})
			case `[^\x{0}-\x{7F}]`:
				panic(ineligible("\\P{ASCII} in class"))
			default:
				frags = append(frags, pr)
			}
			return 0, false
		case 'k':
			if !t.u {
				t.i++
				return 'k', true
			}
		}
		if e >= '1' && e <= '9' {
			panic(ineligible("annex-b class octal"))
		}
		ch := t.escapeChar(true)
		if ch < 0 {
			panic(ineligible("unknown class escape"))
		}
		return ch, true
	}
	for {
		if t.i >= len(t.rs) {
			panic(ineligible("unterminated class"))
		}
		if t.rs[t.i] == ']' {
			t.i++
			break
		}
		a, aok := classAtom()
		if t.peek(0) == '-' && t.peek(1) != ']' && t.peek(1) != -1 {
			save := t.i
			t.i++
			b, bok := classAtom()
			if aok && bok {
				ranges = append(ranges, rng{a, b})
				if b > 0xFFFF {
					astral = true
				}
				continue
			}
			// Annex B: a class escape on either side makes '-' literal.
			_ = save
			if aok {
				ranges = append(ranges, rng{a, a})
			}
			ranges = append(ranges, rng{'-', '-'})
			if bok {
				ranges = append(ranges, rng{b, b})
			}
			continue
		}
		if aok {
			ranges = append(ranges, rng{a, a})
			if a > 0xFFFF {
				astral = true
			}
		}
	}
	if astral && !t.u {
		panic(ineligible("astral in class without u"))
	}
	if t.ic {
		var extra []rng
		for _, r := range ranges {
			if r.hi > 127 {
				panic(ineligible("non-ASCII class with i and no u"))
			}
			for c := r.lo; c <= r.hi; c++ {
				if c >= 'a' && c <= 'z' {
					extra = append(extra, rng{c - 32, c - 32})
				} else if c >= 'A' && c <= 'Z' {
					extra = append(extra, rng{c + 32, c + 32})
				}
			}
		}
		ranges = append(ranges, extra...)
		for _, fr := range frags {
			if strings.HasPrefix(fr, `\p`) || strings.HasPrefix(fr, `\P`) {
				panic(ineligible("property in class with i and no u"))
			}
		}
	}
	t.out.WriteByte('[')
	if neg {
		t.out.WriteByte('^')
	}
	for _, r := range ranges {
		if r.lo == r.hi {
			fmt.Fprintf(&t.out, `\x{%X}`, r.lo)
		} else {
			fmt.Fprintf(&t.out, `\x{%X}-\x{%X}`, r.lo, r.hi)
		}
	}
	for _, fr := range frags {
		t.out.WriteString(fr)
	}
	t.out.WriteByte(']')
	t.setAtom(0, false)
}
