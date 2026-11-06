# Testing a plugin

A plugin needs three kinds of test:

1. **Native tests** of the plugin's logic, through the `rlib` (`cargo test`). These are the fast ones and carry most of the oracles: build the module's inputs (event text, table rows) by hand and check its outputs.
2. **The module in the app.** Load the built `.wasm` in Schemify from its folder and run its commands. This catches what native tests cannot: the ABI encoding, fuel, grants, the size limit.
3. **The committed module equals its rebuild.** `build.sh` is reproducible, so CI rebuilds the module and fails if the committed bytes differ.

The host that loads plugins is part of the Schemify app and is not published, so there is no in-process guest test. Schemify's own CI loads the SDK conformance guests and the example plugins on every release, so the ABI itself is tested there.

## Locally

### Native tests

```sh
cargo test
```

Keep the logic in plain functions over plain data, so a test calls them directly. Check the module's size in a test as well (the limit is 200 KB):

```rust
#[test]
fn module_fits_the_size_limit() {
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/my_plugin.wasm")).unwrap();
    assert!(wasm.len() < 200 * 1024, "size limit: {}", wasm.len());
}
```

Tests that need a real tool (a simulator, xschem) skip when it is missing, unless an environment variable such as `MY_PLUGIN_REQUIRE_NGSPICE=1` turns the skip into a failure; CI sets it.

### In the app

Point a user package at the repository directory. A `path` package is used in place and never copied, so a rebuild shows up on the next start:

```lua
-- init.lua
schemify.pack.add {
  { path = "~/src/my-plugin", grant = { "read_project" } },
}
```

Then open a project. `schemify pack status` lists the package and any grant it requests but you did not give (`denied`). Faults (out of fuel, traps) appear in the editor's status line, and a module that fails to load says why. `--no-default-ui` runs the bare core, which shows what your plugin contributes on its own.

## In CI

`ci.yml` runs on every push and pull request:

```yaml
name: ci
on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: "1.95.0"
          targets: wasm32-unknown-unknown
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: ./build.sh
      - run: cargo test --no-fail-fast
      # build.sh is byte-reproducible: a source change without its module, or a module
      # built elsewhere, fails here. Commit the module this job built.
      - name: committed module equals the rebuild
        run: |
          git diff --exit-code --stat -- '*.wasm' || {
            echo "::error::the committed .wasm differs from build.sh output"; exit 1; }
```

Without `rust-lld` in the toolchain (a Nix shell), use `nix develop -c ./build.sh` instead; the module bytes depend on the compiler, so build the committed one the way CI does.

When Schemify releases a new SDK revision, bump the pin ([publishing.md](publishing.md)) and rerun. The versioning policy ([versioning.md](versioning.md)) says what a release may change.

Add jobs for what your plugin needs beyond this: macOS and Windows runners for a native command line, a Nix job for tools that only Nix provides, fuzzing for readers of untrusted input.
