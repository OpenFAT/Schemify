# Contributing

Rules for everyone who changes this repository, people and AI agents alike. The root `CLAUDE.md` imports this file and `AGENTS.md` points to it, so every agent reads it at startup.

Schemify is a schematic editor and SPICE front end where a wrong number is worse than a crash and a slow frame is a bug. Five commitments decide every change:

1. **Lean**: anything that exists a million times (objects, rows, samples) is as small as its stated range allows.
2. **No residue**: an abstraction may cost compile or test time, never a cycle the hand-written loop would not spend.
3. **Cheap change**: a likely change lands as new rows, variants, transforms or spine lines, and the compiler lists every other site.
4. **Contained failure**: an expected error returns to the caller that can decide; a bug hits an assert and stops before it corrupts anything downstream.
5. **Proof by test**: every behaviour has an independent oracle that turns red when the behaviour breaks.

Fast and lightweight are the same goal here: fewer bytes per row, fewer passes per frame, fewer crates rebuilt per edit.

## Read before touching code

- [architecture.md](architecture.md): crates and their layers, owners, spines, tables with capacities, invariants. Every feature names the owner it writes and the spine line it adds.
- [decisions.md](decisions.md): the settled decisions (`D1`–`D41` plus dated amendments). Append an amendment to change one.
- [../spec/](../spec/): the normative specs. Code follows the spec; a wrong spec is fixed in the same change.
- [testing.md](testing.md): the binding test plan (D37): oracles per area, tiers, release exit criteria.
- [perf.md](perf.md): the D39 budgets and how each was measured.
- [status.md](status.md) and [backlog.md](backlog.md): what is done and what is open.

## The shape of the code

The crates sit in four layer folders, and a crate depends only on its own layer or a lower one (`crate_folders_depend_only_downward` in `crates/model/cli/tests/deps.rs` enforces it):

| Layer | Crates | Holds |
|---|---|---|
| `crates/protocol/` | `abi`, `format` | Wire and file formats every side shares: the plugin ABI, the line-record codec. Leaves. |
| `crates/plugins/` | `plugin` | The wasm plugin host: capabilities, snapshots, request intake. |
| `crates/model/` | `expr doc elab netlist wave run config cli ui canvas editor` | Owned tables and the transforms over them, the CLI and editor spines. All headless: no GPU or window crate (`tests/deps.rs`). |
| `crates/view/` | `render`, `app` | The humble view: pixels, the window, paint caches. |

The editor follows the one-way GUI architecture (`gui-architecture` skill, D4/D6/D39):

- **Owner**: each table has one writing crate (`pub(crate)` mutation). Anyone else appends to a **request table** the owner drains in spine order.
- **Spine**: one function per pipeline, one line per transform (`Editor::update` for a frame, the CLI's `spines`). Branches that choose which work happens sit in the spine; transforms are straight loops over whole tables.
- **Humble view**: `app::paint` reads `Editor::picture` (borrowed, read-only tables) and reports back only through `Editor::painted`. A decision worth a test lives in the model as a pure function, never in the view.
- **Idle at zero**: frames run on input, a worker's post, or a deadline from `wake_ms(&tables)`; with nothing pending the window sleeps.

Place a file in the lowest crate that can import everything it needs and that owns the tables it writes. A helper that touches one owner's tables goes into that owner; a type several owners share goes into a leaf. Topic folders (`utils`, `common`, `helpers`) close cycles, so code is grouped by owner instead.

Paths name their anchor, so a crate can move without edits: a crate's own files from `concat!(env!("CARGO_MANIFEST_DIR"), "/…")`, any other repository file from `concat!(env!("SCHEMIFY_ROOT"), "/crates/…")` (set in `.cargo/config.toml`). The plugin repositories compile `protocol/`, `plugins/` and the model's domain crates as git dependencies without this repository's cargo config, so `SCHEMIFY_ROOT` appears only in tests and in `ui`, `editor` and `app` (`paths_are_anchored_at_the_crate_or_the_root` in `tests/deps.rs`). Cargo manifests have no root variable: their few `path =` entries stay relative.

## The order of a change

Each step's done condition is in the skill named beside it.

1. **Data** (`data-oriented-design`): answer the six questions in writing; SoA by default, narrowest widths from the spec, existence-based tables, index handles.
2. **Owner and spine** (`codebase-clean-code`): name the table's owner, the request table any outside writer uses, and the spine line. State the **change radius** (existing function bodies edited) as a number.
3. **Seams** (`codebase-clean-code` ladder): a seam needs two cases today or a drilled change; it sits on the cheapest rung that holds. Rung 5 and above enters a hot path only with an `asm_probe` verdict (`rust` skill, `zero-cost.md`).
4. **Contracts** (`assertions`): entry and exit asserts, capacities, bounded loops.
5. **Oracle** (`test-plan`, [testing.md](testing.md)): the test that goes red when the behaviour breaks, negative space included.
6. **Branches, then lanes** (`branchless`, then `simd-loops`): only for a measured hot loop. Delete the branch first (split the table, sort, hoist), make it branchless second, vectorize third. The scalar loop stays as the tail and the oracle. A speed claim cites a benchmark; a zero-cost claim cites the probe.

Conflict rule: data layout beats code brevity; `ponytail` decides how little code to write, never what shape the data takes. The remaining rulings are in `codebase-rules`.

## Skills to load first

In Claude Code they are user skills (`Skill` tool); other agents read `~/.claude/skills/<name>/SKILL.md`.

| Skill | Use it for |
|---|---|
| `data-oriented-design` | Every struct or table layout. |
| `codebase-clean-code` | Owners, spines, seams and their rungs, change drills; which crate or file a feature goes in. |
| `gui-architecture` | Anything in `editor`, `ui`, `canvas`, `render` or `app`: owned tables, request tables, the humble view, canvas from tables, 0% idle, headless frame tests. |
| `rust` | Edition 2024 on 1.95: newtypes, typed handles, niches, exhaustive matches, arenas, the `asm_probe`. Explicit SIMD is `fearless_simd` (`simd-first.md`). |
| `branchless`, `simd-loops` | A hot loop over large data (parsers, connectivity, netlist emission, scene stamping, hit tests, waveform columns), in that order. |
| `assertions` | Rule 5 below. |
| `doc-comments` | Every `pub` item: each `///` states a precondition, invalidation, ownership, failure, cost or reason the signature cannot. |
| `test-plan` | Choosing the oracle and the tier for a new behaviour or a bug. |

UI and visual work has its own guides under [skills/](skills/), read as documents:

| Guide | Use it for |
|---|---|
| [analog-eda-ux](skills/analog-eda-ux/SKILL.md) | Designer-taste rules and the review checklist for any UI surface. |
| [schemify-theme](skills/schemify-theme/SKILL.md) | Colour and chrome tokens; `python3 docs/dev/skills/schemify-theme/check_theme.py` gates them. |
| [schemify-ui-loop](skills/schemify-ui-loop/SKILL.md) | The edit, render, critique, bless loop for the default UI's look. |
| [schemify-symbols](skills/schemify-symbols/SKILL.md) | Drawing rules for the device symbols in `crates/model/doc/devices/`. |
| [schemify-plugin](skills/schemify-plugin/SKILL.md) | Starting a plugin in any language from a template in `examples/plugins_new/` (linked into `.claude/skills/`, so Claude Code loads it). |

## Hard rules

1. **Clean room** (D1). Behaviour comes from `docs/spec/` and from golden outputs produced by running xschem. Never open, copy, port or paraphrase xschem source (`*.c`, `*.y`, `*.awk`, `*.tcl`) or any other GPL code.
2. **Zero-overhead abstractions** that merge several interfaces into one, from the list in [architecture.md](architecture.md) § Unifying abstractions. Static dispatch in the core; the plugin boundary is the one runtime dispatch.
3. **Single owner.** Only the owning crate writes its tables; other crates send request tables. Layers depend downward only.
4. **Determinism.** The same input gives byte-identical output on Linux, macOS, Windows and the web: ordered collections in output paths, i32 milli-unit coordinates where the spec says i32 (D33), fixed reduction order.
5. **Asserts** (TigerStyle). At least two per function on average: entry invariants and one exit invariant, positive and negative space, split rather than compound. Every table has a stated capacity; every loop and queue is bounded.
6. **Safe Rust**: `#![forbid(unsafe_code)]`, unless a profile proves the need and a `// SAFETY:` comment states the obligation.
7. **Dependencies** climb the ladder: stdlib, an existing dependency, a few lines of our own, then a new dependency justified in the PR. GPU and window crates (vello, wgpu, winit) live in `crates/view/`, wasmi in `plugin`, Lua in `config`; `tests/deps.rs` checks the headless core and the model.
8. **Inert design files** (D7): nothing in a `.sh*` file can run a command.
9. **Exhaustive boundary matches**: a `match` over an enum that crosses a crate boundary lists every variant, so a new variant is a compiler-listed set of sites.
10. **Marked simplifications**: `// ponytail: <ceiling and upgrade path>`.

## Tests

[testing.md](testing.md) is binding; these habits keep it honest:

- **Oracle first.** A test names what makes its answer independent: a spec value, a reference tool (ngspice, Xyce, `spicecmp`), a round trip, a metamorphic relation, a certificate, the scalar loop, a golden with recorded provenance. A test that recomputes the answer the way the code does passes by construction.
- **Every bug buys an oracle.** A bug from a user, a developer or a review is evidence the oracle set is incomplete: it lands with a regression test and, where one applies, a new property, generator feature or invariant that would have caught the whole class.
- **Main is green.** A flaky test is fixed or quarantined the same day, with a reason in the test. Known gaps are annotated by name with their cause, and an annotated test that starts passing fails until its note is removed.
- **Ratchets move one way.** Coverage, the perf baseline and the fuzz corpus are raised, never lowered; a lowered baseline needs a measured reason in the PR.
- **The frame is testable headless.** Editor behaviour is asserted on tables through the real frame spine (`crates/view/app/tests/common`); pixels only in goldens.

## Building

- Edition 2024, stable toolchain pinned to `rust-version` in `Cargo.toml`. `nix develop` gives the pinned toolchain (rust-lld included for the wasm guests), the simulators and the GUI libraries.
- Test the crates you changed: `cargo test -p schemify-<crate>`; `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo fmt` are clean.
- Root `Cargo.toml` dependency changes and `Cargo.lock` go through the maintainer. A needed dependency or an API change in another crate is stated in the PR.

### Fast incremental builds

Cargo rebuilds per crate with no early cutoff: an edit rebuilds its crate and every crate that depends on it. So:

- **Thin, hot leaves; stable interfaces inward.** New code lands in the crate that owns it, so an edit stays off the trunk of the graph; a crate split is justified only by ownership, a heavy dependency or a platform (architecture.md).
- **Flags live in files.** Profiles and lints are set in `Cargo.toml`; a one-off `RUSTFLAGS` in a shell rebuilds the whole graph twice (into it and back out).
- **One mode per loop.** `cargo check` and `cargo build`/`test` keep separate artifacts; the edit loop runs `cargo test -p <crate>` (or `check`), not both alternately.
- **The editor gets its own target directory** (rust-analyzer's `cargo.targetDir`), so it never holds the build lock.
- `--release` only for the perf checks that need it. `cargo clean -p <crate>` (or `cargo sweep`) once a large build will not be reused: nothing collects `target/` on its own.

### Memory limits on the shared build machine

The machine has 31 GB of RAM shared by several jobs. Run every build, test or tool through a memory-capped cgroup:

```sh
systemd-run --user --scope -q -p MemoryMax=6G -p MemorySwapMax=0 <command>
```

7G when at most three jobs share the machine, 6G with four. Exit code 137 means the cap killed the process: find the cause rather than raising the cap. Cap parallelism with `export CARGO_BUILD_JOBS=3` (4 with three jobs or fewer) and `cargo test -- --test-threads=4`. Check `free -g` before a release, LTO or wasm guest build, wait below 12 GB available, and run one heavy job at a time.

### Wasm guests

The default UI and the host's test guests are wasm modules committed next to their users; CI's `guests` job rebuilds them and fails when a committed module differs. After editing a guest, `crates/protocol/abi`, or moving either (panic locations embed the source path):

```sh
nix develop -c ui/build.sh
```

and commit every `.wasm` it changes. Without Nix, download the `guests` artifact of the PR's CI run (`gh run download <run> -n guests -D <tmp>`, then `cp -r <tmp>/. .`). The plugin repositories under `examples/plugins/` pin a Schemify revision and test against `main` nightly ([../plugins/testing.md](../plugins/testing.md)), so an ABI change reaches them when they bump.

## Branches and releases

- `main` is the integration branch (`origin` = github.com/OmarSiwy/Schemify, public) and is always green: work lands through a PR whose merge result passed the merge tier. CI work may push `ci/*` branches. One branch per change; delete worktrees when done.
- Generated files on `main` are the ones CI regenerates and compares (the wasm guests, goldens blessed with `SCHEMIFY_BLESS=1` and reviewed).
- Release notes are written by hand, one line per change, and every number in them is measured by the release job, never typed ([testing.md](testing.md) § Release exit criteria).

## Running the editor

- `cargo run -p schemify-app -- edit crates/view/app/tests/fixtures/inv/top.shcomp` opens a cell; its project is the nearest directory with a `schemify.project`. `edit <dir>` opens a project with a new schematic. `--no-default-ui` runs the bare core (window and canvas, mouse only), which proves the editor stands without the default UI plugin (D6).
- `cargo test -p schemify-app` drives the real frame loop headlessly. `SCHEMIFY_BLESS=1` re-blesses the golden PNGs in `crates/view/app/tests/golden/`. The 100k-object budget check is `cargo test --release -p schemify-app --test perf -- --ignored --nocapture`.
- On NixOS, winit loads Wayland and Vulkan at runtime: export `LD_LIBRARY_PATH` with the `lib` directories of `nixpkgs#wayland`, `#libxkbcommon`, `#vulkan-loader` and `/run/opengl-driver/lib`. For a screenshot without a desktop, run `WLR_BACKENDS=headless WLR_RENDERER=pixman cage -- schemify edit FILE` inside `nix shell nixpkgs#cage`, then `grim out.png` with `WAYLAND_DISPLAY` set to cage's socket.

## Definition of done

- The spec section exists and matches the code.
- The behaviour has its oracle-backed tests, negative space included, and each test goes red when its subject breaks.
- Public items are documented per `doc-comments`; `cargo doc -p <crate>` prints no warnings.
- A hot-path change cites its benchmark against the D39 budget; a zero-cost claim cites its probe.
- The commit or PR states the change radius.
