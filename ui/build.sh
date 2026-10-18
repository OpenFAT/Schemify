#!/usr/bin/env bash
# Build the core's guest modules for wasm32-unknown-unknown: the host's test fixtures in
# crates/plugin/tests/fixtures/ and the bundled default UI in crates/app/assets/ (embedded by the
# app with include_bytes!). Fails if a module reaches its size limit (200 KB). The optional
# plugins build in their own repositories (docs/plugins/).
#
# Needs the wasm32-unknown-unknown std (rustup: `rustup target add wasm32-unknown-unknown`;
# `nix develop` has it, rust-lld included). Without a rust-lld on the
# toolchain the script re-runs itself under `nix shell nixpkgs#lld` and links with wasm-ld.
#
# Reproducible: the CI `guests` job rebuilds every module and fails when a committed one
# differs. The bytes depend only on the sources, the lockfile and the toolchain: build in
# `nix develop` (the rustc and wasm-ld flake.lock pins; CI does) or commit the modules of the
# CI `guests` artifact. Host paths are remapped out of the modules (panic locations of path
# and registry crates would otherwise name this checkout).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
fixtures="$here/../crates/plugin/tests/fixtures"
assets="$here/../crates/app/assets"
host="$(rustc -vV | sed -n 's/^host: //p')"
pinned="$(sed -n 's/^rust-version = "\(.*\)"/\1/p' "$here/Cargo.toml")"
if ! rustc -V | grep -q "^rustc $pinned\(\.0\)\? "; then
  echo "warning: $(rustc -V) is not the pinned rustc $pinned (nix develop): the modules will" \
    "not match the committed ones byte for byte" >&2
fi
# Unit separator between flags: paths may contain spaces.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$root=/schemify"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"

if [[ ! -x "$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld" ]] \
   && [[ -z "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER:-}" ]]; then
  if command -v wasm-ld >/dev/null; then
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld
  else
    exec nix shell nixpkgs#lld -c env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld "$0" "$@"
  fi
fi

# Six jobs: the machine's agents share its RAM (docs/dev/contributing.md).
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}" \
  cargo build --manifest-path "$here/Cargo.toml" --release --target wasm32-unknown-unknown

# Copies module $1 into directory $2, enforcing the size limit $3 (KB, default 200).
install_wasm() {
  local wasm="${CARGO_TARGET_DIR:-$here/target}/wasm32-unknown-unknown/release/$1.wasm"
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
