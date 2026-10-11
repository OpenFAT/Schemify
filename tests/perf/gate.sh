#!/usr/bin/env bash
# Nightly perf regression gate (D37 nightly tier, D39 budgets).
#
#   tests/perf/gate.sh <results> [<baseline>]     (baseline default: tests/perf/baseline.txt)
#
# <results> is the combined output of N rounds of the `--ignored --release` perf tests; every
# gated metric is a `PERF <key> <ms>` line. Each test already reports the best of its own
# repetitions; a key reported by several rounds counts the median of its values (best-of-k
# medians: one slow or lucky round moves nothing; with two rounds, their mean). Fails (exit 1)
# when any key is more than 10% and more than 0.5 ms slower than its baseline (the absolute
# floor keeps microsecond metrics from flapping on timer noise; their own budget asserts guard
# them), or when a baseline key is missing from the results (a renamed or deleted metric
# updates the baseline in the same change). New keys are reported, not gated.
#
# The baseline is measured on the CI runner class that runs the gate (GitHub ubuntu-latest),
# never on a developer machine. Re-bless after an intended change: take the medians this gate
# prints for the nightly `perf-results` artifact into baseline.txt.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
results="$1"
baseline="${2:-$here/baseline.txt}"

# POSIX awk (mawk on the runners has no asort): values per key as a space-separated list,
# sorted by insertion when the median is taken (a handful of rounds per key).
awk -v limit=1.10 -v floor_ms=0.5 '
  function median(list,    v, n, i, j, t) {
    n = split(list, v, " ")
    for (i = 2; i <= n; i++) {
      t = v[i] + 0
      for (j = i - 1; j >= 1 && v[j] + 0 > t; j--) v[j + 1] = v[j]
      v[j + 1] = t
    }
    return n % 2 ? v[(n + 1) / 2] + 0 : (v[n / 2] + v[n / 2 + 1]) / 2
  }
  FNR == NR {
    if ($1 != "" && $1 !~ /^#/) { base[$1] = $2 }
    next
  }
  $1 == "PERF" { vals[$2] = ($2 in vals) ? vals[$2] " " $3 : $3; runs[$2]++ }
  END {
    bad = 0
    printf "%-44s %12s %12s %8s %6s\n", "metric", "baseline ms", "median ms", "ratio", "runs"
    for (k in vals) {
      got = median(vals[k])
      if (!(k in base)) { printf "%-44s %12s %12.3f %8s %6d\n", k, "-", got, "new", runs[k]; continue }
      r = got / base[k]
      slow = r > limit && got - base[k] > floor_ms
      flag = slow ? "  REGRESSION" : ""
      if (slow) bad = 1
      printf "%-44s %12.3f %12.3f %8.2f %6d%s\n", k, base[k], got, r, runs[k], flag
    }
    for (k in base) if (!(k in vals)) { printf "%-44s %12.3f %12s   MISSING\n", k, base[k], "-"; bad = 1 }
    exit bad
  }
' "$baseline" "$results"
