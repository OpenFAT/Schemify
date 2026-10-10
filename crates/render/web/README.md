# Web build (wasm32 + WebGPU)

The demo shell runs in the browser from the same code as the native build (D2): winit attaches a
canvas to the page, wgpu talks to WebGPU, and the only JavaScript is wasm-bindgen's generated glue
plus the one-line loader in `index.html`.

Requirements:

- the `wasm32-unknown-unknown` Rust target;
- a linker for it: rustup toolchains ship `rust-lld`; a distro rustc may not, in which case point
  `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER` at an `lld` matching rustc's LLVM version
  (`rustc -vV`);
- `wasm-bindgen` CLI at exactly the `wasm-bindgen` version in `Cargo.lock` (0.2.129 at the time of
  writing): `cargo install wasm-bindgen-cli --version 0.2.129 --locked`;
- a browser with WebGPU enabled.

Build and serve, from the repository root:

```text
cargo build --release --target wasm32-unknown-unknown -p schemify-render --example demo
wasm-bindgen --target web --no-typescript --out-dir crates/render/web/pkg \
    target/wasm32-unknown-unknown/release/examples/demo.wasm
python3 -m http.server --directory crates/render/web 8080
```

Then open <http://localhost:8080>. Any static file server works; `pkg/` is generated and ignored by
git.

Measured sizes (release, 2026-10-10): `demo_bg.wasm` 8.8 MB raw, 3.4 MB gzip (D39 budget: 8 MB
compressed). About 2 MB of that is the embedded fonts.
