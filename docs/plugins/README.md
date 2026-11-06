# Writing a plugin

A Schemify plugin is a wasm module ([plugin ABI](../spec/plugin-abi.md)) that lives in its own git repository. Schemify neither builds nor tests plugins: each repository builds its module, tests it against a pinned Schemify revision, and commits the built `.wasm`. Users install it straight from the repository URL ([config.md](../spec/config.md) §5).

| Page | Read it when you want to |
|---|---|
| [Languages](languages.md) | write a plugin in Rust, C, C++, Zig, Odin, Go, TypeScript, Nim, D or Swift, starting from a template in [examples/plugins/](../../examples/plugins) |
| [Repository layout](repository.md) | start a plugin repository: the files it needs, how it depends on Schemify's crates, where a second interface goes |
| [Testing](testing.md) | test a plugin on your machine and in CI, against a local Schemify checkout and against Schemify's `main` |
| [Publishing](publishing.md) | tag a release, let users install it from `init.lua` or a project manifest, and move to a newer Schemify |

## Examples

Four plugins live in their own repositories under the OpenFAT organization. Each is a complete repository that follows these pages:

| Plugin | Repository | Shows |
|---|---|---|
| `interop` | [Interoperate](https://github.com/OpenFAT/Interoperate) | a large guest plus a native command line (`schemify-interop`) built from the same crate, fuzzing, a nightly differential against a real tool (xschem) |
| `pdk-manager` | [PDK-Manager](https://github.com/OpenFAT/PDK-Manager) | `spawn:` grants, a panel and commands, tests with fake binaries on `PATH` |
| `cktimg` | [cktImg](https://github.com/OpenFAT/cktImg) | a plugin that links a native engine (Zig, through its C ABI) into the wasm module; the engine's C ABI and LaTeX package are secondary interfaces |
| `gmid-lut` | [GmId-Visualizer](https://github.com/OpenFAT/GmId-Visualizer) | pure expression functions for docs pages (`functions` contributions); the C++ library and CLI are a secondary interface |

The bundled default UI ([ui/default-plugin](../../ui/default-plugin)) is a plugin too, but it ships inside the editor and builds in this repository.
