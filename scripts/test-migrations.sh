#!/usr/bin/env bash
# Phase 0 exit check: migrations apply cleanly to a fresh database, a second run is a no-op,
# and every schema test in migrations/tests/*.sql passes.
#
# Creates and drops a scratch database next to the one in $DATABASE_URL, so the role needs
# CREATEDB. Never point this at a database holding real rows.
#
# Usage: DATABASE_URL=postgres://user@host/postgres scripts/test-migrations.sh
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL}"
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

scratch="nisse_test_$$_$RANDOM"
admin_url="$DATABASE_URL"
admin() { psql "$admin_url" -X -q -v ON_ERROR_STOP=1 "$@"; }
admin -c "CREATE DATABASE $scratch"
trap 'admin -c "DROP DATABASE IF EXISTS $scratch WITH (FORCE)"' EXIT

# Same server and credentials, scratch database (the path is the part after the last slash).
base="${DATABASE_URL%%\?*}"
query="${DATABASE_URL#"$base"}"
export DATABASE_URL="${base%/*}/$scratch$query"

scripts/migrate.sh
if scripts/migrate.sh | grep -q 'applying'; then
  echo "test-migrations: second run applied something; migrate.sh is not idempotent" >&2
  exit 1
fi

for test in migrations/tests/*.sql; do
  echo "test-migrations: $test"
  # Each test runs in a transaction that is rolled back, so tests cannot see each other's rows.
  psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
    -c 'BEGIN' -f migrations/tests/support/helpers.sql -f "$test" -c 'ROLLBACK' >/dev/null
done
echo "test-migrations: ok"
