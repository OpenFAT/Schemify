#!/usr/bin/env bash
# The xschem-differential ratchet (RATCHET.md: "the count may only go up"), run by nightly CI.
#
#   tests/corpus/ratchet.sh <TABLE.md> [<RATCHET.md>]
#
# <TABLE.md> is what `crates/netlist/tests/corpus.rs` (xschem_differential) writes into
# `.cache/netlist-out/`. Both files hold the same per-set table (`| set | mode | agree | … |`):
# the gated numbers are the SPICE agree count (column 3 of the `spice` rows) and the CDL agree
# count against the code-stripped golden (column 7 of the `cdl` rows), summed over sets.
# Exit 1 when either is below the recorded value; a higher value asks for RATCHET.md to be
# raised in the change that earned it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
table="$1"
recorded="${2:-$here/RATCHET.md}"

# Prints "<spice agree> <cdl stripped agree>" of a table file.
counts() {
  awk -F'|' '
    { for (i = 2; i <= NF; i++) { gsub(/[ *]/, "", $i) } }
    $3 == "spice" { spice += $4; rows++ }
    $3 == "cdl" { cdl += $8; rows++ }
    END { if (rows == 0) exit 2; print spice + 0, cdl + 0 }
  ' "$1"
}

read -r rec_spice rec_cdl < <(counts "$recorded")
read -r now_spice now_cdl < <(counts "$table")
echo "SPICE agree: $now_spice (recorded $rec_spice)"
echo "CDL agree, golden code stripped: $now_cdl (recorded $rec_cdl)"
status=0
(( now_spice >= rec_spice )) || { echo "::error::SPICE agree dropped: $now_spice < $rec_spice"; status=1; }
(( now_cdl >= rec_cdl )) || { echo "::error::CDL agree dropped: $now_cdl < $rec_cdl"; status=1; }
if (( status == 0 )) && (( now_spice > rec_spice || now_cdl > rec_cdl )); then
  echo "::notice::the ratchet went up: record the new numbers in tests/corpus/RATCHET.md"
fi
exit $status
