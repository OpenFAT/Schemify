#!/usr/bin/env bash
# Fetch third-party corpus inputs at pinned commits into tests/corpus/.cache/src/.
# Idempotent: a repo already at its pin with the same sparse paths is skipped.
# Shallow (depth 1), blob-filtered, sparse: only the listed paths are downloaded.
# Nothing fetched here is committed (.cache/ is gitignored); xschem data is GPL.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$here/.cache/src"
mkdir -p "$src"

# name | url | commit | sparse paths (space separated, no-cone patterns)
# xschem is pinned to tag 3.4.7 to match the nix-built binary golden.sh runs.
repos=(
  "xschem|https://github.com/StefanSchippers/xschem.git|92dd8fe5f4d5c1057489710d8a22f18fdc9d7ed0|/xschem_library/ /LICENSE"
  "xschem_sky130|https://github.com/StefanSchippers/xschem_sky130.git|fef70fad3804d1c298c70f78de769825d7dfea19|/sky130_fd_pr/ /sky130_stdcells/ /sky130_tests/ /stdcells/ /xschemrc /LICENSE"
  "gf180mcu_fd_pr|https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_pr.git|e11a8c98dfc905561d38813b1416e65a4b9280e9|/cells/xschem/ /models/ngspice/ /LICENSE"
  "ihp_open_pdk|https://github.com/IHP-GmbH/IHP-Open-PDK.git|5e6d592e4002946a4616f798c357f0f3c06cf3b6|/ihp-sg13g2/libs.tech/xschem/ /ihp-sg13g2/libs.tech/ngspice/ /ihp-sg13g2/libs.tech/verilog-a/ /ihp-sg13g2/libs.ref/sg13g2_stdcell/sym/ /ihp-sg13g2/libs.ref/sg13g2_stdcell/sch/ /ihp-sg13g2/libs.ref/sg13g2_stdcell/spice/ /ihp-sg13g2/libs.ref/sg13g2_stdcell/cdl/ /ihp-sg13g2/libs.ref/sg13g2_stdcell/verilog/ /ihp-sg13g2/libs.ref/sg13g2_io/spice/ /ihp-sg13g2/libs.ref/sg13g2_io/cdl/ /LICENSE"
)

fetch_one() {
  local name="$1" url="$2" rev="$3" paths="$4" dir="$src/$1"
  if [[ -d "$dir/.git" ]] \
     && [[ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" == "$rev" ]] \
     && [[ "$(git -C "$dir" config --get corpus.paths || true)" == "$paths" ]]; then
    echo "ok      $name @ ${rev:0:12} (cached)"
    return
  fi
  rm -rf "$dir"
  git init -q "$dir"
  git -C "$dir" remote add origin "$url"
  set -f # paths are git patterns, not shell globs
  # shellcheck disable=SC2086
  git -C "$dir" sparse-checkout set --no-cone $paths
  set +f
  git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$rev"
  git -C "$dir" -c advice.detachedHead=false checkout -q FETCH_HEAD
  git -C "$dir" config corpus.paths "$paths"
  [[ "$(git -C "$dir" rev-parse HEAD)" == "$rev" ]] || { echo "pin mismatch for $name" >&2; exit 1; }
  echo "fetched $name @ ${rev:0:12}"
}

for r in "${repos[@]}"; do
  IFS='|' read -r name url rev paths <<<"$r"
  fetch_one "$name" "$url" "$rev" "$paths"
done

echo
echo "corpus sources in $src:"
for r in "${repos[@]}"; do
  IFS='|' read -r name _ rev paths <<<"$r"
  printf '  %-16s %s  %6s  %s\n' "$name" "${rev:0:12}" "$(du -sh --exclude=.git "$src/$name" | cut -f1)" "$paths"
done
