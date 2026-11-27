#!/usr/bin/env bash
# Evaluates one research config where the bar files and the store are: runs the backtest
# (which registers a trial; the trial count never resets) and prints the gate report for the
# resulting strategy version. Run it once per proposal, after reading the proposal's note:
# every run spends one trial of the lifetime budget.
#
# The run covers ordinary history only: it ends the day before the holdout starts
# (`holdout_start` in the gates file). A look at the holdout is a separate, deliberate
# `nisse-backtest --holdout`.
#
# Usage: DATABASE_URL=... scripts/evaluate-proposal.sh research/configs/<name>.json
# Optional environment: START (default 2015-01-01), STARTING_CASH (100000),
#   SLIPPAGE_BPS (2), FEE_PER_SHARE (0.0001), BARS_DIR (data/bars),
#   GATES (config/eval/gates.json), LIMITS (config/limits/limits.json).
# Needs jq. Exit status: the backtest's when it fails, else the gate report's
# (0 every gate passes, 1 a gate fails, 3 or 4 as nisse-gate-report documents).
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL}"
if (( $# != 1 )); then
  echo "usage: scripts/evaluate-proposal.sh research/configs/<name>.json" >&2
  exit 2
fi
config="$1"
cd "$(dirname "$0")/.."
[[ -r "$config" ]] || { echo "evaluate-proposal: cannot read $config" >&2; exit 2; }
command -v jq >/dev/null || { echo "evaluate-proposal: jq is not installed" >&2; exit 2; }

gates="${GATES:-config/eval/gates.json}"
holdout_start="$(jq -er '.holdout_start' "$gates")"
# The last ordinary day: the day before the holdout starts.
end="$(date -u -d "$holdout_start - 1 day" +%Y-%m-%d)"

job="$(mktemp)"
trap 'rm -f "$job"' EXIT
jq -e \
  --arg start "${START:-2015-01-01}" --arg end "$end" \
  --arg cash "${STARTING_CASH:-100000}" \
  --arg slippage "${SLIPPAGE_BPS:-2}" --arg fee "${FEE_PER_SHARE:-0.0001}" \
  --arg bars "${BARS_DIR:-data/bars}" --arg gates "$gates" \
  --arg limits "${LIMITS:-config/limits/limits.json}" \
  '{strategy: .strategy, symbols: .symbols, start: $start, end: $end,
    starting_cash: $cash, costs: {slippage_bps: $slippage, fee_per_share: $fee},
    bars_dir: $bars, limits_path: $limits, gates_path: $gates, seed: 0}' \
  "$config" > "$job"

echo "evaluate-proposal: backtest of $config, ${START:-2015-01-01} to $end (registers a trial)"
out="$(cargo run --quiet --locked --release -p nisse-eval --bin nisse-backtest -- "$job")"
echo "$out"
version="$(sed -n 's/^strategy_version: //p' <<<"$out")"
[[ -n "$version" ]] || { echo "evaluate-proposal: no strategy version in the output" >&2; exit 1; }

echo "evaluate-proposal: gate report for strategy version $version"
cargo run --quiet --locked --release -p nisse-eval --bin nisse-gate-report -- "$version" "$gates"
