#!/usr/bin/env bash
# D39 binary-size budgets (docs/perf.md "Binary size"): the headless core `schemify` <= 2 MB
# and the editor `schemify-gui` <= 10 MB, built the way flake.nix ships them: stripped, fat LTO,
# one codegen unit, panic=abort, and on Linux no unwind tables + packed relative relocations.
# Keep the env below in sync with the flake's `env`.
#
#   tools/size-check.sh            build both binaries for the host, then check
#   tools/size-check.sh --no-build check binaries already built by this script
#
# Exit 1 when a binary is over budget, 2 when one is missing. The fat-LTO editor link needs
# about 4 GB of RAM.
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
# Checks binary $1 against budget $2 (bytes; MB = 10^6 as in D39).
check() {
  local bin="target/$host/release/$1$exe"
  if [[ ! -f "$bin" ]]; then
    echo "missing: $bin" >&2
    status=2
    return
  fi
  local size
  size=$(wc -c <"$bin")
  if ((size > $2)); then
    echo "OVER  $1: $size bytes > budget $2" >&2
    ((status == 2)) || status=1
  else
    echo "ok    $1: $size bytes <= budget $2"
  fi
}
check schemify 2000000
check schemify-gui 10000000
exit "$status"
