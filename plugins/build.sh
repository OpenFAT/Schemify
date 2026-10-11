#!/usr/bin/env bash
# Build the guest plugins for wasm32-unknown-unknown: refresh the host's test fixtures in
# crates/plugin/tests/fixtures/, the bundled default UI in crates/app/assets/ (embedded by the
# app with include_bytes!), the gm/Id plugin of the app's OTA fixture project (loaded through
# its manifest's `plugin` entry, not embedded) and the cktimg plugin next to its source (a
# project loads it with `plugin cktimg path plugins/cktimg`). Fails if a module reaches its
# size limit: 200 KB, except cktimg (550 KB: cktImg's layout engine is linked in; see its README).
#
# Needs the wasm32-unknown-unknown std (rustup: `rustup target add wasm32-unknown-unknown`;
# the NixOS rustc ships it) and, for cktimg, Zig 0.17 (its build.rs compiles extern/cktimg;
# `nix develop` has it, and `git submodule update --init` fetches the source). The nix-built
# rustc has no rust-lld, so without one on the toolchain the script re-runs itself under
# `nix shell nixpkgs#lld` and links with wasm-ld.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixtures="$here/../crates/plugin/tests/fixtures"
assets="$here/../crates/app/assets"
host="$(rustc -vV | sed -n 's/^host: //p')"

if [[ ! -x "$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld" ]] \
   && [[ -z "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER:-}" ]]; then
  if command -v wasm-ld >/dev/null; then
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld
  else
    exec nix shell nixpkgs#lld -c env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld "$0" "$@"
  fi
fi

# Six jobs: the machine's agents share its RAM (AGENTS.md).
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}" \
  cargo build --manifest-path "$here/Cargo.toml" --release --target wasm32-unknown-unknown

# Copies module $1 into directory $2, enforcing the size limit $3 (KB, default 200).
install_wasm() {
  local wasm="$here/target/wasm32-unknown-unknown/release/$1.wasm"
  local limit="${3:-200}"
  local size
  size=$(wc -c <"$wasm")
  if (( size >= limit * 1024 )); then
    echo "error: $1.wasm is $size bytes (limit $limit KB)" >&2
    exit 1
  fi
  mkdir -p "$2"
  cp "$wasm" "$2/$1.wasm"
  echo "$1.wasm $size bytes -> $2"
}

for name in hello bad_actor gmid_stub; do
  install_wasm "$name" "$fixtures"
done
install_wasm default_ui "$assets"
install_wasm gmid_lut "$here/../crates/app/tests/fixtures/ota/plugins"
install_wasm cktimg "$here/cktimg" 550
