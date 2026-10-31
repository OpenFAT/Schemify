#!/usr/bin/env bash
# Build the core's guest modules for wasm32-unknown-unknown: the host's test fixtures in
# crates/plugins/plugin/tests/fixtures/ and the bundled default UI in crates/model/editor/assets/ (embedded by the
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
# CI `guests` artifact. The build runs in one fixed directory, $stage, holding a copy of the
# checkout's tracked and unignored files: cargo hashes the absolute path of crates outside
# this workspace's root (the abi and the plugin test guests) into their symbols, so a build in
# another checkout's directory gives other bytes. $stage is remapped out of the modules
# (panic locations name /schemify), as is the registry.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
# Always $stage/ui/target: a target directory elsewhere escapes the remap below.
unset CARGO_TARGET_DIR
fixtures="$root/crates/plugins/plugin/tests/fixtures"
assets="$root/crates/model/editor/assets"
stage=/tmp/schemify-guests
host="$(rustc -vV | sed -n 's/^host: //p')"
pinned="$(sed -n 's/^rust-version = "\(.*\)"/\1/p' "$here/Cargo.toml")"
if ! rustc -V | grep -q "^rustc $pinned\(\.0\)\? "; then
  echo "warning: $(rustc -V) is not the pinned rustc $pinned (nix develop): the modules will" \
    "not match the committed ones byte for byte" >&2
fi
# Unit separator between flags: paths may contain spaces.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$stage=/schemify"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"

if [[ ! -x "$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld" ]] \
   && [[ -z "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER:-}" ]]; then
  if command -v wasm-ld >/dev/null; then
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld
  else
    exec nix shell nixpkgs#lld -c env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld "$0" "$@"
  fi
fi

# One build at a time owns $stage. flock is util-linux; without it (macOS) builds are not
# serialized.
if command -v flock >/dev/null; then
  exec 9>"$stage.lock"
  flock 9
fi
# Fresh sources, kept target: everything but ui/target goes, then the copy lands with new
# mtimes, so cargo rebuilds every path crate from these sources and keeps registry crates.
mkdir -p "$stage/ui"
find "$stage" -mindepth 1 -maxdepth 1 ! -name ui -exec rm -rf {} +
find "$stage/ui" -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} +
git -C "$root" ls-files -z --cached --others --exclude-standard \
  | while IFS= read -r -d '' f; do [[ -e "$root/$f" ]] && printf '%s\0' "$f"; done \
  | tar -C "$root" --null -T - -cf - \
  | tar -C "$stage" -xmf -

# Six jobs: the machine's agents share its RAM (docs/dev/contributing.md).
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}" \
  cargo build --manifest-path "$stage/ui/Cargo.toml" --release --target wasm32-unknown-unknown

# Copies module $1 into directory $2, enforcing the size limit $3 (KB, default 200).
install_wasm() {
  local wasm="$stage/ui/target/wasm32-unknown-unknown/release/$1.wasm"
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
