#!/usr/bin/env bash
# One command for a live run: starts clean, runs run.sh with the flags you give it, and
# collects everything worth sending back into <work>/summary.txt (also printed at the end).
#
#   cd <your Virtuoso project directory>        # the one with cds.lib
#   /path/to/Schemify/plugins/interop/live/test.sh [run.sh flags, e.g. --lib sample]
#
# It deletes the previous work directory (default ./sfylive) and this user's /tmp/sfy_* files.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$PWD/sfylive"
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  [[ ${args[i]} == --work ]] && work="${args[i + 1]:-$work}"
done

rm -rf "$work"
rm -f /tmp/sfy_selftest* /tmp/sfy_probe.txt 2>/dev/null

"$here/run.sh" "$@" 2>&1 | tee "$PWD/sfylive-run.log"
status=${PIPESTATUS[0]}

out="$work/out"
summary="$work/summary.txt"
mkdir -p "$work"
{
  echo "== run.sh exit $status, flags: $*"
  echo
  echo "== report"
  cat "$out/report.md" 2>/dev/null || echo "(no report: see sfylive-run.log)"
  echo
  echo "== refusals and SKILL errors (count, line)"
  grep -h -e refused -e 'SKILL error' "$work"/in/*.log "$out"/*.sfyx.log 2>/dev/null \
    | sort | uniq -c | sort -rn | head -40
  echo
  echo "== si errors"
  grep -h -i -e error -e '\*E' "$out"/netlist/*.log 2>/dev/null | sort -u | head -20
  for n in 1 2; do
    if ! grep -q "INFO session end" "$out/session$n.txt" 2>/dev/null; then
      echo
      echo "== session $n did not finish: last lines of its Virtuoso log"
      tail -30 "$out/CDS.session$n.log" 2>/dev/null || echo "(no log)"
    fi
  done
} > "$summary"

echo
echo "================================================================"
cat "$summary"
echo "================================================================"
echo "test.sh: paste or send $summary (full run output: $PWD/sfylive-run.log)"
exit "$status"
