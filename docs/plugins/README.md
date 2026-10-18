# Writing a plugin

A Schemify plugin is a wasm module ([plugin ABI](../spec/plugin-abi.md)) that lives in its own git repository. Schemify neither builds nor tests plugins: each repository builds its module, tests it against a pinned Schemify revision, and commits the built `.wasm`. Users install it straight from the repository URL ([config.md](../spec/config.md) §5).

| Page | Read it when you want to |
|---|---|
| [Repository layout](repository.md) | start a plugin repository: the files it needs, how it depends on Schemify's crates, where a second interface goes |
| [Testing](testing.md) | test a plugin on your machine and in CI, against a local Schemify checkout and against Schemify's `main` |
| [Publishing](publishing.md) | tag a release, let users install it from `init.lua` or a project manifest, and move to a newer Schemify |

## Examples

Four plugins are checked out under [examples/plugins/](../../examples/plugins) as git submodules. They set `update = none` (so cargo git dependencies on Schemify skip them), which plain `git submodule update --init` and `git clone --recurse-submodules` also honour: fetch them with `git submodule update --init --checkout`. Each is a complete repository that follows these pages:

| Plugin | Repository | Shows |
|---|---|---|
| `interop` | [Schemify-Interop](https://github.com/OmarSiwy/Schemify-Interop) | a large guest plus a native command line (`schemify-interop`) built from the same crate, fuzzing, a nightly differential against a real tool (xschem) |
| `pdk-manager` | [Schemify-PDK-Manager](https://github.com/OmarSiwy/Schemify-PDK-Manager) | `spawn:` grants, a panel and commands, tests with fake binaries on `PATH` |
| `cktimg` | [cktImg](https://github.com/OmarSiwy/cktImg) | a plugin that links a native engine (Zig, through its C ABI) into the wasm module; the engine's C ABI and LaTeX package are secondary interfaces |
| `gmid-lut` | [GmIDVisualizer](https://github.com/OmarSiwy/GmIDVisualizer) | pure expression functions for docs pages (`functions` contributions); the C++ library and CLI are a secondary interface |

The bundled default UI ([ui/default-plugin](../../ui/default-plugin)) is a plugin too, but it ships inside the editor and builds in this repository.
