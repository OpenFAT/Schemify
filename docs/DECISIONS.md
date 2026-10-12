# Schemify — decision log

Settled in the design grill of 2026-10-10. Each entry: the decision, then why. IDs are stable; amend by appending a dated note, never by rewriting.

Evidence:
- [xschem-complaints.md](research/xschem-complaints.md) — what users dislike/like about xschem
- [xschem-feature-inventory.md](research/xschem-feature-inventory.md) — full parity spec (menus, keys, dialogs, subcommands, file format)
- [xschem-ui/](xschem-ui/README.md) — 42 screenshots + live menu dump
- [pdk-xschem-devices.md](research/pdk-xschem-devices.md) — PDK device survey for `tools/devicereader`
- [commercial-feature-parity.md](research/commercial-feature-parity.md) — Virtuoso/ADE-class features
- [file-format.md](research/file-format.md) — measured format study (`tools/fmtbench`)
- `../../Zig/TESTING-METHODOLOGY.md` — test methodology (binding, see D37)

## Goal

XSchem replacement: same functionality, modern, lighter. Neovim model: a lean base, plugins make it beautiful, but the base stands alone. Commercial-grade ("proprietary level") capability. Runs native and on the web from one Rust codebase.

## Licensing and provenance

| ID | Decision | Why |
|---|---|---|
| D1 | **MIT/Apache-2.0, clean-room.** No xschem (GPL) code is copied or ported, including `eval_expr.y` and `spice.awk`. Behaviour is specified from docs and verified against golden outputs produced by *running* xschem. | Keeps closed/commercial options open; GPL would infect in-process plugins. |
| D26 | Salvage from UW-ASIC/Schemify (own code, relicensed): `crates/wave` as-is; `wave_view.rs` logic ported to the new toolkit; optimizer → core; `gmid-lut` → plugin; `net2schem` → plugin. Agent/MCP not salvaged. | Proven code. Verify the UW-ASIC org holds no IP claim. |

## Platform and runtime

| ID | Decision | Why |
|---|---|---|
| D2 | Web: **generated glue only** (wasm-bindgen/web-sys + a one-line loader). No hand-written JS, npm, bundler or JS framework. | "No JS runtime" taken as far as browsers allow. |
| D3 | Plugins are **WASM run by wasmi everywhere** (native + web). wasmtime only behind a feature flag if a profile demands it. | One engine; no JS needed to load plugins; deterministic; small. Ceiling: interpreter speed; heavy compute stays in external tools. |
| D4 | **Custom UI toolkit**: taffy (layout), parley (text, IME), winit (windowing/input), AccessKit tree generated from the UI table. | Every UI element must be plugin-overridable with full styling; egui caps styling. |
| D5 | **vello** renders both UI and schematic canvas. | One renderer; GPU compute scales to huge canvases; vello_cpu/hybrid fallbacks share the scene API. |
| D32 | **Linux, macOS, Windows, Web all tier 1** (CI every commit, release-blocking). | xschem's install/platform pain is a top complaint. |
| D39 | **Performance budgets** (native mid-range laptop; web ≤ 2×): cold start ≤ 300 ms; open 100k objects ≤ 200 ms; pan/zoom ≥ 120 fps at 100k visible primitives; netlist 1M-instance flat ≤ 1 s; param edit → recompute + redraw ≤ 8 ms for 1k dependents; idle CPU 0%; native binary ≤ 25 MB; wasm ≤ 8 MB compressed; RAM ≤ 2× file size + 150 MB. | Turns "lightweight" and "instant netlisting" into tested numbers (nightly benchmark tier). |

## Plugins

| ID | Decision | Why |
|---|---|---|
| D3a | **One data-oriented ABI**: plugins read core tables (document, UI tree, netlist, results) and return **request tables** (UI edits, commands, document edits, run requests). The core owns every table and applies requests. | One interface covers UI, netlisting, simulation, checks; no vtables in core; single-owner rule. |
| D6 | The xschem-parity UI (menus, toolbar, tabs, dialogs) is a **bundled default plugin** built on the same ABI. Core ships the UI table, widget kinds, command registry, canvas. | Forces ABI completeness; users can replace the whole UI. |
| D7 | **Capability manifest**: plugins declare read/write project, `spawn <binary>`, network, clipboard; the user grants on install. **Documents are pure data**: commands that execute live only in runner plugins and user config, never in files. | Opening an untrusted file can't execute anything (xschem #492). |
| D8 | Conflicts: **ordered plugin list, last wins**, overridden edits recorded in a health report. | Deterministic, zero runtime cost, neovim semantics. |
| D35 | **Packages** (plugins *and* libraries): manifest sources `git`+`rev` / `url`+`sha256` / `path`; lockfile pins content hashes. No central registry. | One mechanism for both; no service to run. |

Plugin-controllable areas: UI (every element), commands and keybindings, edit tools/modes, netlisters (new `lang` tags), simulator runners and testbench code languages, import/export formats, PDK readers, ERC rules, constraint types, param callbacks and expression functions, waveform visualisations, event hooks (save, netlist, place, run).

## Files and data model

| ID | Decision | Why |
|---|---|---|
| D9 | **Format = sectioned line records** (measured, `file-format.md`): fixed section order (symbol interface → symbol graphics → schematic → bench code); instances as header line + indented field lines; wires/labels/pins one line each; records sorted by **random 6-char id**; net names only on labels/ports (connectivity from geometry); nothing derived stored (no offsets/counts/hashes/sidecar index); one escaping layer. Same syntax for config. | 1-line diffs for move/param/delete; net rename = label lines only; symbol header read in 5 µs at any size; 100k objects parse in 34 ms; block layout merges 50/50 driverless. |
| D10 | References are **`lib:cell`**; a project manifest maps lib → source; lockfile pins. Library = directory, one cell per file. No search path. | xschem's #1 complaint (global path, absolute paths, PDK updates break files). |
| D11 | Three file kinds, **one shape** `Cell { symbol?, impls[] }`: `.shprim` = symbol + ≥1 leaf impl (spice/PDK device, veriloga, verilog, …); `.shcomp` = symbol + ≥1 schematic impl + optional leaf impls (behavioural models) + docs; `.shbench` = one schematic impl + docs + bench tables, no symbol. Impl kind = `Schematic \| Text{lang}` (open tag). One impl marked `default`. Symbol pins are the contract every impl must match (checked on save). Shared param schema. | Behavioural ↔ transistor swap without separate symbol files. |
| D11a | **Impl selection only in the bench config**: instance-path override > per-cell override > view order > cell default. **Named configs** (e.g. `behavioural`, `signoff`) are a sweep dimension. | Instances never change to swap models. |
| D11b | **Only `.shbench` exposes Run.** A "New bench from this component" action creates a bench instantiating the cell. | Testing stays one click without making components runnable. |
| D33 | Coordinates are **i32 fixed-point, 1 unit = 1/1000 xschem unit**. Import rounds the 0.36% of corpus coordinates that are inexact (max error 0.0005 units) and lists them in the import report. | Exact connectivity, deterministic output; range ±2.1M covers corpus max 417k (1/32000 would not). |
| D40 | Instances store **param overrides only**. PDK pinned by lockfile hash; a lockfile bump runs a "default changes" report shown in the visual diff. | Smallest files and cleanest diffs without silent default drift. |
| D29 | Core connectivity: buses `a[3:0]`, iterated instances `x1[3:0]`, **global nets**, **inherited connections** (netSet-style net expressions, per-instance override), **signal domains** analog/digital/real on pins and nets (auto A/D bridges, ERC), **bundles/repeat** syntax. Cadence `<>` mapped on import. | Mixed-signal goal; PDK stdcell power pins; Virtuoso parity. |
| D30 | **Generic constraint objects** in core (type tag, params, member ids/paths): saved, diffed, revalidated after edits, drawn. Plugins define types and export. | Cheap now, a format change later. |

## Computation and gating

| ID | Decision | Why |
|---|---|---|
| D12 | **One pure, total expression language** for derived params, conditions, netlist templates, labels, docs vars, outputs/specs and the waveform calculator. Arithmetic, units/SI suffixes, string interpolation, if/else, min/max/round-to-grid; no loops, no IO. Seeded from UW-ASIC `wave/expr.rs`. WASM callbacks only for what expressions can't express. | Replaces xschem's `@subst` + `expr()` + `tcleval` "quoting hell"; files stay data. |
| D13 | Gating: **grid/step auto-snaps**; min/max/choices violations are kept but shown as live errors and **block Run** until fixed or **waived per instance** (waiver + reason saved in the file). Sweeps/optimizer clamp to schema bounds. | Never tape out a bad L by accident; still allows deliberate exploration. |
| D14 | `tools/devicereader` sources: **SPICE model bin ranges** (lmin/lmax/wmin/wmax), **xschem `drc=` procs** (pattern-translated, unknown → `needs_review`), **tech-file grid** (KLayout `.lyt` / Magic `.tech`), **Cadence `cdfDump`** (written from docs, untested — no sample available). No Tcl runtime: expression language + PDK variable table + known-proc rewrites. | Virtuoso-style schematic-level gating from PDK data beyond SPICE. |
| D19 | **Docs `var` blocks are the cell's variables table** — one table read by instance params, optimizer, sweeps, specs. A doc is ordered blocks: `prose` (markdown + LaTeX), `var` (`name = expr`, unit, bounds), later `plot`/`table`. Reactive recompute over one dependency graph (docs vars → instance params → derived params). | The doc can't disagree with the schematic. gm/Id sizing lives beside the circuit. |
| D20 | Plugins may **export pure functions** into the expression namespace (`gmid.*`). Computed values are **cached in the file** with the plugin version hash; without the plugin, files open/netlist/simulate on cached values marked *unverified*. | Shareable files without forcing everyone's plugin set. |
| D21 | Math: **LaTeX math subset as a core widget** (RaTeX parser + layout, drawn via parley/vello, OpenType MATH font). | Docs are part of every file, so they must render without plugins. |

## Simulation and results

| ID | Decision | Why |
|---|---|---|
| D15 | Native default runner: **ngspice subprocess from PATH** (`spawn ngspice` capability), stream stdout, read `.raw`. | Zero link deps; a crash can't kill the editor. |
| D16 | Web: edit, netlist, view results; **simulation via runner plugins** (e.g. remote runner with `network`). | No processes in browsers; no ngspice port to maintain. |
| D17 | `.shbench` = schematic + **structured tables** (design variables with bounds, model/corner sections, impl configs, outputs, specs) + **lang-tagged raw code blocks** (spice default; runner plugins claim other langs). Analysis engines edit tables, never parse text. | User's "any language, run however the plugin decides; default SPICE on the bench netlist". |
| D18 | **Sweeps, corners, Monte Carlo, optimizer in core**, each its own crate behind a cargo feature. Core job queue runs N local workers; remote/LSF via runner plugins. Results in `.schemify/results/` keyed by design hash (git-ignored). | User choice for integration; feature flags keep a light build possible. |
| D22 | Core netlisters: **SPICE (ngspice + Xyce dialects, incl. OSDI pre-load and d_cosim wiring), CDL, structural Verilog, Spectre.** Others are plugins. | Mixed-signal + LVS + commercial simulator users. |
| D23 | Core **waveform viewer + calculator** (raw reader ngspice/Xyce binary+ASCII, `graph` widget embeddable in schematic and docs, cursors, expression-language calculator incl. FFT, cross-probe, OP back-annotation overlay, results history). Eye/Smith/histograms are plugins. | Users like xschem's embedded graphs; ported from UW-ASIC. |

## Interop, UX, safety

| ID | Decision | Why |
|---|---|---|
| D24 | xschem interop is **import only** (designs + PDK libs). | Our model has no xschem equivalent for docs/impls/waivers. |
| D25 | Default keymap = **xschem's** (165 combos); bundled **Virtuoso preset**. | Migrating users keep muscle memory. |
| D27 | Headless: **CLI** (`import`, `netlist`, `check`, `run`, `export`, `diff`, `merge-driver`) with real exit codes + **one JSON-RPC command stream** (stdio/socket) over the same command table the UI and plugins use. MCP = thin plugin. | xschem hangs on script errors; one API, not two. |
| D28 | **Edit journal** (append-only sidecar, not committed): per-document undo that survives hierarchy navigation and restarts, crash recovery, session restore. Watched files: silent reload if no local edits, else **3-way semantic merge** keyed on stable ids; only true conflicts prompt. | Undo loss and overwrite-on-save complaints. |
| D31 | Web storage: **OPFS** VFS; projects in/out via zip or File System Access folder picker; PDKs via zip or URL fetch into OPFS. Git stays native. | Persistent without a server. |
| D34 | **`schemify merge-driver`** (via `.gitattributes`) + **`schemify diff`** + on-canvas visual diff (added/removed/moved/param-changed, also in docs). | 350/350 clean merges measured; "diffs are coordinate noise" complaint. |
| — | ERC: core check framework + built-in rules (dangling pins, shorted labels, domain mismatch, param range, impl pin contract, missing lib); plugins add rules; severity configurable per project. | Default, unchallenged. |

## Process

| ID | Decision | Why |
|---|---|---|
| D36 | **M1 headless**: file format, devicereader, xschem importer, SPICE/CDL netlisters, CLI — validated by golden tests against xschem output on sky130/gf180/IHP/examples. **M2 GUI slice**: toolkit + default-UI plugin, open bench → edit → run → waves. | Prove the data model before pixels. |
| D37 | **Testing methodology is binding** (oracles before coverage; differential vs xschem/ngspice; metamorphic; stateful PBT for undo/journal/merge; fuzz every parser; determinism across 4 platforms and `jobs=1` vs `N`; TigerStyle asserts; tiers pre-commit/merge/nightly/release; release exit criteria). Every feature lands with its oracle. | See methodology doc; oracle table in the grill transcript is to be copied into `TESTING.md` at M1 start. |
| D38 | Change drills for the architecture: **layout handoff** (schematic-driven layout: export connectivity + constraints, LVS cross-probe back) and **AI sizing assistant (plugin)**. | Validate change radius before code. |
| — | Architecture follows `codebase-clean-code`: tables, transforms, one spine per pipeline, single owner per table, variation by data (compile-time generics only). An abstraction is allowed only at zero or negative overhead **and** when it unifies several interfaces into one. | User rule. |

## Amendments

| Date | ID | Amendment |
|---|---|---|
| 2026-10-10 | D18 | Results live under `.schemify/runs/<design hash>/` (job directories + `results` file), not `.schemify/results/`: one directory per design hash makes stale-result detection and cleanup a directory operation. Still git-ignored. |
| 2026-10-10 | D33 | Unchanged unit (1/1000 xschem unit); the importer measured 0.52% inexact coordinates on the larger 526k-coordinate corpus (max error 0.0005 units). |
| 2026-10-11 | D26 | net2schem is replaced by the `cktimg` plugin (SPICE → Schemify via `extern/cktimg`, user decision): cktImg places and routes; the plugin keeps the Schemify side (core-cell recognition, hierarchy, code blocks, ids, file writing). |
| 2026-10-11 | D9 | User config is **Lua** (`init.lua`), not line records (user decision): a scriptable config like Neovim's. Design files, project manifest and lockfiles stay line records. The config reaches the app only through a sandboxed `schemify.*` API over the D27 command table (options, keymaps, event hooks, simulator commands, packages); no `io`, `os.execute`, `debug` or native modules. Plugins stay wasm. |
| 2026-10-11 | D35 | User-level packages: `init.lua` declares plugins with `schemify.pack.add{…}` (lazy.nvim-style specs: `git`+`rev` / `url`+`sha256` / `path`, capability grants, lazy-load on command or event). They install under the data dir, are pinned by a lockfile next to `init.lua` (meant for dotfiles), and are managed by `schemify pack sync\|update\|restore\|clean\|status`. Still no central registry. Directories follow Neovim's `stdpath`: config `$XDG_CONFIG_HOME/schemify` (default `~/.config/schemify`, on macOS too), data `$XDG_DATA_HOME/schemify`; Windows `%LOCALAPPDATA%\schemify` and `%LOCALAPPDATA%\schemify-data`; web OPFS `/config`, `/data`. |
| 2026-10-11 | D39 | Binary size (user decision, Neovim's shape): `schemify` is a headless core (CLI, command stream, config, package manager, plugin host) of **≤ 2 MB**; the editor is a separate binary of **≤ 10 MB** (`schemify edit` starts it). Replaces "native binary ≤ 25 MB". Measured 2026-10-11 at 23 MB, dominated by an embedded full NotoSans (1.6 MB), the wgpu/naga/vello stack, AccessKit's D-Bus client, LaTeX + `regex`, and generic monomorphization. |
| 2026-10-11 | D17 | `code <name> <lang>` records are also allowed in a `.shcomp` (the recommendation in BACKLOG "Decisions pending", taken when the user asked to close every backlog item; reviewable in its PR): netlist text that belongs to a component (`.param`, `.include`, `.model`, XSPICE `A` cards) is emitted inside every specialization of the component's subcircuit. The bench-only tables (corners, configs, sweeps, Monte Carlo, optimize, outputs, specs) stay `.shbench`-only. |
| 2026-10-12 | D9 | The Lua runtime is **`omnilua`** (pure Rust, Lua 5.4, passes the PUC-Rio suite) on every target, replacing mlua's vendored C Lua (user decision): one runtime gives the user config identical behaviour natively and in the browser (D32), where C Lua cannot build. Accepted risk: a young crate (0.7.x, one maintainer). |
| 2026-10-12 | D39 | Binary sizes are **informational, not budgets** (user decision): the earlier figures (core ≈ 2 MB, editor ≈ 10 MB) were a rough indication of where the binaries should land. `tools/size-check.sh` reports sizes and never fails on them; CI and releases do not gate on size. The other D39 budgets (frame time, open, netlist, param edit, idle CPU) stay. |
| 2026-10-12 | D10, D24 | PDKs are installed **per project** (`.schemify/pdks/<family>/<version>/`) by a `pdk-manager` plugin driving **ciel** (sky130, gf180mcu, ihp-sg13g2; git recipes such as ASAP7 later), switched by rewriting the manifest, removed when neither active nor locked. Designs can retarget **across technologies** through technology classes (`tech:nmos`, `tech:pmos`, … resolved by `lib tech map <family>` and the PDK's `tech.map` parameter expressions); direct PDK cells stay technology-specific (user decision). Spec: [spec/pdk.md](spec/pdk.md). |
