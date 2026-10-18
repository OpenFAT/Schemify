# Architecture (draft, pre-code)

Schemify turns `lib:cell` files (`.shprim`, `.shcomp`, `.shbench`) into netlists, simulation jobs, results and an interactive picture of all three. Everything is tables owned by one crate each, transforms over whole tables, and one spine per pipeline. Decisions referenced as `D#` are in [docs/dev/decisions.md](decisions.md).

Rule for every abstraction: allowed only at zero or negative overhead **and** when it merges several interfaces into one. The ones that pass are listed in [Unifying abstractions](#unifying-abstractions); anything not on that list is not an abstraction we have.

## Crates

Crate = owner. A crate boundary exists only where it enforces single ownership (other crates get `&T`, never `&mut T`), gates a heavy dependency, or splits by platform.

| Crate | Owns (writes) | Reads | Heavy deps | Notes |
|---|---|---|---|---|
| `format` | — (codec) | bytes | none | Line-record codec (D9): parse, canonical write, `merge3`, record diff. Used for documents, config, manifest, lockfile, journal. |
| `expr` | — (pure) | source text | `libm` | The one expression language (D12): lex → parse → check → compile → eval. Scalar mode (params, docs, templates) and column/complex mode (waveform calculator, with its own radix-2 + Bluestein FFT). Seeded from UW `wave/expr.rs`. Transcendentals and FFT twiddles via pure-Rust `libm` for bit-identical results on all targets (D32); complex numbers are its own `f64` pair (`dmath::Complex`). |
| `doc` | `cells`, `symbols`, `pins`, `graphics`, `param_schema`, `impls`, `instances`, `inst_params`, `wires`, `labels`, `ports`, `texts`, `shapes` (schematic graphics), `doc_blocks`, `bench_*`, `constraints`, `waivers`, `staged_edits`, `journal` | `edit_requests` | `format` | The only writer of design data. Load/save, edit transactions (commit or stage), undo/redo, lib resolution via manifest + lockfile (D10, D35). Bundles the `core` library (`core_library`, `lib core builtin`, D10). |
| `elab` | `expr_graph`, `values` (`Graph`), `nets`, `pin_nets` (`CellNets`), `defs`, `def_insts`, `leaf_impls` (`Elab`: hierarchical, one row per specialization), `violations`, `annotations` | doc tables, `annotation_requests` | `expr` (`format` for doc's column types) | Everything derived: dirty-graph recompute (D19), connectivity from geometry, hierarchy elaboration with bench-config impl selection (D11a) and subckt specialization, checks/gating (D13), one annotation overlay. Paths are not materialized: a path is the walk of `defs`/`def_insts` from the top. |
| `netlist` | `netlist_text` (job arena), `name_map` | elab tables only | — | Emitters: `Dialect = Ngspice \| Xyce \| Cdl \| Verilog \| Spectre`, closed enum, exhaustive match (D22). Plugin dialects go through `run_requests`. |
| `wave` | `raw_plots` (column SoA), viewer tables (`traces`, `panes` with their properties, x view, cursors) | `.raw` bytes, `wave_requests` | none (`expr`) | Reader for ngspice/Xyce/LTspice binary+ASCII (UW salvage, D26, hardened). Calculator = `expr` calc mode over borrowed raw columns. SIMD min/max display decimation, digital runs and histogram bins (spec `docs/spec/waves.md`). `Waves::apply(Request)` is the viewer tables' only writer (D23); `Waves::reload` swaps in new results keeping the traces. |
| `run` | `jobs`, `results` | bench tables, elab, netlist | features: `sweep`, `corners`, `mc`, `optimize` | Job expansion (sweep × corner × MC × named config), local worker queue, native subprocess runner (D15), result tagging by design hash, outputs/specs eval, optimizer ask/tell (UW salvage). |
| `abi` | — (types) | — | none | Versioned plugin ABI: table snapshot layouts, request-table layouts, capability ids. Shared by host and guests; doubles as the Rust plugin SDK. |
| `plugin` | `plugins`, `grants`, `request_queues`, `health` | snapshots of all tables | `wasmi` | Host: load, capability enforcement (D7), snapshot export, request intake, last-wins ordering + health report (D8). |
| `ui` | `ui_nodes`, `ui_styles`, `keymap`, `ui_layout` (frame) | `ui_edit_requests`, input events | `taffy`, `parley`, `accesskit`, `ratex-*` | UI-as-data (D4, D6). Widget kinds (incl. `math`, `graph`, `prose` = markdown + math wrapped to its width, multi-line `text_area`, scrolling `scroll`, `dropdown` popups, a `split` with a draggable divider; buttons draw vector `icon`s) are a closed enum. Input routing (pointer, keyboard menus, typing as `input` commands) → `command_requests`. The app state contribution's free text is interned per generation (`records::parse_live`). |
| `render` | `scene` (frame), `camera` | doc, elab, ui_layout, results | `vello`, `wgpu` | The only crate touching the GPU. Draws canvas, UI, graphs, visual diff and annotations (D5). A humble view: it places what `ui` decides (row colours `row_paint`, tick labels and digits, the waves readout, legend and measurement strings, `measure_at`); nothing in it needs a unit test. |
| `config` | the user-config table (options, keymaps, sims, package specs, hooks) | `init.lua` + `lua/` modules | `omnilua` (pure-Rust Lua 5.4, every target) | D9/D35 as amended: `stdpath` directories, the sandboxed Lua state (instruction budget, memory cap, no `io`/`os`/`debug`), the `schemify.*` API ([config.md](../spec/config.md)). Only this crate touches Lua; callers read the table and drain request lists. The web reads `init.lua` from OPFS (`Config::load_with`). |
| `cli` | user packages on disk (`<data>/pack`, `schemify-pack.lock`) | everything headless | — | The headless core, binary `schemify` (D39 ≤ 2 MB): the CLI spines, `libs` (load spine), `platform` (the OS boundary; on the web OPFS behind a synchronous in-memory table, the Clipboard API and downloads, [web.md](../spec/web.md)), `zip` (project archives), `spines` (netlist/run steps shared with the editor), `pack` (package manager: `git`/`curl` as child processes, sha256 + lockfile rows from `doc`), `rpc` (the D27 JSON-RPC command stream: codec, line framing, local sockets, and `schemify rpc`'s headless server over the CLI commands and `edit`; [rpc.md](../spec/rpc.md)), `mcp` (`schemify mcp`: the same methods as MCP tools, D27). `schemify edit` runs `schemify-gui`. Links no GPU/window/text/accessibility/LaTeX crate (`tests/deps.rs`). |
| `app` | `commands` (registry), `session` (project, open documents, tabs + their docs toggle, hierarchy stacks), `selection`, `tool_state`, `canvas_prims` + `objects` (the active document drawn, derived), `symbols` (cache), clipboard (+ the system clipboard as line-record text, `clip`), recent files (`platform::state_dir`), the command queue (`Editor::command`, the CLI/JSON-RPC entry, D27), dialog state (incl. the canvas context menu, positioned by `UiState::set_dialog_at`), view options (`EditOpts`: layer visibility, colors, widths, crosshair, symbol display), docs page state (`docs::Page`), net highlight + decor prims (`checks`), the run in flight (`sim::Sim`: run queue, plot slots; jobs/results stay `run`'s), the editor's `elab::Annotations` instance | everything | (`winit` through `render::run`) | The editor, binary `schemify-gui` (D39 ≤ 10 MB): the `frame` spine over `cli`'s spines, `libs` and `platform` (re-exported). User packages load through the plugin host between the default UI and the project's plugins; the user's Lua keymaps and hooks run through it (`plugins::Host`). `--rpc <socket>` serves the JSON-RPC command stream over the same command table (`rpc`: requests queued through `Editor::command` between frames, editor events as notifications, D27). |

Outside `crates/`: `tools/fmtbench` (format benchmark), `tools/spicecmp` (semantic SPICE/CDL comparator, the netlist oracle of docs/dev/testing.md; it depends on no workspace crate, so it cannot share the bugs it judges), `ui/default-plugin` (D6). The optional plugins are standalone repositories since 2026-10-18 (docs/plugins/), checked out as submodules under `examples/plugins/` and never built here; each depends on the core crates it uses through a git dependency pinned to a Schemify revision: `examples/plugins/interop` (D24 as amended 2026-10-12: interop is a plugin, never linked into the core; the clean-room xschem reader, attribute/expression translation and PDK constraint readers (D14) today, Virtuoso import/export and xschem export to come; the same code runs as the native CLI `schemify-interop` (feature `cli`), which hosts the nightly xschem differential), `examples/plugins/GmIDVisualizer` (D20/D26: `gmid.*` expression functions over gm/Id lookup tables, characterized through `spawn:ngspice`; loaded by a project manifest, not embedded), `examples/plugins/cktImg` (D26 as amended 2026-10-11: SPICE → Schemify import; a thin safe wrapper over cktImg's C ABI (`ffi` module, the crate's only `unsafe`) plus the glue that maps devices onto `core` cells, tells cktImg their pin geometry and writes its placement as records; its `build.rs` compiles the repository's Zig engine, `engine/`, with Zig 0.17; the C ABI and LaTeX package are its secondary interfaces), `examples/plugins/pdk-manager` (per-project PDK installs through `spawn:ciel`/`spawn:schemify-interop` and the `pdk.*` commands and panel, [pdk.md](../spec/pdk.md); loaded by a project manifest).

Dependency direction (no cycles; arrows mean "depends on"):

```
format   expr   abi                      (leaves, zero internal deps)
  ↑   ↖  ↑  ↑     ↑
 doc ── elab ──── wave
         ↑   ↖     ↑
        netlist   run
            ↖      ↑
   plugin   ui   render
       ↖     ↑    ↗
           app
```

`ui` and `render` also depend on `wave` (the waves window and its panes); `wave` depends on `expr`. `cli` depends on `netlist run config` (with `doc elab expr format`); `app` depends on `cli`; `config` is a leaf.

Two binaries (D39 as amended): the core `schemify` = `cli` over `format expr doc elab netlist run config`; the editor `schemify-gui` = `app`, which adds `abi plugin ui render` on top of `cli`.

## Spines

Each spine lives in `app` as one function, one line per transform. Branches that choose work (command dispatch, gating) sit in the spine, never inside a transform.

- **`frame`** (event-driven; nothing runs when idle, D39):
  `ui::route_input(ui_layout, keymap, events) → command_requests` →
  `app::dispatch(commands, command_requests) → edit_requests, ui_edit_requests, run_requests, plugin_events` →
  `plugin::deliver(plugins, plugin_events, snapshots) → request_queues` →
  `doc::apply_edits(edit_requests ∪ plugin edits) → doc tables, journal` →
  `ui::apply_ui_edits(ui_edit_requests ∪ plugin ui edits) → ui_nodes` (ordered, last wins) →
  `elab::CellState::recompute(project, doc, ops) → values, nets, violations` (per open document; value edits re-evaluate only dependents) →
  `elab::apply_annotations(annotation_requests) → annotations` →
  `run::poll(jobs) → results` →
  `ui::layout(ui_nodes, ui_styles) → ui_layout` →
  `render::build_scene(doc, elab, ui_layout, results) → scene` → `render::submit(scene)`

  As implemented in M2 (`crates/app/src/editor.rs` `Editor::update` + `paint`, integration steps in `editor/integrate.rs`):
  0. waves dock: when open, the window splits (editor above, `WavesView` below); pointer events go to the side they are over, keys to the side the pointer was last over, `Ctrl+W` to the dock
  1. `ui::route_input(ui_layout, keymap, events) → command_requests, canvas events`
  2. `app::dispatch(commands, command_requests) → edit_requests, plugin_events, ui state` — core commands are the closed `Cmd` enum, plugin commands become `Event::Command` to their owner (D27); `n`/`Shift+N` netlist (`spines::elaborate_all` → gate → `netlist::emit` → `.schemify/netlist/<cell>.<ext>` + a read-only text tab), `s`/Optimize start a run (`sim::Sim::start`: gate + design hash only, nothing waits)
  3. `app::gesture(tool_state, selection, canvas events) → edit_requests` — the xschem tool state machine; one transaction per gesture, so one undo; with the waves dock open a click on a net or branch-device pin cross-probes `v(<net>)` / `i(<dev>)` through the name map
  4. `plugin::Host::poll(now) → plugin_events` (child-process `IoDone`, due `Timer`s) → `plugin::deliver(plugins, plugin_events, snapshots) → request_queues` → plugin edits join `edit_requests`, `run` requests join the run queue, logs go to the status line, file I/O completes in the frame and `spawn` starts a worker (`platform::spawn_start`)
  5. `doc::apply_edits(edit_requests) → doc tables, journal` (+ `DocChanged` events; steps 4–5 run twice at most so plugins answer in the same frame)
  6. `app::load_symbols(active doc)` — refreshes the symbol cache from the library index (`Project::interface`: an open cell draws its edited document, any other its indexed symbol), opening nothing; elaboration and the run's stale check open the cells they reach
  7. `elab::CellState::recompute(project, active doc, committed ops, plugin functions) → values, nets, violations` — every shown document (D13 live checks, D19 values); incremental per commit and per undo/redo of one of the last 64 commits (over that entry's ops), a full `build` after older undos, other documents' edits or a plugin change. Plugin functions are called through `plugin::Plugins::call` (D20)
  8. `docs::cache_ops(values) → doc::apply_edits` — the D20 `cached` write-back (own params and vars), extending the triggering docs-page edit's undo entry and journal record (`Edit::Extend`, `docs/spec/docs-page.md` §6); then `elab::Annotations::apply(checks request)` — the `checks` source of the overlay
  9. `sim::Sim::{serve_queue, poll}` — plugin run requests become jobs; ≤ 8 jobs prepared per frame; `run::Runner::poll` (never waits); optimizer `OptRun::step` (ask → job → tell); `RunDone` queued for requesting plugins; on idle the results file is saved and the optimizer's best point is staged (`doc` stage); stale check (design hash) after edits; plot slots and the `op` overlay source from the selected job's raw file
  10. `app::ui_inputs` (the plugin ui+keymap sets when their composition changed, with the command rows; the docs page follows the active document; its prose images are read once per path, a failure goes to the status line) → `ui::compose(core base, plugin ui+keymap sets, app state) → ui_nodes, ui_styles, keymap` (last wins, D8) → `ui::layout → ui_layout`. The app projects its own state (tabs, status line, dialog rows, **the docs page** of the active level under `docs.page` with filled plot slots as `graph` nodes and the staged-edit banner, the checks panel or results table under `bottom`, a netlist text tab replacing `canvas`; the results table's rows and the text tab's lines are windowed by their scroll offset, spacers standing in for the rest, through `ui::records::window` and `spacer`, so their node and shaped-line counts are bounded by the screen) as the *last* contribution, made of `set`s and child nodes under plugin-declared containers; the core base (window + canvas | docs pane, `bottom`) applies only when no plugin declares a `window`; then the canvas palette is re-read from the composed styles when the composition changed. `compose_ui` is a projection (no I/O, no model table written), so it also runs at the top of the frame (after `ui_inputs`, so step 1 routes over what is shown) and again after a level's first fit
  11. `scene::build(doc, symbols, values) → canvas_prims, objects` (when the document, view, symbols or values changed; an edit runs `scene::update`, which re-stamps only the objects whose rows changed; pan/zoom never rebuilds; symbol texts show evaluated params), then `checks::decor(scene, overlay, highlight, staged diff) → decor prims` (only when one of them changed), selection marks and the gesture overlay
  12. `paint`: `render::draw_ui(main)` (prose, math, scroll clips) → `render::canvas::draw_cached` (grid, scene, decor and marks as world-anchored tiles of one raster layer: copied on a pan, stretched mid-zoom, patched where `CanvasDiff` finds changed rows, recorded within a per-frame budget otherwise, rows read through `PrimIndex`; D39, `docs/dev/perf.md`) + the gesture overlay live → `render::waves::draw_pane` per filled plot slot → `render::draw_ui(overlay)` → the waves dock (`render::waves::draw_window`, shifted below the editor); on the GPU a frame that only moved its layers is composed without vello: the layer quads over the last frame, then what was painted over them (canvas floats, popups) from its own texture (`render::Layers`). `paint` writes only paint caches and returns the canvas outcome (`canvas::Drawn`); the window hands it back through `Editor::painted`, the one way paint informs the model

  Wake-ups (D39): the window sleeps until input; `Editor::wake_ms` asks for a tick only while a run is in flight (25 ms), a plugin child process runs (20 ms), a plugin timer is pending (its due time) or plugin events wait (next tick), and while the last painted canvas (`Editor::painted`) was partly recorded (next tick) or stretched mid-zoom (when the zoom settles). `render::App::wake_ms` turns it into `ControlFlow::WaitUntil`; with nothing pending it is `Wait` (idle CPU 0%).
- **`load`**: `doc::Project::new(manifest, lockfile)` (parses both) → `Project::index_library` per library (the interface only: ranks 0–2, so instances draw from the index) → `Project::open` (opened cells, with their journal) → `elab::recompute(all dirty)`
- **`netlist`**: `elab::elaborate(project, top, request{config, corner, overrides, env, dialect}) → defs, def_insts, leaf_impls, globals, bench lines, violations` (open `needs_open` cells and repeat if non-empty) → **gate** (`violations.blocking()`; waived rows are `I005`, D13) → `netlist::emit(&elab, dialect) → netlist_text, name_map`
- **`run`** (`crates/run`, spec `docs/spec/run.md`; shared CLI/editor steps in `app::spines`): `Plan::read(bench)` → `expand(plan, select) → jobs` → `design(project, top, jobs) → digest` (gate: errors or unopened cells block) → per job: `prepare` (elaborate with the job's assignments → `netlist::emit`) → `Runner::submit` (native subprocess; web: runner plugin) → `Runner::wait` (CLI) or `Runner::poll` from the frame spine (editor) → `Results::record(raw)` → outputs/specs → `Run::save` (`results` + `bench` sidecar in `.schemify/runs/<design>/`); optimizer = `OptRun::step` ask → job → tell loop, best point → staged edit; MC yield via `yield_summary`
- **`import`** (in `examples/plugins/interop`, outside the core): `interop::xschem::read → records` → `interop::translate_library(files, Options{roots, pdk_vars, models, drc, …}) → (path, bytes) outputs + Report (needs_review, rounded coords)` → `interop::check_output` (every output loads in `doc`)
- **`interop`** (`examples/plugins/interop`, spec `docs/spec/interop.md`): one `job::Job` per conversion, driven by the editor guest (inverted I/O) and the `schemify-interop` CLI (file system). Export: `design::load(files, tops) → Design` → `map::resolve → Plan` → `nets::extract` + `remap::apply` (re-mastered instances) → `virtuoso::write → .sfyx` (+ `ade::export`) or `xschem_out::export → .sch/.sym`. Import: `sfyx::read` → `virtuoso::read → Schemify documents` (+ `ade::import`) → `merge::merge(local, stash)` → fingerprints (§7.5) → files. The Virtuoso side is `schemify.il` (`skill/*.il`): a data mover (`SfyExport`, `SfyExportLib`, `SfyImport`, `SfySelfTest`) whose input is never evaluated.
- **`merge-driver`**: `format::parse(O, A, B)` → `format::merge3` (by record id, by field line) → `format::write` (no semantics needed; D34)

## Tables (initial)

| Table | Owner | Written by | Read by | Capacity | Arena |
|---|---|---|---|---|---|
| cells | doc | load, apply_edits | all | 2^16 per project | project |
| instances, wires, labels, ports, texts, shapes, graphics | doc | load, apply_edits | elab, render, plugin, format | 2^22 objects per cell | document |
| param_schema, inst_params | doc | load, apply_edits | elab | 2^8 params per cell | document |
| impls | doc | load, apply_edits | elab | 2^4 per cell | document |
| doc_blocks (prose, var, plot, table) | doc | load, apply_edits | elab, ui, render | 2^12 per cell | document |
| bench_vars ≡ var blocks, bench_corners, bench_configs, bench_outputs, bench_specs, bench_code | doc | load, apply_edits | elab, run | 2^12 each | document |
| constraints, waivers | doc | load, apply_edits | elab, plugin, render | 2^16 per cell | document |
| staged_edits | doc | apply_edits (stage) | render (visual diff), plugin | 2^16 | document |
| journal | doc | apply_edits | doc (undo/recovery) | append-only, compacted on save | sidecar file |
| expr_graph, values | elab | recompute | netlist, run, ui, render | = expressions in project | elab |
| nets, pin_nets | elab | recompute | netlist, checks, render, plugin | 2^22 | elab |
| defs, def_insts, leaf_impls (+ globals, bench lines) | elab | elaborate | netlist, run, annotations, plugin | 2^24 definition rows (not flattened) | elaboration |
| name_map | netlist | emit | run, annotations, plugin | 2^24 paths | job |
| violations | elab | check | gate, render, plugin | 2^20 | elab |
| annotations | elab | apply_annotations (editor: sources `checks`, `op`) | render (decor), checks panel | 2^20 | elab |
| jobs, results | run | expand_jobs, poll | ui, render, optimizer, plugin | 2^20 jobs | project |
| raw_plots | wave | read | run, render | bounded by file | job |
| traces, panes, x view, cursors (viewer) | wave | `Waves::apply`, `Waves::reload` | ui (rows, input), render (panes) | 64 traces, 16 panes, 256 datasets | session |
| ui_nodes, ui_styles, keymap | ui | apply_ui_edits | ui::layout, ui::route_input, plugin | 2^16 nodes | session |
| scene, ui_layout | render, ui | per frame | — | per frame | frame |
| commands | app | registration (core enum + plugin rows) | dispatch, JSON-RPC, keymap | 2^12 | session |
| tabs (level stacks: doc, view, camera, selection) | app | dispatch (open, descend, return) | frame spine | 64 tabs × 40 levels | session |
| selection (+ partial wire ends, `Level::ends`) | app | gestures, dispatch | edits, marks, plugin `Selection` snapshot | 2^22 keys | per hierarchy level |
| tool_state | app | gestures, dispatch | overlay, status line | 1 row | session |
| recent files | app | opens, Save As (`recent`) | File menu, Open Most Recent | 10 paths | user state dir |
| highlight (net keys), decor prims | app | `k`/`Shift+K`; `checks::decor` | render canvas | 2^16 keys; per scene | session / scene |
| run (`sim::Sim`: one run, plot slots, plugin run queue) | app (tables owned by `run`) | Simulate, Optimize, plugin `run` requests, `Sim::poll` | results panel, docs plots, `op` overlay, plugins (`Results` snapshot) | 1 run; queue 256; 64 plots | until the next run |
| canvas_prims, objects, conn (scene) | app | `scene::build` on document change | render canvas, hit test, box select | 2^22 per kind | document epoch |
| plugins, grants, request_queues, health | plugin | load, deliver | app, ui (health panel) | 2^8 plugins; bounded queues | session |

Handles are `u32` index + `u32` generation; files store ids, never handles (D9). No pointer is stored in a field.

## Invariants

- Only `doc` writes design data. Everything else (UI, plugins, optimizer, importer, AI assistant) sends `edit_requests`.
- Only `ui::apply_ui_edits` writes `ui_nodes`; the default UI plugin uses the same path as third-party plugins.
- Files contain no derived data, no executable content, and no handles (D7, D9).
- Every object id is unique within its file and never reused.
- Every impl's pin set equals its symbol's pin set (checked on save, D11).
- Netlist emitters read `elab` tables only, never `doc`, so every dialect sees identical resolved values.
- Expression evaluation is pure and total: no IO, bounded steps, deterministic.
- Same inputs → byte-identical saves and netlists on all four platforms (D32, D37).
- Plugins never hold a handle across deliveries without a generation check; a stale handle is a declared error, never a crash.
- Only `render` talks to the GPU; only `cli` (`platform`, `pack`) and `app` talk to the OS; only `config` touches the Lua state.

## Unifying abstractions

The complete list. Each is zero-overhead and replaces two or more interfaces:

| Abstraction | Replaces | Mechanism |
|---|---|---|
| Line-record codec (`format`) | document, config, manifest, lockfile and journal parsers; merge driver; diff | one parser/writer |
| Request tables | plugin API, UI actions, JSON-RPC, optimizer write-back, importer output, waveform viewer actions | plain data queues consumed by the owner |
| `Cell { symbol?, impls[] }` | three file kinds | one table set; kind = which rows exist |
| Expression language | xschem `@subst`/`expr()`/`tcleval`, param callbacks, docs math, waveform calculator, specs | one compiler, two eval modes |
| Annotation overlay | ERC markers, OP back-annotation, LVS errors, diff markers, plugin markers | one table keyed by path/id |
| Platform split | native fs/OPFS, subprocess/no-subprocess | `cfg`, compile-time only |
| Command table | menu items, keymap targets, CLI, JSON-RPC, plugin commands | core commands = closed enum; plugin commands = rows |
| `render::Painter` | vello GPU `Scene` and `vello_cpu` raster (goldens, no-GPU fallback) | trait used only as a compile-time generic bound; monomorphized |

## Change drill

| Feature | Edits | Change radius |
|---|---|---|
| **Layout handoff** (D38): export connectivity + constraints to KLayout/Magic; LVS errors cross-probed back onto the schematic | New plugin: reads `nets`, `name_map`, `constraints` snapshots; writes CDL via the existing `Cdl` dialect plus a constraint file (capability `write project`); parses the LVS report → `annotation_requests` keyed by hierarchical path. | **0 core bodies**, as long as `annotations` + `annotation_requests` and the `name_map` exist from M1 |
| **AI sizing assistant (plugin)** (D38) | New plugin: reads `doc_blocks` (vars), `bench_specs`, `results`; sends `run_requests` (optimizer jobs) and staged `edit_requests`; the user reviews in the visual diff, then accept/reject commands commit or drop the stage. LLM access through capability `network`. | **0 core bodies**, as long as staged edits + accept/reject exist from M2 |

Both drills pass only because three tables exist on day one: `annotations` (+ requests), `name_map`, `staged_edits`. These are in the M1/M2 scope for that reason.

## Adding a feature

- New netlist dialect in core → one `Dialect` variant (the compiler lists every match), one emitter module in `netlist`.
- New dialect or simulator as a plugin → new `lang` tag; the plugin claims it and receives `run_requests`.
- New check → a transform in `elab::check` + one spine line; plugin checks → `annotation_requests` with severity.
- New analysis engine in core → a module in `run` behind a feature + one line in the `run` spine.
- New widget kind → one `ui` enum variant + its layout and render arms.
- Anything outside a crate that needs to change its table → a request table consumed by the owner.

## Software division (workstreams)

| Workstream | Crates | Milestone |
|---|---|---|
| WS1 Data | `format`, `doc` (load/save, transactions, journal, merge) | M1 |
| WS2 Semantics | `expr`, `elab` (graph, connectivity, elaboration, checks) | M1 |
| WS3 Interop and output | `netlist`, `examples/plugins/interop` (xschem import and export, Cadence Virtuoso bridge (D41); native CLI `schemify-interop`) | M1 |
| WS7 Test infra | golden corpus from running xschem, differential harness vs xschem/ngspice, fuzz targets, CI ×4 platforms (D37) | M1, ongoing |
| WS4 Simulation | `run`, `wave`, analysis engines | M2 slice, then M3 |
| WS5 Frontend | `ui`, `render`, `ui/default-plugin` | M2 |
| WS6 Plugin platform | `abi`, `plugin`, capability model, SDK | M2 |

M1 critical path: `format` → `doc` → `expr` → `elab` → `netlist` + the xschem importer (`examples/plugins/interop`), with WS7 building the xschem golden corpus in parallel from day one.
