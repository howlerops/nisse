#!/usr/bin/env bash
# Shallow-clone every candidate in repos.txt into $1 (default: ./clones).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dest=${1:-$here/clones}
mkdir -p "$dest"
grep -v '^#' "$here/repos.txt" | while read -r tier repo; do
  dir="$dest/${repo/\//_}"
  [ -d "$dir" ] || git clone -q --depth 1 "https://github.com/$repo" "$dir" &
done
wait
