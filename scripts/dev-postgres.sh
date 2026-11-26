#!/usr/bin/env bash
# Make sure a local Postgres is running for development and tests, and print the URLs to export.
#
# For a development container that has Postgres 16 installed as a system package. It starts
# the default cluster if it is down and creates a throwaway superuser role named `nisse` with
# the password `nisse`. That role exists only on this machine's test server; it is not a
# secret and guards nothing. Never use it for a database that holds real rows.
#
# Usage: eval "$(scripts/dev-postgres.sh)"
set -euo pipefail

if ! pg_isready -q -h 127.0.0.1 -p 5432; then
  setsid pg_ctlcluster 16 main start >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    pg_isready -q -h 127.0.0.1 -p 5432 && break
    sleep 0.5
  done
fi
pg_isready -q -h 127.0.0.1 -p 5432 || { echo "dev-postgres: server did not start" >&2; exit 1; }

if ! PGPASSWORD=nisse psql -h 127.0.0.1 -U nisse -d postgres -Atqc 'select 1' >/dev/null 2>&1; then
  su postgres -c "psql -qc \"CREATE ROLE nisse LOGIN SUPERUSER PASSWORD 'nisse'\"" >/dev/null
fi

url='postgres://nisse:nisse@127.0.0.1:5432/postgres'
echo "export DATABASE_URL='$url'"
echo "export TEST_DATABASE_URL='$url'"
