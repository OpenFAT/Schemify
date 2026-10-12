#!/usr/bin/env bash
# Binary sizes of the headless core `schemify` and the editor `schemify-gui` (docs/perf.md
# "Binary size"; informational since D39's 2026-10-12 amendment), built the way flake.nix ships
# them: stripped, fat LTO,
# one codegen unit, panic=abort, and on Linux no unwind tables + packed relative relocations.
# Keep the env below in sync with the flake's `env`.
#
#   tools/size-check.sh            build both binaries for the host, then print their sizes
#   tools/size-check.sh --no-build print the sizes of binaries already built by this script
#
# Exits 2 only when a binary is missing; sizes never fail. The fat-LTO editor link needs about
# 4 GB of RAM.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_PROFILE_RELEASE_LTO=fat
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_PANIC=abort
if [[ "$(uname -s)" == Linux ]]; then
  export RUSTFLAGS="-C force-unwind-tables=no -C link-arg=-Wl,-z,pack-relative-relocs"
fi
# An explicit --target, as nix does, keeps RUSTFLAGS off build scripts and proc macros.
host="$(rustc -vV | sed -n 's/^host: //p')"

if [[ "${1:-}" != "--no-build" ]]; then
  cargo build --release --locked --target "$host" -p schemify-cli -p schemify-app
fi

exe=""
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe=".exe" ;; esac

status=0
# Prints binary $1's size next to its rough target $2 (bytes; MB = 10^6 as in D39).
check() {
  local bin="target/$host/release/$1$exe"
  if [[ ! -f "$bin" ]]; then
    echo "missing: $bin" >&2
    status=2
    return
  fi
  local size
  size=$(wc -c <"$bin")
  echo "$1: $size bytes (rough target $2)"
}
check schemify 2000000
check schemify-gui 10000000
exit "$status"
