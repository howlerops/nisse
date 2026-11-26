#!/usr/bin/env bash
# Start a Claude Code session with Fable as lead/reviewer and Haiku workers.
# Usage: scripts/handoff.sh [phase-number]   (default: lowest open phase)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

LEAD_MODEL="${LEAD_MODEL:-claude-fable-5-2}"
PHASE="${1:-}"

if [[ -n "$PHASE" ]]; then
  TARGET="Work on Phase ${PHASE} only."
else
  TARGET="Work on the lowest-numbered open phase issue."
fi

git fetch -q origin
git switch -q main
git pull -q --ff-only

exec claude --model "$LEAD_MODEL" \
  "$(cat docs/handoff/leader.md)

---
${TARGET} Follow the leader prompt above exactly. Delegate implementation to the haiku-worker subagent and use the fable-reviewer subagent before opening the PR."
