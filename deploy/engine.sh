#!/usr/bin/env bash
# Runs nisse-engine by hand on the engine host, with the host's environment and the output on
# the terminal.
#
# Usage: deploy/engine.sh <command> [args...]
#   deploy/engine.sh status
#   deploy/engine.sh kill --by ana
#   deploy/engine.sh release-kill --by ana
#
# The config file is config/engine/paper.json unless $NISSE_CONFIG names another. The
# environment is read from $NISSE_ENV, default /etc/nisse/nisse.env; nothing from it is printed.
set -euo pipefail

if (( $# < 1 )); then
  echo "usage: deploy/engine.sh <session-close|tick|status|kill|release-kill> [--by NAME]" >&2
  exit 2
fi
command="$1"
shift

cd "$(dirname "$0")/.."
set -a
# shellcheck disable=SC1090
. "${NISSE_ENV:-/etc/nisse/nisse.env}"
set +a
exec target/release/nisse-engine "$command" "${NISSE_CONFIG:-config/engine/paper.json}" "$@"
