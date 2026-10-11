#!/usr/bin/env bash
# Compare every golden netlist against ours and print the ratchet numbers.
#
#   tests/corpus/diff_all.sh <our-netlist-dir> [name-regex]
#
# <our-netlist-dir> holds files named like the goldens: <name>.spice (SPICE)
# and <name>.cdl (LVS). Per-file reports land in <our-netlist-dir>/diff/.
# Exit status: 0 when every golden agrees, 1 otherwise, 2 on usage errors.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
golden="$here/.cache/golden"
ours="${1:?usage: diff_all.sh <our-netlist-dir> [name-regex]}"
filter="${2:-}"
[[ -d "$golden" ]] || { echo "no goldens: run tests/corpus/fetch.sh && tests/corpus/golden.sh" >&2; exit 2; }
[[ -d "$ours" ]] || { echo "not a directory: $ours" >&2; exit 2; }

cargo build -q --release -p schemify-spicecmp --manifest-path "$repo/Cargo.toml"
cmp_bin="${CARGO_TARGET_DIR:-$repo/target}/release/spicecmp"
mkdir -p "$ours/diff"

declare -A agree differ missing
total_agree=0 total_differ=0 total_missing=0
for g in "$golden"/*.spice "$golden"/*.cdl; do
  file="$(basename "$g")"
  name="${file%.*}"
  [[ -n "$filter" && ! "$name" =~ $filter ]] && continue
  key="${name%%-*}/${file##*.}" # e.g. sky130/cdl
  if [[ ! -f "$ours/$file" ]]; then
    missing[$key]=$(( ${missing[$key]:-0} + 1 )); total_missing=$((total_missing + 1))
    echo "missing $file"
    continue
  fi
  # Goldens embed the absolute cache path of the checkout that made them; ours may
  # come from another checkout.
  theirs="$(grep -m1 -o '[^ ]*/tests/corpus/\.cache' "$g" || true)"
  extra=()
  [[ -n "$theirs" ]] && extra=(--alias "$theirs=\$CORPUS")
  if "$cmp_bin" --quiet --alias "$here/.cache=\$CORPUS" ${extra[@]+"${extra[@]}"} "$g" "$ours/$file" >"$ours/diff/$file.txt" 2>&1; then
    agree[$key]=$(( ${agree[$key]:-0} + 1 )); total_agree=$((total_agree + 1))
    rm -f "$ours/diff/$file.txt"
  else
    differ[$key]=$(( ${differ[$key]:-0} + 1 )); total_differ=$((total_differ + 1))
    echo "differ  $file  ($ours/diff/$file.txt)"
  fi
done

echo
printf '%-14s %6s %6s %8s\n' set agree differ missing
for key in $(printf '%s\n' "${!agree[@]}" "${!differ[@]}" "${!missing[@]}" | sort -u); do
  printf '%-14s %6d %6d %8d\n' "$key" "${agree[$key]:-0}" "${differ[$key]:-0}" "${missing[$key]:-0}"
done
printf '%-14s %6d %6d %8d\n' total "$total_agree" "$total_differ" "$total_missing"
echo "ratchet: $total_agree agree of $((total_agree + total_differ + total_missing))"
(( total_differ == 0 && total_missing == 0 ))
