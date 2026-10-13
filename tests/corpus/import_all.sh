#!/usr/bin/env bash
# Import every corpus library group with `schemify-interop xschem-import` (the interop
# plugin's native CLI, plugins/interop; spec docs/spec/import.md),
# so the netlister can be compared against the xschem goldens.
#
#   tests/corpus/import_all.sh [out-dir]          (default: a fresh mktemp directory)
#   CORPUS_CACHE=<dir> overrides tests/corpus/.cache (e.g. from another worktree).
#
# For each rcfile group of golden.sh (xschem, sky130, gf180, ihp) it converts the
# group's xschem search path into <out>/<group>/ (one Schemify library per xschem
# library directory, schemify.project, import-report.{md,txt}) and checks every output
# with the import oracle (schemify-interop exits 1 if any output fails to load).
#
# It then writes <out>/benches.map, one line per benches.txt entry:
#   <bench-name> <group> <cell-file relative to <out>/<group>> <PDK_ROOT> <PDK>
# The cell file is the bench copy <stem>__top.shbench when the schematic has a symbol
# and netlist text, else <stem>.shbench, else the component <stem>.shcomp.
#
# Then each mapped bench is netlisted (SPICE: the cell above; CDL: the component
# <stem>.shcomp first, as LVS checks the cell) and compared with diff_all.sh. NETLIST is
# the command template, default the CLI built from this checkout:
#   NETLIST='schemify netlist --dialect %dialect% -o %out% --allow-errors %cell%'
# %cell% is the absolute cell path, %dialect% is ngspice or cdl, %out% the target file
# (<out>/netlists/<bench>.spice|.cdl), run with PDK_ROOT/PDK/HOME exported as golden.sh
# did. NETLIST=none stops after the map. The authoritative ratchet (golden code blocks
# stripped for CDL, golden-unevaluated parameters) is
# plugins/interop/tests/xschem_differential.rs.
# Exit status: 0 ok, 1 an import check failed, 2 usage or setup error.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
cache="${CORPUS_CACHE:-$here/.cache}"
src="$cache/src"
out="${1:-$(mktemp -d)}"
[[ -d "$src" ]] || { echo "no corpus: run tests/corpus/fetch.sh" >&2; exit 2; }
mkdir -p "$out"

cargo build -q --release --manifest-path "$repo/plugins/Cargo.toml" -p interop --features cli \
  --bin schemify-interop
dr="${CARGO_TARGET_DIR:-$repo/plugins/target}/release/schemify-interop"

lib="$src/xschem/xschem_library"
ihp="$src/ihp_open_pdk/ihp-sg13g2"
status=0
run() { # group, then xschem-import arguments
  local g=$1; shift
  rm -rf "${out:?}/$g"
  echo "== $g"
  if ! "$dr" xschem-import "$@" -o "$out/$g" | grep -v '^  limit'; then status=1; fi
}

run xschem $(for d in devices examples ngspice ngspice_verilog_cosim logic xschem_simulator \
  generators inst_sch_select binto7seg pcb rom8k; do echo "$lib/$d"; done)
run sky130 "$lib/devices" --root "$lib" "$src/xschem_sky130" \
  --xschemrc "$src/xschem_sky130/xschemrc"
run gf180 "$lib/devices" --root "$lib" "$src/gf180mcu_fd_pr/cells/xschem" \
  --xschemrc "$src/gf180mcu_fd_pr/cells/xschem/xschemrc"
run ihp "$lib/devices" --root "$lib" "$ihp/libs.tech/xschem" --root "$ihp/libs.tech/xschem/sg13g2_pr" \
  "$ihp/libs.ref/sg13g2_stdcell/sym/xschem" "$ihp/libs.ref/sg13g2_stdcell/sch/xschem" \
  --xschemrc "$ihp/libs.tech/xschem/xschemrc" --xschemrc "$ihp/libs.tech/xschem/xschem-drc"

# Bench → cell map (same PDK_ROOT/PDK as golden.sh's rcfiles).
map="$out/benches.map"
: > "$map"
while read -r name rc path; do
  [[ -z "$name" || "$name" == \#* ]] && continue
  stem="$(basename "$path" .sch)"
  dir="$(basename "$(dirname "$path")")"
  case $rc in
    sky130) root="$cache/pdk"; pdk=sky130A ;;
    gf180) root="$src/gf180mcu_fd_pr"; pdk=- ;;
    ihp) root="$src/ihp_open_pdk"; pdk=ihp-sg13g2 ;;
    *) root=-; pdk=- ;;
  esac
  cell=""
  for want in "${stem}__top.shbench" "${stem}.shbench" "${stem}.shcomp"; do
    hits="$(cd "$out/$rc" && ls -1 -- */"$want" 2>/dev/null || true)"
    [[ -z "$hits" ]] && continue
    cell="$(grep -m1 "^$dir/" <<<"$hits" || head -n1 <<<"$hits")"
    break
  done
  if [[ -z "$cell" ]]; then
    echo "unmapped $name ($path)" >&2
    status=1
    continue
  fi
  echo "$name $rc $cell $root $pdk" >> "$map"
done < "$here/benches.txt"
echo "wrote $map ($(wc -l < "$map") benches)"

if [[ -z "${NETLIST:-}" ]]; then
  cargo build -q --release -p schemify-app --manifest-path "$repo/Cargo.toml"
  cli="${CARGO_TARGET_DIR:-$repo/target}/release/schemify"
  NETLIST="$cli netlist --dialect %dialect% -o %out% --allow-errors %cell%"
fi
if [[ "$NETLIST" != none ]]; then
  mkdir -p "$out/netlists"
  while read -r name rc cell root pdk; do
    for mode in spice cdl; do
      dialect=ngspice; target="$cell"
      if [[ $mode == cdl ]]; then
        dialect=cdl
        comp="${cell%__top.shbench}"; comp="${comp%.shbench}"; comp="${comp%.shcomp}.shcomp"
        [[ -f "$out/$rc/$comp" ]] && target="$comp"
      fi
      cmd="${NETLIST//%cell%/$out/$rc/$target}"
      cmd="${cmd//%dialect%/$dialect}"
      cmd="${cmd//%out%/$out/netlists/$name.$mode}"
      ( [[ $root != - ]] && export PDK_ROOT="$root"; [[ $pdk != - ]] && export PDK="$pdk"
        export HOME="$cache/home"
        cd "$out/$rc" && eval "$cmd" ) || echo "netlist failed: $name $mode" >&2
    done
  done < "$map"
  "$here/diff_all.sh" "$out/netlists" || true
fi
exit $status
