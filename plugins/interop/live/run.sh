#!/usr/bin/env bash
# Live Virtuoso test kit for the interop plugin: one command on a machine with Cadence
# Virtuoso and this repository's Rust toolchain. See README.md next to this file.
#
#   plugins/interop/live/run.sh [--cdslib FILE] [--work DIR] [--cell LIB/CELL]...
#       [--pdk LIB[/CELL]] [--callbacks] [--retarget MAP] [--step STEP]
#
# Steps, in order (--step runs one; the default runs all):
#   prepare   Rust: exports and session1.il                    (needs cargo)
#   session1  Virtuoso: probe, loads, dumps, refusals          (needs virtuoso)
#   tools     si netlists and OCEAN runs of the loaded cells   (needs si, ocean, spectre; skipped if absent)
#   mid       Rust: imports of the dumps, edits, session2.il   (needs cargo)
#   session2  Virtuoso: the teamwork round and your cells back (needs virtuoso)
#   verify    Rust: grades everything into <work>/out/report.md (needs cargo)
# Exit status: 0 when no check failed, 1 when one did, 2 on a usage error.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
work="$PWD/sfylive"
cdslib="$PWD/cds.lib"
step=all
cells=()
pdk=""
target=""
callbacks=0
virtuoso="${SFYLIVE_VIRTUOSO:-virtuoso -nograph -restore}"
timeout_s="${SFYLIVE_TIMEOUT:-3600}"

die() { echo "run.sh: $*" >&2; exit 2; }
while (($#)); do
  case "$1" in
    --work) work="$2"; shift 2 ;;
    --cdslib) cdslib="$2"; shift 2 ;;
    --cell) cells+=("$2"); shift 2 ;;
    --pdk) pdk="$2"; shift 2 ;;
    --callbacks) callbacks=1; shift ;;
    --retarget) target="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"; shift 2 ;;
    --step) step="$2"; shift 2 ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done
[[ -f "$cdslib" ]] || die "no cds.lib at $cdslib: run from your Virtuoso project directory or pass --cdslib"
cdslib="$(cd "$(dirname "$cdslib")" && pwd)/$(basename "$cdslib")"
mkdir -p "$work/out" "$work/libs"
work="$(cd "$work" && pwd)"
out="$work/out"

# The Rust steps: one ignored test of the interop crate.
host() {
  command -v cargo >/dev/null || die "cargo not found: the $1 step needs the Rust toolchain (README.md: Requirements)"
  SCHEMIFY_LIVE_DIR="$work" SCHEMIFY_LIVE_STEP="$1" \
  SCHEMIFY_LIVE_CELLS="${cells[*]-}" SCHEMIFY_LIVE_PDK="$pdk" \
  SCHEMIFY_LIVE_TARGET="$target" SCHEMIFY_LIVE_CALLBACKS="$callbacks" \
    cargo test --quiet --manifest-path "$repo/plugins/Cargo.toml" -p interop \
      --test virtuoso_live live_step -- --ignored --exact --nocapture
}

# work/cds.lib: the user's libraries, then every library the kit created.
cds() {
  [[ -f "$work/cds.lib" ]] || printf 'SOFTINCLUDE %s\n' "$cdslib" > "$work/cds.lib"
  [[ -f "$work/libs.txt" ]] || return 0
  while read -r lib; do
    [[ -d "$work/libs/$lib" ]] || continue
    grep -q "^DEFINE $lib " "$work/cds.lib" || printf 'DEFINE %s %s\n' "$lib" "$work/libs/$lib" >> "$work/cds.lib"
  done < "$work/libs.txt"
}

session() {
  command -v "${virtuoso%% *}" >/dev/null || die "${virtuoso%% *} not found: put Virtuoso on PATH or set SFYLIVE_VIRTUOSO"
  cds
  echo "run.sh: session $1 (Virtuoso log: $out/CDS.session$1.log)"
  (cd "$work" && timeout "$timeout_s" $virtuoso "session$1.il" -log "$out/CDS.session$1.log" </dev/null)
  echo "$?" > "$out/session$1.status"
  cds
}

# si netlists of the lines of netlists.txt (lib cell simulator).
netlists() {
  local nl="$out/netlist"
  rm -rf "$nl"; mkdir -p "$nl"
  if ! command -v si >/dev/null; then echo "si not on PATH" > "$nl/SKIPPED"; return; fi
  while read -r lib cell sim; do
    local d="$work/si/$lib.$cell.$sim" key="$lib.$cell.$sim" views stop
    rm -rf "$d"; mkdir -p "$d"
    printf 'INCLUDE %s\n' "$work/cds.lib" > "$d/cds.lib"
    if [[ $sim == auCdl ]]; then
      views="'(\"auCdl\" \"schematic\")"; stop="'(\"auCdl\")"
    else
      views="'(\"spectre\" \"cmos_sch\" \"cmos.sch\" \"schematic\" \"veriloga\")"; stop="'(\"spectre\")"
    fi
    cat > "$d/si.env" <<EOF
simLibName = "$lib"
simCellName = "$cell"
simViewName = "schematic"
simSimulator = "$sim"
simNotIncremental = 't
simReNetlistAll = 't
simViewList = $views
simStopList = $stop
simNetlistHier = 't
hnlNetlistFileName = "netlist"
EOF
    (cd "$d" && timeout 600 si -batch -command nl > "$nl/$key.log" 2>&1 </dev/null)
    echo "$?" > "$nl/$key.status"
    local f
    f="$(cd "$d" && ls netlist input.scs *.cdl 2>/dev/null | head -1)"
    [[ -n "$f" ]] && cp "$d/$f" "$nl/$key.net"
  done < "$work/netlists.txt"
}

# OCEAN runs of the exported scripts of the lines of oceans.txt.
oceans() {
  local oc="$out/ocean"
  rm -rf "$oc" "$work/ocean"; mkdir -p "$oc"
  if ! command -v ocean >/dev/null; then echo "ocean not on PATH" > "$oc/SKIPPED"; return; fi
  while read -r d; do
    local w="$work/ocean/$d" status=0
    mkdir -p "$w"
    cp -r "$work/in/ocean/$d/." "$w/"
    printf 'INCLUDE %s\n' "$work/cds.lib" > "$w/cds.lib"
    : > "$oc/$d.log"
    for s in "$w"/*.ocn; do
      echo "== $(basename "$s")" >> "$oc/$d.log"
      (cd "$w" && timeout 1800 ocean -nograph < "$s" >> "$oc/$d.log" 2>&1) || status=$?
    done
    echo "$status" > "$oc/$d.status"
  done < "$work/oceans.txt"
}

run() {
  case "$1" in
    prepare) host prepare || die "prepare failed" ;;
    session1) session 1 ;;
    tools) cds; netlists; oceans ;;
    mid) host mid || die "mid failed" ;;
    session2) session 2 ;;
    verify)
      host verify; local s=$?
      mkdir -p "$out/load-logs"
      cp "$work"/in/*.log "$out/load-logs/" 2>/dev/null
      tar czf "$work/sfylive-results.tgz" -C "$work" out session1.il session2.il options.txt
      echo "run.sh: report $out/report.md; send back $work/sfylive-results.tgz"
      return $((s == 0 ? 0 : 1)) ;;
    *) die "unknown step $1" ;;
  esac
}

if [[ $step == all ]]; then
  for s in prepare session1 tools mid session2; do run "$s"; done
  run verify
else
  run "$step"
fi
