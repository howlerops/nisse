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
# environment is read from $NISSE_ENV, default /etc/nisse/nisse.env, which only the engine user
# can read: run this as that user. Nothing from the file is printed.
set -euo pipefail

if (( $# < 1 )); then
  echo "usage: deploy/engine.sh <session-close|tick|status|kill|release-kill> [--by NAME]" >&2
  exit 2
fi
command="$1"
shift

cd "$(dirname "$0")/.."
env_file="${NISSE_ENV:-/etc/nisse/nisse.env}"
if [[ -r "$env_file" ]]; then
  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  set +a
else
  # Carry on: `kill` does whatever half it can with the variables already exported.
  echo "deploy/engine.sh: cannot read $env_file (run as the engine user); using the current environment" >&2
fi
exec target/release/nisse-engine "$command" "${NISSE_CONFIG:-config/engine/paper.json}" "$@"
