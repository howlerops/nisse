package re2at

// FindStringSubmatchIndexAt is the one addition to Go's regexp package that a
// JavaScript-compatible fast path needs: match starting at byte offset pos while
// keeping the text before pos as context for \b, ^ and \A. The standard package
// has this internally (doExecute's pos argument) but does not export it.
func (re *Regexp) FindStringSubmatchIndexAt(s string, pos int) []int {
	return re.pad(re.doExecute(nil, nil, s, pos, re.prog.NumCap, nil))
}
