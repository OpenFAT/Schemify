# Backlog

Open items collected from workstream reports. Each item names its owner crate. Remove an item in the change that resolves it.

## Cross-crate API requests

- **elab**: adopt the doc and expr APIs that replaced its workarounds: `Project::resolve_in(row, ref)` and `Project::open_handle(row)` (for `cells::resolve_in_row` / `is_open`), `apply_edits_report` (ops of commits, undos and redos, instead of the caller passing ops back), `expr::topo_order_all` (every cycle in one pass, instead of `Graph::finish` cutting one cycle per round).
- **ui**: switch the local line-record lexer to `schemify-format`. (**plugin** host done; `abi::text` stays the guests' encoder/decoder until `format` has a public value lexer, pinned to the format by `abi_and_format_lexers_agree`.)
- **format**: a `\u{+41}` escape is accepted (`u32::from_str_radix` takes a sign; the grammar says `1*6HEXDIG`). `abi::text` rejects it.

## app / editor

- **app**: `load_symbols` opens every instantiated cell to get its symbol; the library index now keeps rank 2 (`Project::interface(row)` has the symbol graphics, `Project::open_handle(row)` says whether the cell is open), so the editor can draw instances from the index. `scene::diff` (a positional scan per edit) can take the changed records from `apply_edits_report` instead.
- **ui**: text fields commit only on Enter (the app copies typed text every frame).
- Editor gaps vs xschem (after the parity pass, `tests/parity.rs`): partial wire selection (xschem's Ctrl+drag box selects only the inside ends of crossing wires; here a stretch follows wire ends on moved connection points); vertex editing of a selected wire/line/polygon/rect corner; symbol `stext` orientation (only schematic `text` records turn); the context menu closes with Esc or a row, not a click outside; recent files outside the current project do not open (one project per session); layer colors, dim and line width are not persisted (set them in `init.lua`: `schemify.opt` has no keys for them yet); the web build has no system clipboard (D2 glue) and no exports to disk (OPFS); plugin I/O capabilities fetch/clipboard in the app.
- **Exports**: PDF/EPS embed whole UI faces (Noto Sans ~190 KB, KaTeX Main ~65 KB when used, ASCII85; twice that as EPS hex) instead of subsets of the glyphs used; their text is placed glyph by glyph (no kerning or ligatures); characters neither face has print `.notdef` (the canvas falls back to system fonts). The export page, PNG scale and package status are commands without default-UI menu rows yet (`options.export_page_size`, `options.png_export_scale`, `file.image_export.png_export_full`, `tools.package_status`).
- **Make subcircuit** (Ctrl+Shift+H) connects the new instance by net labels at the old boundary points and on its pins (xschem rewires); the new cell's symbol is the generated box.
- **Join/trim** does not merge diagonal wires or wires with attributes other than `bus`.

## user config and packages (config spec)

- **config**: no Lua on the web: C Lua 5.4 (mlua vendored) has no `wasm32-unknown-unknown` build, so the web runs without `init.lua`. Upgrade: a pure-Rust Lua 5.4 (piccolo, once it covers `error(msg, level)`, `tonumber`, `load` semantics) or C Lua on a wasi-libc shim; OPFS `/config` then holds `init.lua`.
- **config**: a single C library call (a pattern match over a huge string) runs between budget checks; only the memory cap bounds it.
- **cli/pack**: `url` packages are one `.wasm` file (no archives); `pack` has no parallel fetches.

## docs page (D19–D21)

- A structural edit (instance insert/delete) still rebuilds the cell state (136 ms at 100k objects when the cell has vars): left to the in-flight incremental-elab work (**elab**: per-net connectivity and checks, then instance blocks in the graph), which owns `CellState::recompute`.
- Plot overlays: at most 16 jobs per slot, read from disk on each refresh (no per-job raw cache); cells below the bench are found among open documents (results reloaded at open reach the bench's direct children only, by stem).
- D20 caches of parked (shown before, not active) documents are written when they are activated again.
- `gmid-lut` characterization uses `spawn:ngspice`, not `run` requests: the ABI cannot (**abi**/**plugin**) run a bench the plugin just wrote (cells are indexed at project load) or hand back a job's raw file/directory (`RunDone` and `Results` carry scalar outputs; the LUT needs the (L, VDS, VGS) grid). Needs a "run this written bench" request and the job directory in `RunDone`.
- Integration points still stubbed: symbols via elab, web OPFS.

## run in the editor (M2 integration)

- One run at a time (xschem-like); a plugin request for another bench waits until the current run is idle. Results table: ≤ 64 job rows, no sort/filter; MC yield and the optimizer's best point are text rows.
- OP back-annotation maps ngspice names only (`v(net)`, `@dev[p]`); Xyce OP naming and `.save` of device quantities (ngspice saves `@dev[gm]` only when asked) are not added automatically. Cross-probe of currents: branch devices (`v`, `e`, `h`, `l`) only.
- Staged edits: the canvas diff outlines changed objects only (inserted/deleted ones are not drawn until accepted); Accept/Reject act on the document's first stage.
- Waves dock: fixed 60/40 split (no splitter drag); keyboard focus follows the pointer.
- Netlist text tab: a prose code block capped at 2^14 lines / 256 KiB (the file has everything); no search.
- Highlight keys a component by its first bit name (a bus component highlights by its first bit); keys of nets the name map lacks fall back to the level path (no cross-hierarchy match then).
- `Results` snapshot `pass` is 0: spec verdicts are per spec, not per output.
- The OTA fixture has no bench; the GUI run tests and `docs/screenshots/simulate.png` use `crates/app/tests/fixtures/rc`.

## Decisions pending (user)

- **cktImg upstream patches** (`docs/upstream/`): `cktimg-lib-step.patch` (a `zig build lib` step: `zig build` fails on wasm32-freestanding because the tools need an OS), `cktimg-optional-zon.patch` (`-Dzon_config=false`, plugin 645 KB → 478 KB), `cktimg-performance.md` (99% of layout time is the round loop in `layout.place`; batching separation requests). Apply upstream, then bump the `extern/cktimg` submodule.


## command stream (rpc, D27)

- The editor's server has no `edit`/`netlist` methods of its own (its table is the command table; use the headless server for files the editor does not hold open). Command answers carry the status line only, not structured results (a `sim.netlist` answer does not include the text).
- `schemify rpc --listen` serves one client at a time; Windows has no Unix-domain sockets in `std` (TCP loopback only; named pipes need `windows-sys`).
- `schemify mcp` exposes the headless methods only (no editor tools, no MCP resources or prompts); a bridge to a running editor's socket would add its command table as tools.

## plugins

- Web: plugins with a fuel override run their events on the frame thread (no threads in `wasm32-unknown-unknown`), so a large cktimg import still blocks the browser tab. Slice them with wasmi resumable calls (out-of-fuel resumption) per frame.
- A plugin's `eval` builds the document's `CellState` once per delivery (100+ ms at 100k objects); pass the editor's own state for the active document if eval-heavy plugins appear.

## cktimg plugin

- **Fuel**: cktImg needs ~10^8 wasmi fuel per millisecond of native layout; 11 of 80 fixtures exceed the default 10^8 per event and abort with `E_FUEL` (up to > 4·10^10). Needs the per-plugin fuel override the plugin ABI (§2) specifies but the app does not implement yet (`plugins.load(.., FUEL_DEFAULT)` for every plugin), and/or a faster cktImg layout (`docs/upstream/cktimg-performance.md`). With a bigger budget, one cell per event (a timer between cells) keeps the editor responsive.
- **Size**: `cktimg.wasm` is ~630 KB (`plugins/build.sh` allows 700 KB for it, 200 KB for the rest); `docs/upstream/cktimg-optional-zon.patch` takes it to ~480 KB.
- **Upstream**: `docs/upstream/cktimg-lib-step.patch` (then `build.rs` can call `zig build lib`), `cktimg-optional-zon.patch`.
- Block symbols of generated subckt cells are the plugin's own (inputs left, the rest right); cktImg's `*@ left/right/top/bottom` directives are not read yet (a probe placement of one instance would give cktImg's block geometry).
- XSPICE `A/N/P/Y` and node-less `X` cards are kept as code text, not drawn.

## netlist

- Remaining corpus differences are owned and annotated in `tests/corpus/RATCHET.md` (SPICE 246/301 + 23 golden-unevaluated, CDL code-stripped 227/301 + 32, 22 gate-blocked).
- **import**: parametric bus widths: `ADD[dim-1:0]` (template parameters: needs widths resolved per instance, a variant cell per distinct value, and the cell's own labels resolved too; the only corpus bench, ram_tb, has an invalid-SPICE golden with literal `DIN[width-1:0]` ports) and `A[max:0]` (no parameter: the pin takes the attached net's width, an elab rule; test_ngspice also needs `~`). Per-instance `spice_sym_def` built by Tcl (`[abs_sym_path f]`, `[read_data_nonewline …]`) stays reviewed (inst_sch_select).
- **elab** (asked by import): XSPICE inverted-net labels `~D[5..0]` import as the bundle `~D5,…,~D0`, but the net grammar (netlist §4 `ident`) has no `~`, so the bundle is one scalar name and the pin is `E011` (sar_adc, tb_sar_adc, test_ngspice): allow an optional leading `~` in `ident` (netexpr.rs), the bit name keeping it (ngspice inverts the node).
- **elab**: xschem replicates a 1-pin instance on an N-bit bus (`.save v(count[k])` per bit) and sizes an unnamed net on an iterated instance by pin width × elements; both need a rule that keeps the sky130 adders' broadcast.
- `file` stays relative in templates (netlist §7.6.14): absolute paths would break byte-identical output across checkouts; decide on project-relative paths if a template needs one.
- Precompiled OSDI cells stay lang `veriloga` (no `file`): their body is simulation-only text the netlister keeps out of CDL, as xschem keeps it in its architecture block; lang `spice` would put `pre_osdi` loads into LVS netlists.
- Spectre has no second oracle (snapshots only): no Spectre-syntax simulator runs in CI. Verilog is compiled by Icarus in CI (`netlist/tests/iverilog.rs`).

## run

- `format`: make var `dist` and optimize `seed` known attributes (they work as preserved unknowns today).
- ngspice's random seed isn't set per MC sample, so PDK `agauss` statistics aren't reproducible yet.
- Inline Verilog-A/Verilog `body` sources aren't compiled (only `file`); OpenVAF/Icarus steps tested with stand-ins only (tools not installed); compile blocks `poll` the first time.
- Optimizer runs only at the bench's default config/corner with unit goal weights.
- Relative `.lib` paths resolve against the job directory.

## Performance

Numbers and how to reproduce them: `docs/perf.md`. D39 status: pan/zoom at 100k objects
0.13 ms CPU, 0.08 ms GPU (RTX; 0.4–1.1 ms on the Intel iGPU); flat 1M netlist 0.85–0.93 s;
param edit 1.8 ms; idle 0%. Not met: a local edit at 100k objects (~50 ms, D39 has no row
for it; the goal set for this work was ≤ 1 ms).

- **elab**: an edit at 100k objects rebuilds connectivity (20 ms) and re-runs every check
  (7 ms, 3.4 of it sorting the rows): incremental connectivity per touched component
  (a deleted wire can split only its own net) and checks over the changed nets would make
  it O(change). The 1M flat netlist has 7–15% margin: generated net names (1M interns,
  ~100 ms) and the elements' names (~90 ms) are the next costs.
- **elab**: edits that change an expression's dependencies rebuild the whole graph (17 ms at 30k nodes).
- **app**: per edit, plugin snapshots are refilled twice (one delivery before the edit is
  applied, one after: ~3.5 ms each at 100k) and decor is rebuilt (~4.5 ms): per-table
  snapshot keys and an incremental decor (changed marks and objects only).
- **render/app**: `CanvasDiff` hashes every row of a changed table (~3.4 ms for the 320k
  rows of a 100k-object scene and its decor): `scene::update` and the decor builder
  reporting their changed rows would make it O(change). A click at 1M objects costs 17 ms
  (`scene::marks` scans every primitive's owner).
- **render**: a crisp zoom frame or a pan past the margin re-records the whole layer
  (~12 ms CPU, 3–5 ms RTX / ~47 ms Intel GPU at 100k): tiles (record only the newly uncovered
  part) and a level-of-detail table for zoomed-out views.
- **format**: 100k-object parse ~52–60 ms; doc open 89 ms (budget 200 ms, OK); the 100k parse test sits near 100 ms under load.

## Correctness / semantics

- **elab**: `E002` doesn't yet compare a SPICE `body`'s `.subckt` ports with the symbol pins; `global true` names are gathered from every reachable schematic, including unselected impls; `net_<id>` may collide with a user label of the same spelling; `CellState` doesn't observe edits to other documents.
- **ui**: status text that changes every frame grows the string table; IME candidate window not positioned.
- **wave**: per-sweep-step readouts; FFT traces share the time-domain x axis.
- **import (readers)**: cdfDump untested against a real dump: producing one needs Cadence Virtuoso (`cdfDump` is a SKILL function), which no CI or this machine has; the reader follows the documented format only.

## Parity gaps vs xschem (UI)

- **ui/render**: toolbar icons (text labels now), menu keyboard navigation, tear-off menus, split dragging, dropdown popups. (Context menus are positioned dialogs, `UiState::set_dialog_at`; chooser lists scroll.)

## Parity: intentionally dropped

xschem menu items and keys with no meaning in Schemify, removed from the default UI (`plugins/default-ui`) rather than left unresolved. The parity ratchet (`crates/app/tests/parity.rs`) holds every remaining name to an implementation; `tests/shell.rs` lists the dropped inventory chords.

- File → Start new Xschem process (`x`), Alt+Shift+E / Alt+Shift+I "in a new process": one process with tabs; `x` and the Alt+Shift keys open a new tab instead.
- Options → Debug mode: logs go to the status line and stderr; there is no Tcl debug level.
- Options → No XCopyArea drawing model, Fix for GPUs with broken tiled fill, Fix broken RDP mouse coordinates, `$` / Ctrl+`$`: X11 drawing workarounds; vello draws every frame.
- Options → Enable infix-interface, Intuitive Click & Drag interface: Schemify keeps xschem's default interaction (infix and intuitive), one gesture state machine.
- Options → Undo buffer on Disk: undo always lives in the on-disk journal (D28).
- Options → Unsel. partial sel. wires after stretch move: wires are selected whole; a stretch never leaves partially selected wires.
- Options → Variable grid point size, View → Set grid point size: the canvas draws grid dots at one size.
- Options → Group bus slices in Verilog instances, Replace `[` and `]` for buses in SPICE netlist: netlist spellings are fixed by the dialects (D22, netlist spec §7.6).
- Options → Netlist format → VHDL, tEDAx, Split netlist, Symbol global attrs: no such emitters or modes; SPICE/Verilog map to the dialect list (Simulation → Netlist dialect).
- View → Horizontal toolbar: toolbar placement is a UI-plugin style (D6); View → Tabbed interface: always tabbed.
- View → Show hidden texts: the scene draws every text (no hidden-text attribute).
- Properties → Change selected object insertion order (Shift+S): rank-3 order is by id (D9); netlist order is not set by insertion.
- Tools → Execute TCL command: no Tcl, documents and the UI run no scripts (D7, D12).
- Tools → Grab screen area (Print): no image objects in the format; Print exports a PNG of the canvas instead.
- Tools → Insert JPG/PNG/SVG image: no image records in the format (D9).
- Ctrl+h launcher, Simulation → Graphs → Add waveform reload launcher: executable content in a design (D7).
- Symbol → Change selected inst. texts to floaters, Unselect attached floaters (Ctrl+u): no floater texts (instance texts are symbol `stext` templates).
- Symbol → Search all search-paths for schematic: no search path (D10, `lib:cell`).
- Symbol → Allow duplicated instance names: duplicate names break netlists; copies are renamed, and Highlight → duplicate / rename duplicate names repair imports.
- Highlight → Propagate highlight (drill): no `propag` pin attribute; highlights are hierarchy-wide already (Ctrl+Shift+K highlights).
- Simulation → Set netlist dir, Set top level netlist name, Use netlist directory / 'simulation' dir / 'simulation/[schname]' dir: netlists go to `.schemify/netlist/<cell>.<ext>` (spines).
- Simulation → Keep symbols when traversing hierarchy: the symbol cache is always kept.
- Simulation → Configure simulators and tools: simulators come from the user config (`sim` records, format spec §4; the reader is the config crate's).
- Simulation → Shell [simulation path], Utile stimuli editor / translate: terminals and xschem's awk tool chain.
- Simulation → LVS (LVS netlist, upper-case SUBCKT, top level is a subckt, lvs_ignore, spiceprefix): LVS netlists are the `cdl` dialect (D22).
- Simulation → Graphs → Hide graphs if no spice data loaded, Live annotate probes with 'b' cursor: docs plot slots show their state; cursor-time annotation is a waves-dock gap (see "Parity gaps vs xschem (UI)" waves).
- Keys `0`–`4`: xschem's built-in logic simulator (net logic values); simulation goes through runners (D15).
- **waves**: horizontal cursors, dataset tracking, measurement tooltip, digital/bus mode, histogram, graph properties dialog, per-graph x locking, typed cursor positions, colour picker.

## Testing infrastructure

- sky130 model files aren't fetched (open_pdks builds them); the ngspice second oracle for sky130 needs `volare`, then a nightly step for `ngspice_results_agree`.
- **plugins/build.sh is not byte-reproducible** (`hello`, `gmid_stub`, … differ between rebuilds and machines), so CI cannot check that the committed `.wasm` assets match their sources. Make it reproducible (pinned toolchain in the build, `--remap-path-prefix`, no host paths in custom sections), then add the diff to the `plugins` job.
- Fuzz-corpus replay at the merge tier: the nightly corpora live in the actions cache and artifacts, not the repo. Check in minimized corpora (`cargo fuzz cmin`) and replay them in `ci.yml`.
- Coverage ratchet (`cargo-llvm-cov`) is not wired (TESTING.md nightly tier).
- **render** (web): no WebGPU device in headless Chrome (CI, SwiftShader) means a blank canvas; the browser smoke test can only check that the editor starts (canvas created, no trap). A `vello_cpu` fallback when no adapter is found would let it compare pixels against the native goldens.
- `tests/perf/baseline.txt` is measured on GitHub's shared `ubuntu-latest` runners; if the 10% gate proves noisy there, gate on best-of-N medians or a self-hosted runner.
- Release builds: no macOS x86_64 binary (only Apple-silicon runners); the Linux binary links the build runner's glibc.
