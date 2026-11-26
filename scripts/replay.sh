#!/usr/bin/env bash
# Replay the nightly decision check for the build at HEAD, and say which other builds have
# decisions that this checkout cannot replay.
#
# The binary replays only decisions whose strategy version carries its own git SHA. For every
# other SHA that has decisions (a version with none has nothing to replay) this script prints a notice and cannot replay it: building
# those commits and running the replay from each checkout is deployment work for a later phase
# (Phase 6). Until then a skipped SHA fails the run, because "replay passed" must never mean
# "nothing was replayed".
#
# Exit status: the replay binary's when it is non-zero; 3 when the binary was clean but some
# build's decisions could not be replayed from this checkout; else 0.
#
# Usage: DATABASE_URL=postgres://user@host/db scripts/replay.sh
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL}"
cd "$(dirname "$0")/.."

head="$(git rev-parse HEAD)"
skipped=0

# Captured first so a failing psql stops the script (process substitution would hide it).
shas="$(psql "$DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 \
  -c "SELECT DISTINCT v.git_sha FROM strategy_version v
      JOIN decision d ON d.strategy_version_id = v.id ORDER BY 1")"
while IFS= read -r sha; do
  [[ -z "$sha" || "$sha" == "$head" ]] && continue
  echo "replay: decisions exist for build $sha; replay them from a checkout of that commit" >&2
  skipped=1
done <<< "$shas"

status=0
cargo run --quiet --locked -p nisse-eval --bin nisse-replay || status=$?

if (( status != 0 )); then
  exit "$status"
fi
if (( skipped )); then
  echo "replay: clean for $head, but other builds' decisions were not replayed (see above)" >&2
  exit 3
fi
exit 0
