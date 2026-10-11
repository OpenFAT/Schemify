# Architecture (draft, pre-code)

Schemify turns `lib:cell` files (`.shprim`, `.shcomp`, `.shbench`) into netlists, simulation jobs, results and an interactive picture of all three. Everything is tables owned by one crate each, transforms over whole tables, and one spine per pipeline. Decisions referenced as `D#` are in [docs/DECISIONS.md](docs/DECISIONS.md).

Rule for every abstraction: allowed only at zero or negative overhead **and** when it merges several interfaces into one. The ones that pass are listed in [Unifying abstractions](#unifying-abstractions); anything not on that list is not an abstraction we have.

## Crates

Crate = owner. A crate boundary exists only where it enforces single ownership (other crates get `&T`, never `&mut T`), gates a heavy dependency, or splits by platform.

| Crate | Owns (writes) | Reads | Heavy deps | Notes |
|---|---|---|---|---|
| `format` | — (codec) | bytes | none | Line-record codec (D9): parse, canonical write, `merge3`, record diff. Used for documents, config, manifest, lockfile, journal. |
| `expr` | — (pure) | source text | `num-complex`, `libm` | The one expression language (D12): lex → parse → check → compile → eval. Scalar mode (params, docs, templates) and column/complex mode (waveform calculator, with its own radix-2 + Bluestein FFT). Seeded from UW `wave/expr.rs`. Transcendentals and FFT twiddles via pure-Rust `libm` for bit-identical results on all targets (D32). |
| `doc` | `cells`, `symbols`, `pins`, `graphics`, `param_schema`, `impls`, `instances`, `inst_params`, `wires`, `labels`, `ports`, `texts`, `doc_blocks`, `bench_*`, `constraints`, `waivers`, `staged_edits`, `journal` | `edit_requests` | `format` | The only writer of design data. Load/save, edit transactions (commit or stage), undo/redo, lib resolution via manifest + lockfile (D10, D35). |
| `elab` | `expr_graph`, `values` (`Graph`), `nets`, `pin_nets` (`CellNets`), `defs`, `def_insts`, `leaf_impls` (`Elab`: hierarchical, one row per specialization), `violations`, `annotations` | doc tables, `annotation_requests` | `expr` (`format` for doc's column types) | Everything derived: dirty-graph recompute (D19), connectivity from geometry, hierarchy elaboration with bench-config impl selection (D11a) and subckt specialization, checks/gating (D13), one annotation overlay. Paths are not materialized: a path is the walk of `defs`/`def_insts` from the top. |
| `netlist` | `netlist_text` (job arena), `name_map` | elab tables only | — | Emitters: `Dialect = Ngspice \| Xyce \| Cdl \| Verilog \| Spectre`, closed enum, exhaustive match (D22). Plugin dialects go through `run_requests`. |
| `wave` | `raw_plots` (column SoA), viewer tables (`traces`, `panes`, x view, cursors) | `.raw` bytes, `wave_requests` | none (`expr`) | Reader for ngspice/Xyce/LTspice binary+ASCII (UW salvage, D26, hardened). Calculator = `expr` calc mode over borrowed raw columns. SIMD min/max display decimation. `Waves::apply(Request)` is the viewer tables' only writer (D23). |
| `run` | `jobs`, `results` | bench tables, elab, netlist | features: `sweep`, `corners`, `mc`, `optimize` | Job expansion (sweep × corner × MC × named config), local worker queue, native subprocess runner (D15), result tagging by design hash, outputs/specs eval, optimizer ask/tell (UW salvage). |
| `import` | — (emits doc tables via `format`) | xschem `.sch/.sym`, model cards, tech files, `cdfDump` | `format`, `expr` | Clean-room xschem reader + attribute/expression translation + PDK constraint readers (D14). Library for both `schemify import` and `tools/devicereader`. |
| `abi` | — (types) | — | none | Versioned plugin ABI: table snapshot layouts, request-table layouts, capability ids. Shared by host and guests; doubles as the Rust plugin SDK. |
| `plugin` | `plugins`, `grants`, `request_queues`, `health` | snapshots of all tables | `wasmi` | Host: load, capability enforcement (D7), snapshot export, request intake, last-wins ordering + health report (D8). |
| `ui` | `ui_nodes`, `ui_styles`, `keymap`, `ui_layout` (frame) | `ui_edit_requests`, input events | `taffy`, `parley`, `accesskit`, `ratex-*` | UI-as-data (D4, D6). Widget kinds (incl. `math`, `graph`, `prose` = markdown + math wrapped to its width, multi-line `text_area`, scrolling `scroll`) are a closed enum. Input routing → `command_requests`. |
| `render` | `scene` (frame), `camera` | doc, elab, ui_layout, results | `vello`, `wgpu` | The only crate touching the GPU. Draws canvas, UI, graphs, visual diff and annotations (D5). |
| `cli` | — | everything headless | — | The headless core, binary `schemify` (D39 ≤ 2 MB): the CLI spines, `libs` (load spine), `platform` (the OS boundary), `spines` (netlist/run steps shared with the editor). `schemify edit` runs `schemify-gui`. Links no GPU/window/text/accessibility/LaTeX crate (`tests/deps.rs`). |
| `app` | `commands` (registry), `session` (project, open documents, tabs + their docs toggle, hierarchy stacks), `selection`, `tool_state`, `canvas_prims` + `objects` (the active document drawn, derived), `symbols` (cache), clipboard, dialog state, docs page state (`docs::Page`), net highlight + decor prims (`checks`), the run in flight (`sim::Sim`: run queue, plot slots; jobs/results stay `run`'s), the editor's `elab::Annotations` instance | everything | (`winit` through `render::run`) | The editor, binary `schemify-gui` (D39 ≤ 10 MB): the `frame` spine over `cli`'s spines, `libs` and `platform` (re-exported). JSON-RPC over the command table later (D27). |

Outside `crates/`: `tools/devicereader` (thin bin over `import`), `tools/fmtbench` (format benchmark), `plugins/default-ui` (D6), `plugins/gmid-lut` (D20/D26: `gmid.*` expression functions over gm/Id lookup tables, characterized through `spawn:ngspice`; loaded by a project manifest, not embedded), `plugins/cktimg` (D26 as amended 2026-10-11: SPICE → Schemify import; a thin safe wrapper over cktImg's C ABI (`ffi` module, the crate's only `unsafe`) plus the glue that maps devices onto `core` cells, tells cktImg their pin geometry and writes its placement as records; its `build.rs` compiles `extern/cktimg` with Zig 0.17), `extern/cktimg` (git submodule, MIT: cktImg, the user's SPICE → placed-schematic library, Zig with a C ABI; never edited here — changes go upstream as `docs/upstream/cktimg-*.patch`).

Dependency direction (no cycles; arrows mean "depends on"):

```
format   expr   abi                      (leaves, zero internal deps)
  ↑   ↖  ↑  ↑     ↑
 doc ── elab ──── wave
  ↑      ↑   ↖     ↑
import  netlist   run
            ↖      ↑
   plugin   ui   render
       ↖     ↑    ↗
           app
```

`ui` and `render` also depend on `wave` (the waves window and its panes); `wave` depends on `expr`. `cli` depends on `import netlist run` (with `doc elab expr format`); `app` depends on `cli`.

Two binaries (D39 as amended): the core `schemify` = `cli` over `format expr doc elab netlist import run`; the editor `schemify-gui` = `app`, which adds `abi plugin ui render` on top of `cli`.

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
  6. `app::load_symbols(active doc)` — opens every instantiated cell (with its journal) so its symbol graphics can be drawn
  7. `elab::CellState::recompute(project, active doc, committed ops, plugin functions) → values, nets, violations` — every shown document (D13 live checks, D19 values); incremental per commit, a full `build` after undo/redo, other documents' edits or a plugin change. Plugin functions are called through `plugin::Plugins::call` (D20)
  8. `docs::cache_ops(values) → doc::apply_edits` — the D20 `cached` write-back, folded into the triggering docs-page edit's undo entry (`docs/spec/docs-page.md` §6); then `elab::Annotations::apply(checks request)` — the `checks` source of the overlay
  9. `sim::Sim::{serve_queue, poll}` — plugin run requests become jobs; ≤ 8 jobs prepared per frame; `run::Runner::poll` (never waits); optimizer `OptRun::step` (ask → job → tell); `RunDone` queued for requesting plugins; on idle the results file is saved and the optimizer's best point is staged (`doc` stage); stale check (design hash) after edits; plot slots and the `op` overlay source from the selected job's raw file
  10. `ui::compose(core base, plugin ui+keymap sets, app state) → ui_nodes, ui_styles, keymap` (last wins, D8) → `ui::layout → ui_layout`. The app projects its own state (tabs, status line, dialog rows, **the docs page** of the active level under `docs.page` with filled plot slots as `graph` nodes and the staged-edit banner, the checks panel or results table under `bottom`, a netlist text tab replacing `canvas`) as the *last* contribution, made of `set`s and child nodes under plugin-declared containers; the core base (window + canvas | docs pane, `bottom`) applies only when no plugin declares a `window`
  11. `scene::build(doc, symbols, values) → canvas_prims, objects` (only when the document, view, edit epoch, symbols or values changed; pan/zoom never rebuilds; symbol texts show evaluated params), then `checks::decor(scene, overlay, highlight, staged diff) → decor prims` (only when one of them changed), selection marks and the gesture overlay
  12. `paint`: `render::draw_ui(main)` (prose, math, scroll clips) → `render::canvas::{draw_grid, draw_prims (scene, decor, overlay), draw_marks}` → `render::waves::draw_pane` per filled plot slot → `render::draw_ui(overlay)` → the waves dock (`render::waves::draw_window`, shifted below the editor)

  Wake-ups (D39): the window sleeps until input; `Editor::wake_ms` asks for a tick only while a run is in flight (25 ms), a plugin child process runs (20 ms), a plugin timer is pending (its due time) or plugin events wait (next tick). `render::App::wake_ms` turns it into `ControlFlow::WaitUntil`; with nothing pending it is `Wait` (idle CPU 0%).
- **`load`**: `format::parse(manifest, lockfile)` → `doc::resolve_libs` → `doc::load_headers` (symbol interface only, 5 µs each) → `doc::load_bodies` (opened cells) → `elab::recompute(all dirty)`
- **`netlist`**: `elab::elaborate(project, top, request{config, corner, overrides, env, dialect}) → defs, def_insts, leaf_impls, globals, bench lines, violations` (open `needs_open` cells and repeat if non-empty) → **gate** (`violations.blocking()`; waived rows are `I005`, D13) → `netlist::emit(&elab, dialect) → netlist_text, name_map`
- **`run`** (`crates/run`, spec `docs/spec/run.md`; shared CLI/editor steps in `app::spines`): `Plan::read(bench)` → `expand(plan, select) → jobs` → `design(project, top, jobs) → digest` (gate: errors or unopened cells block) → per job: `prepare` (elaborate with the job's assignments → `netlist::emit`) → `Runner::submit` (native subprocess; web: runner plugin) → `Runner::wait` (CLI) or `Runner::poll` from the frame spine (editor) → `Results::record(raw)` → outputs/specs → `Run::save` (`results` + `bench` sidecar in `.schemify/runs/<design>/`); optimizer = `OptRun::step` ask → job → tell loop, best point → staged edit; MC yield via `yield_summary`
- **`import`**: `import::read_xschem → records` → `import::translate(attrs, pdk_vars) → doc tables + needs_review + rounded_coords` → `format::write` → report
- **`merge-driver`**: `format::parse(O, A, B)` → `format::merge3` (by record id, by field line) → `format::write` (no semantics needed; D34)

## Tables (initial)

| Table | Owner | Written by | Read by | Capacity | Arena |
|---|---|---|---|---|---|
| cells | doc | load, apply_edits | all | 2^16 per project | project |
| instances, wires, labels, ports, texts, graphics | doc | load, apply_edits | elab, render, plugin, format | 2^22 objects per cell | document |
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
| traces, panes, x view, cursors (viewer) | wave | `Waves::apply` | ui (rows, input), render (panes) | 64 traces, 16 panes, 256 datasets | session |
| ui_nodes, ui_styles, keymap | ui | apply_ui_edits | ui::layout, ui::route_input, plugin | 2^16 nodes | session |
| scene, ui_layout | render, ui | per frame | — | per frame | frame |
| commands | app | registration (core enum + plugin rows) | dispatch, JSON-RPC, keymap | 2^12 | session |
| tabs (level stacks: doc, view, camera, selection) | app | dispatch (open, descend, return) | frame spine | 64 tabs × 40 levels | session |
| selection | app | gestures, dispatch | edits, marks, plugin `Selection` snapshot | 2^22 keys | per hierarchy level |
| tool_state | app | gestures, dispatch | overlay, status line | 1 row | session |
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
- Only `render` talks to the GPU; only `cli` (`platform`) and `app` talk to the OS.

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
| WS3 Interop and output | `import`, `netlist`, `tools/devicereader` | M1 |
| WS7 Test infra | golden corpus from running xschem, differential harness vs xschem/ngspice, fuzz targets, CI ×4 platforms (D37) | M1, ongoing |
| WS4 Simulation | `run`, `wave`, analysis engines | M2 slice, then M3 |
| WS5 Frontend | `ui`, `render`, `plugins/default-ui` | M2 |
| WS6 Plugin platform | `abi`, `plugin`, capability model, SDK | M2 |

M1 critical path: `format` → `doc` → `expr` → `elab` → `netlist` + `import`, with WS7 building the xschem golden corpus in parallel from day one.
