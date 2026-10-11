# AGENTS.md

Instructions for every agent (human or AI) working in this repo.

## Must-load skills

Load these **before writing or reviewing any code** (Claude Code: `Skill` tool; other agents: read the files under `~/.claude/skills/<name>/SKILL.md` and the companions they link):

| Skill | Use it for |
|---|---|
| `data-oriented-design` | Every struct/table layout. Answer the six questions in writing first; narrowest types; SoA by default; existence-based tables; handles not pointers. |
| `simd-loops` | Every hot loop over large data (parsers, connectivity, netlist emission, waveform columns). Triage first; scalar loop stays as tail + oracle. |
| `doc-comments` | Every `pub` item. Each `///` carries a fact the signature cannot (precondition, invalidation, ownership, failure, cost, why). |
| `codebase-clean-code` | Program shape: tables, transforms, one spine per pipeline, single owner per table, variation by data. State the change radius before coding a feature. |

## Read before touching code

- [docs/DECISIONS.md](docs/DECISIONS.md) — the 40 settled decisions (`D#`). Don't relitigate; propose an amendment instead.
- [ARCHITECTURE.md](ARCHITECTURE.md) — crates, owners, spines, invariants.
- [docs/spec/](docs/spec/) — normative specs (file format, expression language, netlisting). Code follows the spec; if the spec is wrong, fix the spec in the same change.
- [TESTING.md](TESTING.md) — binding test plan (D37): every feature lands with its oracle.

## Hard rules

1. **Clean-room (D1).** Never open, copy, port or paraphrase xschem source (`*.c`, `*.y`, `*.awk`, `*.tcl`). Behaviour comes from `docs/spec/`, `docs/research/` and golden outputs produced by *running* xschem. Same for any GPL code.
2. **Abstractions** only at zero or negative overhead **and** when they merge several interfaces into one. The allowed list is in ARCHITECTURE.md § Unifying abstractions. No `dyn`/vtables in core; the only runtime dispatch is the plugin boundary.
3. **Single owner.** Only the owning crate writes its tables (`pub(crate)` mutation). Others send request tables.
4. **Determinism.** Same input → byte-identical output on Linux/macOS/Windows/Web. No `HashMap` iteration order in output paths; no floats where the spec says i32 (coordinates are i32 milli-xschem-units, D33).
5. **Asserts (TigerStyle).** ≥ 2 per function on average: entry invariants and one exit invariant; assert positive and negative space. Every table has a stated capacity; every loop and queue is bounded.
6. **No `unsafe`** in our crates (`#![forbid(unsafe_code)]`) unless a profile proves the need and a `// SAFETY:` comment states the obligation.
7. **Dependencies.** Climb the ladder: stdlib → existing dep → a few lines of our own → new dep (justify in the PR). Heavy deps (vello, wgpu, wasmi, winit) only in their designated crates.
8. **Docs files carry no executable content (D7).** Nothing in a `.sh*` file can run a command.
9. Mark deliberate simplifications `// ponytail: <ceiling and upgrade path>`.

## Building on this machine

- Agents may work in their own git worktree. Disk is tight (~19 GB free): when your work is done, **delete your worktree's `target/`** (`cargo clean` or `rm -rf target`) before reporting back. Never set a custom `CARGO_TARGET_DIR` outside the worktree.
- Build and test your crate only: `cargo test -p schemify-<crate>`. Full workspace runs are for the integrator.
- **RAM is the binding limit (31 GB, several agents share it; the host OOMed on 2026-10-11).** Cap parallelism: `export CARGO_BUILD_JOBS=6`, `zig build -j4`, `cargo test -- --test-threads=4`. Never run two heavy jobs at once (a release/LTO build, the wasm guest build, a Zig build, the full corpus differential): finish one before starting the next. Prefer debug builds for tests; use `--release` only for the perf checks that need it.
- Edition 2024, stable toolchain. `cargo clippy -p <crate> -- -D warnings` must be clean. `cargo fmt`.
- Only the integrator edits the root `Cargo.toml` and `Cargo.lock` deps list. Need a dependency or an API change in another crate? Report it, don't edit someone else's crate.

## Running the editor (GUI)

- `cargo run -p schemify-app -- edit crates/app/tests/fixtures/inv/top.shcomp` opens a cell (its project is the nearest directory with `schemify.project`); `edit <dir>` opens a project with a new schematic; `--no-default-ui` runs the bare core (window + canvas, mouse only) to prove it stands without the bundled UI plugin (D6). Keys are xschem's (D25; `/` toggles a key summary).
- The default UI is the wasm plugin `plugins/default-ui` (`ui.records`, `keymap.records`), embedded from `crates/app/assets/default_ui.wasm`. After editing it, run `plugins/build.sh` (rebuilds every guest, refreshes the asset, enforces < 200 KB) and commit the `.wasm`.
- Headless editing tests drive the real frame spine: `cargo test -p schemify-app`; goldens re-bless with `SCHEMIFY_BLESS=1`; the 100k-object budget check is `cargo test --release -p schemify-app --test perf -- --ignored --nocapture`.
- NixOS: winit `dlopen`s Wayland/Vulkan, so export `LD_LIBRARY_PATH` with the `lib` dirs of `nixpkgs#wayland`, `#libxkbcommon`, `#vulkan-loader` and `/run/opengl-driver/lib`. A screenshot without a desktop: `WLR_BACKENDS=headless WLR_RENDERER=pixman cage -- schemify edit FILE` in `nix shell nixpkgs#cage`, then `grim out.png` with `WAYLAND_DISPLAY` set to cage's socket.

## Definition of done (per change)

- Spec section exists and matches the code.
- Oracle-backed tests from TESTING.md for the feature (not just examples), including negative space.
- Public items documented per `doc-comments`; `cargo doc -p <crate>` warns nothing.
- Change radius stated in the commit/PR description.
