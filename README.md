# Schemify SDK

Everything you need to build plugins and themes for [Schemify](https://github.com/OpenFAT), the schematic editor for analog and mixed-signal IC design. The editor itself is a separate product; this repository holds the open parts it is built on.

## What is here

| Path | What it is |
|---|---|
| `crates/protocol/format` | The Schemify file format: one line per object, sorted by a stable id. Read and write `.shcomp`, `.shbench` and project files. |
| `crates/protocol/abi` | The plugin ABI, plus the guest SDK for Rust (`features = ["guest"]`). |
| `crates/protocol/abi/sdk` | Guest SDKs for C, Zig, Go, Odin and AssemblyScript, each with a conformance suite. |
| `crates/plugins/plugin` | The plugin sandbox host: the same wasm engine, fuel limits and grant checks the editor uses. Use it to test a plugin without the editor. |
| `examples/plugins` | A small working plugin in each of ten languages. Copy one to start. |
| `ui/default-plugin` | The editor's default UI as a reference UI pack: every node id and style a theme can change. |
| `docs/plugins` | How to lay out, test and publish a plugin. |
| `docs/spec` | The file format, the plugin ABI, the Lua config API, edit operations, expressions and docs pages. |
| `docs/examples/init.lua` | An example user config. |

## Build and test

You need Rust 1.95 or newer.

```sh
cargo test --workspace
```

To build a guest module, add the wasm target once (`rustup target add wasm32-unknown-unknown`) and follow the example for your language in `examples/plugins/`.

## Write a plugin

1. Copy an example from `examples/plugins/<language>/`.
2. Point your Schemify config at the folder: `schemify.pack.add{ path = "~/dev/my-plugin" }` in `init.lua`.
3. Test it against the sandbox host in `crates/plugins/plugin` (see `docs/plugins/testing.md`).
4. Publish it as described in `docs/plugins/publishing.md`.

## License

MIT. See `LICENSE`.
