#!/usr/bin/env bash
# Nightly coverage ratchet (docs/dev/testing.md nightly tier; coverage is a gap-finder, not a goal).
#
#   tests/coverage/ratchet.sh <coverage.json> [<floor>]     (floor default: tests/coverage/floor.txt)
#
# <coverage.json> is `cargo llvm-cov --workspace --json --summary-only`. Fails (exit 1) when
# the workspace line coverage is more than 0.5 points below the floor (proptest seeds vary
# per run, so a few lines come and go). Prints a raise hint when coverage is a point or more
# above it: raise the floor in the change that adds the tests, never lower it to pass.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
json="$1"
floor_file="${2:-$here/floor.txt}"

now="$(jq -r '.data[0].totals.lines.percent' "$json")"
floor="$(grep -v '^#' "$floor_file" | head -n 1)"
lines="$(jq -r '.data[0].totals.lines | "\(.covered)/\(.count)"' "$json")"
echo "workspace line coverage: $now% ($lines lines), floor $floor%"
awk -v now="$now" -v floor="$floor" 'BEGIN {
  if (now + 0.5 < floor) { printf "coverage %.2f%% is below the floor %.2f%%\n", now, floor; exit 1 }
  if (now >= floor + 1) printf "coverage %.2f%%: raise tests/coverage/floor.txt to %.1f\n", now, int(now * 10) / 10
}'
