#!/usr/bin/env bash
# Option B: copy TypeScript's Go compiler packages (tsc/internal/...) into this module
# under internal/tsgo/ and rewrite their import paths, so our own code can import them.
# Usage: vendor.sh <path to a microsoft/TypeScript checkout, or an older microsoft/typescript-go tree>
set -euo pipefail
src="${1:?path to microsoft/TypeScript checkout}"
[ -d "$src/tsc/internal" ] && src="$src/tsc"   # TypeScript repo layout; typescript-go had internal/ at the root
here="$(cd "$(dirname "$0")" && pwd)"
mod="github.com/howlerops/nisse/spikes/d16vendored"
rm -rf "$here/internal/tsgo"
mkdir -p "$here/internal/tsgo"
# Non-test Go sources plus embedded assets (lib.*.d.ts and friends); skip tests and testdata.
(cd "$src/internal" && find . -type f ! -name '*_test.go' ! -path '*/testdata/*' ! -path './fourslash/*' ! -path './testrunner/*' ! -path './testutil/*' ! -path './execute/tsctests/*' -print0 \
  | xargs -0 -I{} install -D {} "$here/internal/tsgo/{}")
find "$here/internal/tsgo" -name '*.go' -print0 | xargs -0 sed -i -e "s#github.com/microsoft/TypeScript/tsc/internal/#$mod/internal/tsgo/#g" -e "s#github.com/microsoft/typescript-go/internal/#$mod/internal/tsgo/#g"
cp "$src/LICENSE" "$here/internal/tsgo/LICENSE"
cp "$src/../NOTICE.txt" "$here/internal/tsgo/NOTICE.txt" 2>/dev/null || cp "$src/NOTICE.txt" "$here/internal/tsgo/NOTICE.txt" 2>/dev/null || true
# Same dependency versions as upstream.
awk '/^require \(/{p=1} p{print} /^\)/{if(p){p=0; print ""}}' "$src/go.mod" > "$here/.upstream-require"
go_line=$(grep '^go ' "$src/go.mod")
{ echo "module $mod"; echo; echo "$go_line"; echo; cat "$here/.upstream-require"; } > "$here/go.mod"
rm "$here/.upstream-require"
(cd "$here" && go mod tidy >/dev/null)
echo "vendored $(find "$here/internal/tsgo" -name '*.go' | wc -l) Go files ($(cat $(find "$here/internal/tsgo" -name '*.go') | wc -l) lines) from $(git -C "$src" rev-parse --short HEAD 2>/dev/null || echo "$src")"
