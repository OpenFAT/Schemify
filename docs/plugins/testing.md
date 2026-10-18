# Testing a plugin

A plugin needs three kinds of test, and its CI runs all three on every push:

1. **Native tests** of the plugin's logic, through the `rlib` (`cargo test`). These are the fast ones and carry most of the oracles.
2. **The guest through the real host.** Load the built `.wasm` with `schemify-plugin` (the same host the editor runs), deliver events and check what comes back. This catches what native tests cannot: the ABI encoding, fuel, grants, the size limit.
3. **The committed module equals its rebuild.** `build.sh` is reproducible, so CI rebuilds the module and fails if the committed bytes differ.

Then a nightly job repeats them against Schemify's `main`, so a core change that breaks the plugin shows up the next morning instead of at the next revision bump.

## Locally

### Native and guest tests

```sh
./build.sh          # refresh my_plugin.wasm first: the guest test loads it
cargo test
```

A guest test loads the module and drives it like the editor does:

```rust
use schemify_plugin::abi::{Event, FUEL_DEFAULT};
use schemify_plugin::{PluginEvents, Plugins, RequestQueues, Snapshots, deliver};

#[test]
fn loads_and_runs_a_command() {
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/my_plugin.wasm")).unwrap();
    assert!(wasm.len() < 200 * 1024, "size limit: {}", wasm.len());
    let mut plugins = Plugins::new();
    // `load` queues `Event::Load` for the next delivery.
    let id = plugins.load("my-plugin", &wasm, &["read_project"], FUEL_DEFAULT).expect("loads");
    let mut events = PluginEvents::new();
    let (mut snap, mut queues) = (Snapshots::new(), RequestQueues::default());
    deliver(&mut plugins, &events, &mut snap, &mut queues);
    events.push(Some(id), &Event::Command { name: "my.run", args: "" }).expect("queued");
    deliver(&mut plugins, &events, &mut snap, &mut queues);
    // Check the contributions, the requests in `queues` and the health report.
}
```

When the plugin starts programs (`spawn:` grants), answer its I/O requests in the test with fake binaries first on `PATH`; [Schemify-PDK-Manager](https://github.com/OmarSiwy/Schemify-PDK-Manager)'s `tests/guest.rs` is a full example. Tests that need a real tool (a simulator, xschem) skip when it is missing, unless an environment variable such as `MY_PLUGIN_REQUIRE_NGSPICE=1` turns the skip into a failure; CI sets it.

Fixtures from Schemify (device files, example projects) are copied into `tests/fixtures/schemify/`, with a README naming the source path and revision. A plugin's tests never read a path inside a Schemify checkout.

### Against a local Schemify checkout

To try a plugin against core changes you have not pushed, override the git source with a path in `.cargo/config.toml` (keep it out of git, or out of the commit):

```toml
[patch."https://github.com/OmarSiwy/Schemify"]
schemify-abi = { path = "../Schemify/crates/abi" }
schemify-format = { path = "../Schemify/crates/format" }
schemify-plugin = { path = "../Schemify/crates/plugin" }
```

Patch every Schemify crate the plugin uses, so they all come from the same tree. `cargo test` and `./build.sh` then build against your checkout.

### In the editor

Point a user package at the repository directory. A `path` package is used in place and never copied, so a rebuild shows up on the next start:

```lua
-- init.lua
schemify.pack.add {
  { path = "~/src/my-plugin", grant = { "read_project" } },
}
```

Or load it for one project only, in its `schemify.project`:

```
plugin my-plugin path ../my-plugin/my_plugin.wasm
  grant read_project
```

Then run `schemify edit <cell>`. `schemify pack status` lists the package and any grant it requests but you did not give (`denied`). Faults (out of fuel, traps) appear in the editor's status line. `--no-default-ui` runs the bare core, which shows what your plugin contributes on its own.

## In CI

Two workflows. `ci.yml` runs on every push and pull request, against the pinned revision:

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
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: module
          path: "*.wasm"
```

Without `rust-lld` in the toolchain (a Nix shell), use `nix develop -c ./build.sh` instead; the module bytes depend on the compiler, so build the committed one the way CI does.

`nightly.yml` runs the same tests against Schemify's `main`. It rewrites the pin in the runner's checkout only and never commits it:

```yaml
name: nightly
on:
  schedule:
    - cron: "41 4 * * *"
  workflow_dispatch:

jobs:
  schemify-main:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: "1.95.0"
          targets: wasm32-unknown-unknown
      - name: point every Schemify dependency at main
        run: |
          sed -i -E 's#(git = "https://github.com/OmarSiwy/Schemify"), rev = "[0-9a-f]+"#\1, branch = "main"#' Cargo.toml
          cargo update $(grep -o 'schemify-[a-z]*' Cargo.toml | sort -u | sed 's/^/-p /')
      - run: ./build.sh
      - run: cargo test --no-fail-fast
```

A red nightly means Schemify's `main` changed something the plugin uses. Fix the plugin, then bump the pinned revision ([publishing.md](publishing.md)).

Add jobs for what your plugin needs beyond this: macOS and Windows runners for a native command line, a Nix job for tools that only Nix provides (Schemify-Interop runs xschem from Nix), fuzzing for readers of untrusted input.
