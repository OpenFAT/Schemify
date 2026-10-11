#!/usr/bin/env bash
# Nightly perf regression gate (D37 nightly tier, D39 budgets).
#
#   tests/perf/gate.sh <results> [<baseline>]     (baseline default: tests/perf/baseline.txt)
#
# <results> is the combined output of the `--ignored --release` perf tests; every gated metric
# is a `PERF <key> <ms>` line, and a key reported more than once (repeated runs) counts its
# fastest value. Fails (exit 1) when any key is more than 10% and more than
# 0.5 ms slower than its baseline (the absolute floor keeps microsecond metrics from flapping on
# timer noise; their own budget asserts guard them), or when a baseline key is missing from the
# results (a renamed or deleted metric updates the baseline in the same change). New keys are
# reported, not gated.
#
# The baseline is measured on the CI runner class that runs the gate (GitHub ubuntu-latest),
# never on a developer machine. Re-bless after an intended change: copy the `PERF` lines of
# the nightly `perf-results` artifact into baseline.txt.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
results="$1"
baseline="${2:-$here/baseline.txt}"

awk -v limit=1.10 -v floor_ms=0.5 '
  FNR == NR {
    if ($1 != "" && $1 !~ /^#/) { base[$1] = $2 }
    next
  }
  $1 == "PERF" && (!($2 in got) || $3 < got[$2]) { got[$2] = $3 }
  END {
    bad = 0
    printf "%-44s %12s %12s %8s\n", "metric", "baseline ms", "now ms", "ratio"
    for (k in got) {
      if (!(k in base)) { printf "%-44s %12s %12.3f %8s\n", k, "-", got[k], "new"; continue }
      r = got[k] / base[k]
      slow = r > limit && got[k] - base[k] > floor_ms
      flag = slow ? "  REGRESSION" : ""
      if (slow) bad = 1
      printf "%-44s %12.3f %12.3f %8.2f%s\n", k, base[k], got[k], r, flag
    }
    for (k in base) if (!(k in got)) { printf "%-44s %12.3f %12s   MISSING\n", k, base[k], "-"; bad = 1 }
    exit bad
  }
' "$baseline" "$results"
