#!/usr/bin/env bash
# Determinism oracle (D32, AGENTS.md rule 4): writes the CLI outputs that must be byte-identical
# on every platform. CI runs it on each OS (and the WASI build under wasmtime) and diffs the
# trees (`.github/workflows/ci.yml`, job `determinism`).
#
#   tests/determinism/run.sh '<schemify command>' '<schemify-interop command>' <out-dir>
#
# `schemify-interop` is the interop plugin's native CLI (D24 as amended: the importer is not in
# the core), built from the plugins workspace:
#   cargo build --manifest-path plugins/Cargo.toml -p interop --features cli --bin schemify-interop
# Either command may carry a runner prefix ('wasmtime run --dir=. schemify.wasm'). Every path
# it receives is relative to the repository root, the working directory of each call. Writes:
#   netlist/<fixture>.<dialect>   `schemify netlist` of every .shcomp/.shbench fixture of the
#                                 app/elab/netlist tests, in every dialect; `.status` = exit code
#   fmt/<fixture>                 `schemify fmt` (canonical rewrite) of every design fixture
#   import/                       `schemify-interop xschem-import` of tests/determinism/xschem
#                                 (hand-written)
# `*.log` files hold stderr for debugging and are not compared (messages may spell paths
# natively).
set -euo pipefail

cmd="$1"
interop="$2"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
mkdir -p "$3"
out="$(cd "$3" && pwd)"
# Outputs go to a directory under the repository so a WASI runner's `--dir=.` reaches them.
rel=".determinism-out"
cd "$repo"
rm -rf "$rel"
mkdir -p "$rel/netlist" "$rel/fmt"
export LC_ALL=C

run() { # run <log> <status-file> <command> args...
  local log="$1" status="$2" program="$3"
  shift 3
  set +e
  $program "$@" 2>"$log" >>"$log"
  echo "$?" >"$status"
  set -e
}

fixtures=(crates/app/tests/fixtures crates/elab/tests/fixtures crates/netlist/tests/fixtures)
for f in $(find "${fixtures[@]}" -type f \( -name '*.shcomp' -o -name '*.shbench' \) | sort); do
  name="${f//\//__}"
  for d in ngspice xyce cdl spectre verilog; do
    run "$rel/netlist/$name.$d.log" "$rel/netlist/$name.$d.status" "$cmd" \
      netlist "$f" --dialect "$d" --allow-errors -o "$rel/netlist/$name.$d"
  done
done

files=()
for f in $(find "${fixtures[@]}" -type f \( -name '*.shprim' -o -name '*.shcomp' -o -name '*.shbench' \) | sort); do
  cp "$f" "$rel/fmt/${f//\//__}"
  files+=("$rel/fmt/${f//\//__}")
done
run "$rel/fmt.log" "$rel/fmt.status" "$cmd" fmt "${files[@]}"

x="tests/determinism/xschem"
run "$rel/import.log" "$rel/import.status" "$interop" xschem-import "$x/devices" "$x/lib" --root "$x" -o "$rel/import"

rm -rf "${out:?}"/*
cp -R "$rel"/. "$out"/
rm -rf "$rel"
echo "determinism outputs: $(find "$out" -type f ! -name '*.log' | wc -l) files in $out"
