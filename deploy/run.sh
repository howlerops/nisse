#!/usr/bin/env bash
# Runs one scheduled command with the engine host's environment, appending its output to a log.
#
# Usage: deploy/run.sh <log name> <command> [args...]
#   deploy/run.sh tick target/release/nisse-engine tick config/engine/paper.json
#
# The environment (DATABASE_URL, the Alpaca paper keys, the holdout salt) is read from
# $NISSE_ENV, default /etc/nisse/nisse.env: a file readable by the engine user only, never in
# this repository. Nothing from it is printed. Exits with the command's status.
set -euo pipefail

if (( $# < 2 )); then
  echo "usage: deploy/run.sh <log name> <command> [args...]" >&2
  exit 2
fi
log_name="$1"
shift

cd "$(dirname "$0")/.."
# cron starts with a bare PATH; the replay script needs cargo, git and psql.
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
set -a
# shellcheck disable=SC1090
. "${NISSE_ENV:-/etc/nisse/nisse.env}"
set +a
mkdir -p log
exec "$@" >> "log/$log_name.log" 2>&1
