#!/usr/bin/env bash
# Apply migrations/*.sql, in filename order, to the Postgres at $DATABASE_URL.
#
# Idempotent: each file runs once, in one transaction with its schema_migration row, so a
# failed file leaves nothing behind and a rerun picks up where it stopped. Migrations are
# forward-only. An applied file must never be edited (its checksum is verified on every run);
# fix a mistake with a new migration.
#
# Usage: DATABASE_URL=postgres://user@host/db scripts/migrate.sh
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL}"
cd "$(dirname "$0")/.."

# NOTICEs (such as "relation already exists, skipping") are noise; errors still print.
export PGOPTIONS="${PGOPTIONS:-} -c client_min_messages=warning"
q() { psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 "$@"; }

q -c "CREATE TABLE IF NOT EXISTS schema_migration (
        version   TEXT PRIMARY KEY,
        checksum  TEXT NOT NULL,
        event_ts  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        ingest_ts TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
      )"

for file in migrations/*.sql; do
  version="$(basename "$file" .sql)"
  if [[ ! "$version" =~ ^[0-9]{4}_[a-z0-9_]+$ ]]; then
    echo "migrate: bad migration name: $file (want NNNN_snake_case.sql)" >&2
    exit 1
  fi
  checksum="$(sha256sum "$file" | cut -d' ' -f1)"
  applied="$(q -At -c "SELECT checksum FROM schema_migration WHERE version = '$version'")"
  if [[ -z "$applied" ]]; then
    echo "migrate: applying $version"
    # One session so the advisory lock serialises concurrent runners; the INSERT's primary
    # key makes a lost race fail the whole transaction instead of applying a file twice.
    q -1 -c "SELECT pg_advisory_xact_lock(hashtext('nisse.schema_migration'))" \
         -f "$file" \
         -c "INSERT INTO schema_migration (version, checksum) VALUES ('$version', '$checksum')" \
         >/dev/null
  elif [[ "$applied" != "$checksum" ]]; then
    echo "migrate: $version was edited after it was applied; add a new migration instead" >&2
    exit 1
  fi
done
# A ledger row with no file means a migration was deleted or renamed after it ran.
while read -r version; do
  if [[ -n "$version" && ! -f "migrations/$version.sql" ]]; then
    echo "migrate: $version is applied in the database but migrations/$version.sql is missing" >&2
    exit 1
  fi
done < <(q -At -c "SELECT version FROM schema_migration ORDER BY version")

echo "migrate: up to date"
