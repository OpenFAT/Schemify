#!/usr/bin/env bash
# Build the example guest plugins for wasm32-unknown-unknown and refresh the host's test
# fixtures in crates/plugin/tests/fixtures/. Fails if a fixture reaches 200 KB.
#
# Needs the wasm32-unknown-unknown std (rustup: `rustup target add wasm32-unknown-unknown`;
# the NixOS rustc ships it). The nix-built rustc has no rust-lld, so without one on the
# toolchain the script re-runs itself under `nix shell nixpkgs#lld` and links with wasm-ld.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixtures="$here/../crates/plugin/tests/fixtures"
host="$(rustc -vV | sed -n 's/^host: //p')"

if [[ ! -x "$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld" ]] \
   && [[ -z "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER:-}" ]]; then
  if command -v wasm-ld >/dev/null; then
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld
  else
    exec nix shell nixpkgs#lld -c env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld "$0" "$@"
  fi
fi

cargo build --manifest-path "$here/Cargo.toml" --release --target wasm32-unknown-unknown

mkdir -p "$fixtures"
for name in hello bad_actor gmid_stub; do
  wasm="$here/target/wasm32-unknown-unknown/release/$name.wasm"
  size=$(wc -c <"$wasm")
  if (( size >= 200 * 1024 )); then
    echo "error: $name.wasm is $size bytes (limit 200 KB)" >&2
    exit 1
  fi
  cp "$wasm" "$fixtures/$name.wasm"
  echo "$name.wasm $size bytes"
done
