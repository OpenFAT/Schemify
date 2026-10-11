# Schemify

[![ci](https://github.com/OmarSiwy/Schemify/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/OmarSiwy/Schemify/actions/workflows/ci.yml)
[![nightly](https://github.com/OmarSiwy/Schemify/actions/workflows/nightly.yml/badge.svg?branch=main)](https://github.com/OmarSiwy/Schemify/actions/workflows/nightly.yml)

Schematic capture for analog and mixed-signal IC design, written in Rust as a replacement for
[xschem](https://xschem.sourceforge.io/). It reads xschem libraries and PDKs, netlists for ngspice,
Xyce, CDL, Spectre and Verilog, runs simulations, shows waveforms, and extends through WASM
plugins. The same code runs natively on Linux, macOS and Windows and in a browser.

Licensed MIT OR Apache-2.0. It is a clean-room implementation: no xschem (GPL) code is copied or
ported; behaviour is checked against netlists produced by running xschem.

## Status

Pre-release. What works today:

- The CLI: `import` (xschem libraries and PDKs), `netlist`, `check`, `run` (ngspice), `fmt`,
  `diff` and a git merge driver for design files.
- The editor (`schemify edit`): xschem-style editing and keys, a docs pane with live
  expressions, live checks, net highlighting, non-blocking simulation with a waveform viewer,
  and a default UI that is itself a plugin.
- Netlists agree with xschem's on 246 of 301 SPICE corpus benches (sky130, GF180, IHP and
  xschem's examples); the rest are triaged in [tests/corpus/RATCHET.md](tests/corpus/RATCHET.md).
- The browser build starts the editor on a new in-memory schematic. Drawing needs WebGPU, and
  files are not persisted yet (OPFS storage is a stub).

[docs/STATUS.md](docs/STATUS.md) tracks progress and [docs/BACKLOG.md](docs/BACKLOG.md) the open
items.

## Build and run

With Nix (flakes):

```sh
nix run github:OmarSiwy/Schemify -- edit path/to/cell.shcomp   # the packaged editor
nix develop      # toolchain, Zig, simulators, xschem for the golden corpus
```

With Cargo (Rust 1.95 or newer; Linux needs the Wayland or X11 and Vulkan libraries at runtime):

```sh
cargo run --release -p schemify-app -- edit crates/app/tests/fixtures/inv/top.shcomp
cargo run --release -p schemify-app -- netlist crates/app/tests/fixtures/rc/tb.shbench
cargo test --workspace
```

Web build (needs the `wasm32-unknown-unknown` target, the `wasm-bindgen` CLI at the version in
`Cargo.lock` and a browser with WebGPU):

```sh
cargo build --release --target wasm32-unknown-unknown -p schemify-app --bin schemify
wasm-bindgen --target web --no-typescript --out-dir crates/app/web/pkg \
    target/wasm32-unknown-unknown/release/schemify.wasm
python3 -m http.server --directory crates/app/web 8080
```

CI also publishes the bundle as the `web` artifact of each `ci` run.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): crates, data ownership, pipelines.
- [docs/DECISIONS.md](docs/DECISIONS.md): the settled design decisions.
- [docs/spec/](docs/spec/): file format, expression language, netlisting, plugin ABI.
- [TESTING.md](TESTING.md): the test plan and the CI tiers.
- [AGENTS.md](AGENTS.md): rules for contributors.
