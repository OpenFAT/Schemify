# Contributing

Rules for everyone who changes this repository, people and AI agents alike. The root `CLAUDE.md` imports this file, so Claude Code reads it at startup.

## Read before touching code

- [decisions.md](decisions.md): the settled design decisions (`D1` to `D40` plus dated amendments). Don't relitigate one; append an amendment instead.
- [architecture.md](architecture.md): crates, owners, spines, invariants.
- [../spec/](../spec/): the normative specs (file format, expression language, netlisting, runs, plugin ABI). Code follows the spec. If the spec is wrong, fix it in the same change.
- [testing.md](testing.md): the binding test plan (D37). Every feature lands with its oracle.
- [status.md](status.md) and [backlog.md](backlog.md): what is done and what is open.

## Skills to load first

Load these before writing or reviewing code. In Claude Code they are user skills (`Skill` tool); other agents read `~/.claude/skills/<name>/SKILL.md`.

| Skill | Use it for |
|---|---|
| `data-oriented-design` | Every struct or table layout. Answer its six questions in writing first: narrowest types, SoA by default, existence-based tables, handles instead of pointers. |
| `simd-loops` | Every hot loop over large data (parsers, connectivity, netlist emission, waveform columns). Triage first; the scalar loop stays as the tail and the oracle. |
| `doc-comments` | Every `pub` item. Each `///` states something the signature can't: a precondition, invalidation, ownership, failure, cost or reason. |
| `codebase-clean-code` | Program shape: tables, transforms, one spine per pipeline, one owner per table, variation by data. State the change radius before coding a feature. |

UI and visual work has its own guides in this repository, under [skills/](skills/). Read them as documents (they are no longer auto-loaded):

| Guide | Use it for |
|---|---|
| [analog-eda-ux](skills/analog-eda-ux/SKILL.md) | Designer-taste rules and the review checklist for any UI surface. |
| [schemify-theme](skills/schemify-theme/SKILL.md) | Colour and chrome tokens; `python3 docs/dev/skills/schemify-theme/check_theme.py` gates them. |
| [schemify-ui-loop](skills/schemify-ui-loop/SKILL.md) | The edit, render, critique, bless loop for the default UI's look. |
| [schemify-symbols](skills/schemify-symbols/SKILL.md) | Drawing rules for the device symbols in `devices/`. |

## Repository layout

| Path | What |
|---|---|
| `crates/` | The Rust workspace: the headless core (`cli`), the editor (`app`) and the libraries they share. [architecture.md](architecture.md) has the map. |
| `devices/` | The bundled `core:` device library (`.shprim`), compiled into the binary. |
| `ui/default-plugin/` | The default UI: a wasm plugin (menus, toolbar, keys, palette) embedded in the editor as `crates/app/assets/default_ui.wasm`. |
| `plugins/` | The optional plugins: `interop`, `pdk-manager`, `gmid-lut`, `cktimg` (with cktImg as the submodule `plugins/cktimg/vendor`). `plugins/Cargo.toml` is the wasm guest workspace for these, the default UI and the host's test guests in `crates/plugin/tests/guests/`. |
| `tests/` | Cross-crate checks: the xschem corpus differential, determinism, fuzz replay, perf, the web smoke test. |
| `tools/` | `spicecmp` (semantic netlist comparison) and benchmarking helpers. |
| `docs/` | The user guide, the specs and these contributor notes. |

## Hard rules

1. Clean room (D1). Never open, copy, port or paraphrase xschem source (`*.c`, `*.y`, `*.awk`, `*.tcl`). Behaviour comes from `docs/spec/` and from golden outputs produced by running xschem. The same applies to any GPL code.
2. Abstractions only at zero or negative overhead, and only when they merge several interfaces into one. The allowed list is in [architecture.md](architecture.md) § Unifying abstractions. No `dyn` or vtables in the core; the only runtime dispatch is the plugin boundary.
3. Single owner. Only the owning crate writes its tables (`pub(crate)` mutation). Other crates send request tables.
4. Determinism. The same input gives byte-identical output on Linux, macOS, Windows and the web. No `HashMap` iteration order in output paths, and no floats where the spec says i32 (coordinates are i32 milli-units, D33).
5. Asserts (TigerStyle). At least two per function on average: entry invariants and one exit invariant, covering positive and negative space. Every table has a stated capacity; every loop and queue is bounded.
6. No `unsafe` in our crates (`#![forbid(unsafe_code)]`) unless a profile proves the need and a `// SAFETY:` comment states the obligation.
7. Dependencies. Climb the ladder: stdlib, then an existing dependency, then a few lines of our own, then a new dependency (justify it in the PR). Heavy dependencies (vello, wgpu, wasmi, winit) stay in their designated crates.
8. Design files carry no executable content (D7). Nothing in a `.sh*` file can run a command.
9. Mark deliberate simplifications with `// ponytail: <ceiling and upgrade path>`.

## Building

- `main` is the integration branch (remote `origin` = github.com/OmarSiwy/Schemify, public). Work on a branch and open a PR; CI work may push `ci/*` branches. Don't keep extra git worktrees around.
- Build and test the crate you changed: `cargo test -p schemify-<crate>`. Edition 2024, stable toolchain pinned to the `rust-version` in `Cargo.toml`. `cargo clippy -p <crate> -- -D warnings` must be clean, and run `cargo fmt`.
- `nix develop` gives the pinned toolchain, Zig 0.17 (for cktImg), the simulators, xschem and the GUI libraries.
- Changes to the root `Cargo.toml` dependency list or `Cargo.lock` go through the maintainer. If you need a dependency or an API change in another crate, say so in the PR.

### Memory limits on the shared build machine

The machine has 31 GB of RAM shared by several jobs, and it has run out twice. Run every build, test or tool through a memory-capped cgroup so a runaway process gets killed instead of the desktop:

```sh
systemd-run --user --scope -q -p MemoryMax=6G -p MemorySwapMax=0 <command>
```

Use 7G when at most three jobs share the machine and 6G with four. Exit code 137 means the cap killed the process: find the cause instead of raising the cap. Also cap parallelism with `export CARGO_BUILD_JOBS=3` (4 with three jobs or fewer), `zig build -j4` and `cargo test -- --test-threads=4`.

Check `free -g` before a release or LTO build and wait if less than 12 GB is available. Run one heavy job at a time (a release or LTO build, the wasm guest build, a Zig build, the full corpus differential). Prefer debug builds for tests and use `--release` only for the perf checks that need it. Native code reached through FFI (cktImg) has no wasm memory cap, so bound its inputs. Disk is tight too: run `cargo clean` when you finish a large build you won't reuse.

### Wasm guests

The plugins, the default UI and the host's test guests are wasm modules committed next to their users. `plugins/build.sh` rebuilds all of them byte-reproducibly (host paths are remapped), and CI's `plugins` job fails when a committed module differs from its rebuild. After editing any guest or `crates/abi`, run:

```sh
nix develop -c plugins/build.sh
```

and commit every `.wasm` it changes. Without Nix, download the `plugin-guests` artifact of your PR's CI run instead (`gh run download <run> -n plugin-guests -D <tmp>`, then `cp -r <tmp>/. .`).

## Running the editor

- `cargo run -p schemify-app -- edit crates/app/tests/fixtures/inv/top.shcomp` opens a cell; its project is the nearest directory with a `schemify.project`. `edit <dir>` opens a project with a new schematic. `--no-default-ui` runs the bare core (window and canvas, mouse only), which proves the editor stands without the default UI plugin (D6).
- `cargo test -p schemify-app` drives the real frame loop headlessly. `SCHEMIFY_BLESS=1` re-blesses the golden PNGs in `crates/app/tests/golden/`. The 100k-object budget check is `cargo test --release -p schemify-app --test perf -- --ignored --nocapture`.
- On NixOS, winit loads Wayland and Vulkan at runtime, so export `LD_LIBRARY_PATH` with the `lib` directories of `nixpkgs#wayland`, `#libxkbcommon`, `#vulkan-loader` and `/run/opengl-driver/lib`. To take a screenshot without a desktop, run `WLR_BACKENDS=headless WLR_RENDERER=pixman cage -- schemify edit FILE` inside `nix shell nixpkgs#cage`, then `grim out.png` with `WAYLAND_DISPLAY` set to cage's socket.

## Definition of done

- The spec section exists and matches the code.
- The feature has the oracle-backed tests [testing.md](testing.md) asks for, negative space included, not only examples.
- Public items are documented per `doc-comments`, and `cargo doc -p <crate>` prints no warnings.
- The commit or PR description states the change radius.
