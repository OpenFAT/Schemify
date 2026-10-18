# Plugin repository layout

## The plugin comes first

A plugin repository is primarily a Schemify plugin. Its root is the plugin crate, and its README opens with what the plugin does in Schemify and how to install it. When the same code serves another consumer, such as a C library, a command line or a LaTeX package, that interface is secondary: it lives in a subdirectory with its own build and its own README section, and the plugin never depends on it being built.

## Files

```
my-plugin/
  Cargo.toml            # the plugin crate and its own [workspace]
  Cargo.lock            # committed
  rust-toolchain.toml   # the Rust Schemify pins, plus wasm32-unknown-unknown
  schemify.project      # one `plugin` record: the module and the grants it requests
  my_plugin.wasm        # the built module, committed
  build.sh              # reproducible build of my_plugin.wasm
  flake.nix             # optional: a dev shell with the pinned toolchain and test tools
  src/
  tests/
    fixtures/schemify/  # copies of the Schemify files the tests read
  .github/workflows/
    ci.yml
    nightly.yml
  <secondary>/          # optional: a second interface (cpp/, engine/, ...)
```

`schemify.project` and the `.wasm` at the root are what `schemify pack` installs ([config.md](../spec/config.md) §5, Layout). Without a manifest, the module must be named after the repository (`<name>.wasm`, where `<name>` is the last segment of the URL), and it requests exactly what the user grants. With one, the manifest names the module and lists the capabilities it requests:

```
schemify 1 project

plugin my-plugin path my_plugin.wasm
  grant read_project
  grant spawn:ngspice
```

## Cargo.toml

The root is its own workspace, so the profiles live there:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"
rust-version = "1.95"
license = "MIT"
publish = false

[lib]
crate-type = ["cdylib", "rlib"]   # cdylib: the guest; rlib: native tests

[workspace]
resolver = "3"

[workspace.dependencies]
schemify-abi = { git = "https://github.com/OmarSiwy/Schemify", rev = "<full commit sha>", features = ["guest"] }
schemify-format = { git = "https://github.com/OmarSiwy/Schemify", rev = "<full commit sha>" }
schemify-plugin = { git = "https://github.com/OmarSiwy/Schemify", rev = "<full commit sha>" }

[dependencies]
schemify-abi.workspace = true

[dev-dependencies]
schemify-plugin.workspace = true  # the real host, for the guest test

# Small modules: size over speed, no unwinding (a panic is a trap the host reports).
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
debug = false
```

### Why a git dependency pinned to a revision

Schemify's crates are not published to crates.io, and their APIs change between commits. Every Schemify crate a plugin uses comes from the same `rev`, so the plugin always builds against one consistent core and its build never changes under it. Moving to a newer Schemify is a deliberate one-line change ([publishing.md](publishing.md)). Use only `schemify-abi` in the guest when you can: it is the versioned plugin ABI and changes least. Depending on `schemify-format`, `-doc`, `-elab` or `-netlist` is allowed (the interop plugin links several), but those crates are internals, so expect to fix the plugin when you bump the revision.

Cargo checks out a git dependency's submodules too, unless `.gitmodules` sets `update = none` for one. Schemify's submodules (the example plugins under `examples/plugins/`) all set it, so a git dependency on a Schemify crate clones Schemify alone (cargo prints `Skipping git submodule … due to update strategy`).

## rust-toolchain.toml

```toml
[toolchain]
channel = "1.95.0"
targets = ["wasm32-unknown-unknown"]
components = ["rustfmt", "clippy"]
```

Use the version in Schemify's `rust-version`. The committed module is byte-reproducible only with the same compiler.

## build.sh

The script builds the guest for `wasm32-unknown-unknown` in release mode, enforces a size limit and copies the module to the root. It remaps host paths out of the module, so the bytes depend only on the sources, `Cargo.lock` and the toolchain:

```sh
#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$here=/my-plugin"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
cargo build --manifest-path "$here/Cargo.toml" --release --target wasm32-unknown-unknown
wasm="${CARGO_TARGET_DIR:-$here/target}/wasm32-unknown-unknown/release/my_plugin.wasm"
size=$(wc -c <"$wasm")
(( size < 200 * 1024 )) || { echo "error: my_plugin.wasm is $size bytes (limit 200 KB)" >&2; exit 1; }
cp "$wasm" "$here/my_plugin.wasm"
```

The bytes depend on the compiler, so pin it the way Schemify's `flake.nix` does: the `oxalica/rust-overlay` input (following your `nixpkgs`) and `rust-bin.stable."1.95.0".default` with the `wasm32-unknown-unknown` target. That toolchain ships `rust-lld`, which links the guest. nixpkgs' own rustc is whatever the locked nixpkgs carries and has no `rust-lld`: with it, set `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=wasm-ld` and put `lld` in the shell. Schemify's own `ui/build.sh` is a fuller template.

## Rules that still apply

The rules of [contributing.md](../dev/contributing.md) apply inside a plugin repository too: clean room (no xschem or other GPL source), asserts, doc comments, determinism. A plugin gets only the capabilities it requests and the user grants (D7), so request the fewest it needs.
