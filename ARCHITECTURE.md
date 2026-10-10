# Architecture (draft, pre-code)

Schemify turns `lib:cell` files (`.shprim`, `.shcomp`, `.shbench`) into netlists, simulation jobs, results and an interactive picture of all three. Everything is tables owned by one crate each, transforms over whole tables, and one spine per pipeline. Decisions referenced as `D#` are in [docs/DECISIONS.md](docs/DECISIONS.md).

Rule for every abstraction: allowed only at zero or negative overhead **and** when it merges several interfaces into one. The ones that pass are listed in [Unifying abstractions](#unifying-abstractions); anything not on that list is not an abstraction we have.

## Crates

Crate = owner. A crate boundary exists only where it enforces single ownership (other crates get `&T`, never `&mut T`), gates a heavy dependency, or splits by platform.

| Crate | Owns (writes) | Reads | Heavy deps | Notes |
|---|---|---|---|---|
| `format` | — (codec) | bytes | none | Line-record codec (D9): parse, canonical write, `merge3`, record diff. Used for documents, config, manifest, lockfile, journal. |
| `expr` | — (pure) | source text | `rustfft`, `libm` | The one expression language (D12): lex → parse → check → compile → eval. Scalar mode (params, docs, templates) and column/complex mode (waveform calculator). Seeded from UW `wave/expr.rs`. Transcendentals via pure-Rust `libm` for bit-identical results on all targets (D32); FFT twiddles still use platform math (calculator output only, never netlists). |
| `doc` | `cells`, `symbols`, `pins`, `graphics`, `param_schema`, `impls`, `instances`, `inst_params`, `wires`, `labels`, `ports`, `texts`, `doc_blocks`, `bench_*`, `constraints`, `waivers`, `staged_edits`, `journal` | `edit_requests` | `format` | The only writer of design data. Load/save, edit transactions (commit or stage), undo/redo, lib resolution via manifest + lockfile (D10, D35). |
| `elab` | `expr_graph`, `values`, `nets`, `pin_nets`, `inst_paths`, `resolved_impls`, `resolved_params`, `name_map`, `violations`, `annotations` | doc tables, `annotation_requests` | `expr` | Everything derived: dirty-graph recompute (D19), connectivity from geometry, hierarchy elaboration with bench-config impl selection (D11a), checks/gating (D13), one annotation overlay. |
| `netlist` | `netlist_text` (job arena) | elab tables only | — | Emitters: `Dialect = Ngspice \| Xyce \| Cdl \| Verilog \| Spectre`, closed enum, exhaustive match (D22). Plugin dialects go through `run_requests`. |
| `wave` | `raw_plots` (column SoA) | `.raw` bytes | `rustfft` | UW salvage as-is (D26); calculator delegates to `expr` column mode. |
| `run` | `jobs`, `results` | bench tables, elab, netlist | features: `sweep`, `corners`, `mc`, `optimize` | Job expansion (sweep × corner × MC × named config), local worker queue, native subprocess runner (D15), result tagging by design hash, outputs/specs eval, optimizer ask/tell (UW salvage). |
| `import` | — (emits doc tables via `format`) | xschem `.sch/.sym`, model cards, tech files, `cdfDump` | `format`, `expr` | Clean-room xschem reader + attribute/expression translation + PDK constraint readers (D14). Library for both `schemify import` and `tools/devicereader`. |
| `abi` | — (types) | — | none | Versioned plugin ABI: table snapshot layouts, request-table layouts, capability ids. Shared by host and guests; doubles as the Rust plugin SDK. |
| `plugin` | `plugins`, `grants`, `request_queues`, `health` | snapshots of all tables | `wasmi` | Host: load, capability enforcement (D7), snapshot export, request intake, last-wins ordering + health report (D8). |
| `ui` | `ui_nodes`, `ui_styles`, `keymap`, `ui_layout` (frame) | `ui_edit_requests`, input events | `taffy`, `parley`, `accesskit`, `ratex-*` | UI-as-data (D4, D6). Widget kinds (incl. `math`, `graph`) are a closed enum. Input routing → `command_requests`. |
| `render` | `scene` (frame), `camera` | doc, elab, ui_layout, results | `vello`, `wgpu` | The only crate touching the GPU. Draws canvas, UI, graphs, visual diff and annotations (D5). |
| `app` | `commands` (registry), `session` | everything | `winit` | The spines. Platform split by `cfg`: native fs / OPFS (D31), subprocess runner / none (D16). CLI + JSON-RPC over the command table (D27). Binary `schemify`. |

Outside `crates/`: `tools/devicereader` (thin bin over `import`), `tools/fmtbench` (format benchmark), `plugins/default-ui` (D6), `plugins/gmid-lut`, `plugins/net2schem` (D26).

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

Headless build (M1): `format expr doc elab netlist import run app` with `app` feature `gui` off. GUI adds `abi plugin ui render`.

## Spines

Each spine lives in `app` as one function, one line per transform. Branches that choose work (command dispatch, gating) sit in the spine, never inside a transform.

- **`frame`** (event-driven; nothing runs when idle, D39):
  `ui::route_input(ui_layout, keymap, events) → command_requests` →
  `app::dispatch(commands, command_requests) → edit_requests, ui_edit_requests, run_requests, plugin_events` →
  `plugin::deliver(plugins, plugin_events, snapshots) → request_queues` →
  `doc::apply_edits(edit_requests ∪ plugin edits) → doc tables, journal` →
  `ui::apply_ui_edits(ui_edit_requests ∪ plugin ui edits) → ui_nodes` (ordered, last wins) →
  `elab::recompute(doc, dirty) → values, nets, violations` →
  `elab::apply_annotations(annotation_requests) → annotations` →
  `run::poll(jobs) → results` →
  `ui::layout(ui_nodes, ui_styles) → ui_layout` →
  `render::build_scene(doc, elab, ui_layout, results) → scene` → `render::submit(scene)`
- **`load`**: `format::parse(manifest, lockfile)` → `doc::resolve_libs` → `doc::load_headers` (symbol interface only, 5 µs each) → `doc::load_bodies` (opened cells) → `elab::recompute(all dirty)`
- **`netlist`**: `elab::elaborate(doc, bench, config) → inst_paths, resolved_impls` → `elab::resolve_params → resolved_params` → `elab::check → violations` → **gate** (violations without waiver block, D13) → `netlist::emit(dialect) → netlist_text, name_map`
- **`run`**: `run::expand_jobs(bench, corners, mc, sweeps, configs) → jobs` → per job: `netlist` spine with overrides → `run::dispatch(jobs) → subprocess | plugin runner` → `wave::read(raw) → raw_plots` → `run::eval_outputs(expr, raw_plots) → results` → `run::eval_specs(results)`; optimizer = `ask → jobs → tell` loop around this spine
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
| inst_paths, resolved_impls, resolved_params, name_map | elab | elaborate | netlist, run, annotations, plugin | 2^24 elaborated instances | elab |
| violations | elab | check | gate, render, plugin | 2^20 | elab |
| annotations | elab | apply_annotations | render | 2^20 | elab |
| jobs, results | run | expand_jobs, poll | ui, render, optimizer, plugin | 2^20 jobs | project |
| raw_plots | wave | read | run, render | bounded by file | job |
| ui_nodes, ui_styles, keymap | ui | apply_ui_edits | ui::layout, ui::route_input, plugin | 2^16 nodes | session |
| scene, ui_layout | render, ui | per frame | — | per frame | frame |
| commands | app | registration (core enum + plugin rows) | dispatch, JSON-RPC, keymap | 2^12 | session |
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
- Only `render` talks to the GPU; only `app` talks to the OS.

## Unifying abstractions

The complete list. Each is zero-overhead and replaces two or more interfaces:

| Abstraction | Replaces | Mechanism |
|---|---|---|
| Line-record codec (`format`) | document, config, manifest, lockfile and journal parsers; merge driver; diff | one parser/writer |
| Request tables | plugin API, UI actions, JSON-RPC, optimizer write-back, importer output | plain data queues consumed by the owner |
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
