# Backlog

Open items collected from workstream reports. Each item names its owner crate. Remove an item in the change that resolves it.

## Cross-crate API requests

- **app**: feed `CellState::recompute` the ops `Project::applied(doc, outcome)` lends (resolved commit ops, undo inverses) and drop the `commits` deque in `editor.rs` (elab and its oracle take those ops since `pr/clean-elab`).
- **ui**: switch the local line-record lexer to `schemify-format`. (**plugin** host done; `abi::text` stays the guests' encoder/decoder until `format` has a public value lexer, pinned to the format by `abi_and_format_lexers_agree`.)
- **format**: a `\u{+41}` escape is accepted (`u32::from_str_radix` takes a sign; the grammar says `1*6HEXDIG`). `abi::text` rejects it.

## app / editor

- **app**: `load_symbols` opens every instantiated cell to get its symbol; the library index now keeps rank 2 (`Project::interface(row)` has the symbol graphics, `Project::open_handle(row)` says whether the cell is open), so the editor can draw instances from the index. `scene::diff` (a positional scan per edit) can take the changed records from `Project::applied` instead.
- Editor gaps vs xschem (after the parity pass, `tests/parity.rs`; gestures in `tests/gestures.rs`): the web build has no system clipboard (D2 glue) and no exports to disk (OPFS); plugin I/O capabilities fetch/clipboard in the app; polygon vertices cannot be inserted (xschem Shift+B1) or deleted (Ctrl+B1), only dragged; a partial wire selection goes stale after its move is undone (rows keep the moved positions) and loses a wire whose stretched end turned into an L (Manhattan mode); layer colors, dim and line width changed in the editor last for the session (`init.lua` keeps them, config spec §3).
- **Exports**: PDF/EPS embed whole UI faces (Noto Sans ~190 KB, KaTeX Main ~65 KB when used, ASCII85; twice that as EPS hex) instead of subsets of the glyphs used; their text is placed glyph by glyph (no kerning or ligatures); characters neither face has print `.notdef` (the canvas falls back to system fonts). The export page, PNG scale and package status are commands without default-UI menu rows yet (`options.export_page_size`, `options.png_export_scale`, `file.image_export.png_export_full`, `tools.package_status`).
- **Make subcircuit** (Ctrl+Shift+H): selected ports stay in the parent (they are its interface); a pin direction is inferred from the moved instance pins on the net (`out` if one drives it, `in` if all are inputs, else `inout`); where no L wire from a new pin to a cut point avoids other nets, a label pair joins them.

## user config and packages (config spec)

- **cli/pack**: `url` packages are one `.wasm` file (no archives); `pack` has no parallel fetches.

## web (web spec)

- **cli/platform**: the whole OPFS tree lives in memory and a changed file is rewritten whole (a journal append rewrites the journal). Upgrade: mount the open project with `/config` and `/state` only; append in place (`createWritable({keepExistingData})` + `seek`).
- **cli/platform**: a refused mount or a failed OPFS write is reported on the browser console only (plus the unload guard), not in the status line.
- **app**: the URL fragment is the only argument on the web (no `--clean`, `-u`, `--no-default-ui`).
- **Simulation on the web** (D16; designed in web spec §6, not implemented). In order, by owner:
  1. **abi/plugin**: `RunJob { job, lang, dir }` + the netlist as NUL payload; output record `job <job> done|failed <message>` into a `jobs` request queue, taken after the delivery's file writes.
  2. **cli/platform** (web): `fetch_start(url, body) -> Spawned` (natively `curl`, on the web `window.fetch`); **app/plugins.rs**: execute `fetch` through it, as `spawn`.
  3. **run + cli/spines**: on the web `Run` holds a plugin runner with `Runner`'s five calls; `RunnerConfig`/`SimCommand` build for wasm.
  4. **app/sim.rs**: compile on wasm (run history through `platform::list_files`, not `std::fs`); route `RunJob`/`job` between the run and the host.
  5. **web**: `plugins/remote-runner` and a fake endpoint in the smoke test.

## docs page (D19–D21)

- A structural edit (instance insert/delete) still rebuilds the cell state (136 ms at 100k objects when the cell has vars): left to the in-flight incremental-elab work (**elab**: per-net connectivity and checks, then instance blocks in the graph), which owns `CellState::recompute`.
- Plot overlays: at most 16 jobs per slot, read from disk on each refresh (no per-job raw cache); cells below the bench are found among open documents (results reloaded at open reach the bench's direct children only, by stem).
- D20 caches of parked (shown before, not active) documents are written when they are activated again.
- `gmid-lut` characterization uses `spawn:ngspice`, not `run` requests: the ABI cannot (**abi**/**plugin**) run a bench the plugin just wrote (cells are indexed at project load) or hand back a job's raw file/directory (`RunDone` and `Results` carry scalar outputs; the LUT needs the (L, VDS, VGS) grid). Needs a "run this written bench" request and the job directory in `RunDone`.
- Integration points still stubbed: symbols via elab.

## run in the editor (M2 integration)

- OP quantities are `.save`d only for MOSFETs the netlist names (`M…` leaves): a PDK device wrapped in a model subcircuit (`XM1` → `m.xm1.msky130_fd_pr__nfet_01v8`) has an inner name the netlist does not know. Xyce runs get no probe lines (lead currents need `.print`).
- Staged edits: an inserted instance is drawn as a box with its name, not its symbol (the stage's records are not elaborated).
- Waves dock: keyboard focus follows the pointer; the divider position is not kept across sessions.
- Netlist text tab: a prose code block capped at 2^14 lines / 256 KiB (the file has everything); find lists matching lines, it does not scroll to them in the full text.
- Highlight keys of nets the name map lacks fall back to the level path (no cross-hierarchy match then). OP voltage marks of a bus net use its first bit.
- The OTA fixture has no bench; the GUI run tests and `docs/screenshots/simulate.png` use `crates/app/tests/fixtures/rc`.

## Decisions pending (user)

- **cktImg upstream PR** OmarSiwy/cktImg #5 (separations batched past 256 rounds only; fixes #4's slowdown on regular decks): merge, then re-point `extern/cktimg` at the merged `main` commit.


## command stream (rpc, D27)

- The editor's server has no `edit`/`netlist` methods of its own (its table is the command table; use the headless server for files the editor does not hold open). Command answers carry the status line only, not structured results (a `sim.netlist` answer does not include the text).
- `schemify rpc --listen` serves one client at a time; Windows has no Unix-domain sockets in `std` (TCP loopback only; named pipes need `windows-sys`).
- `schemify mcp` exposes the headless methods only (no editor tools, no MCP resources or prompts); a bridge to a running editor's socket would add its command table as tools.

## plugins

- Web: plugins with a fuel override run their events on the frame thread (no threads in `wasm32-unknown-unknown`), so a large cktimg import still blocks the browser tab. Slice them with wasmi resumable calls (out-of-fuel resumption) per frame.
- A plugin's `eval` builds the document's `CellState` once per delivery (100+ ms at 100k objects); pass the editor's own state for the active document if eval-heavy plugins appear.

## cktimg plugin

- **Fuel**: cktImg needs ~10^8 wasmi fuel per millisecond of native layout; 11 of 80 fixtures (one large flat cell each) exceed the default 10^8 per event and abort with `E_FUEL` unless the project manifest sets `fuel` for the plugin (one cell per event, so the budget is per cell); at the ceiling (10^11) all but `bus_sram_cell_array` import. A faster cktImg layout (`docs/upstream/cktimg-performance.md`) is the other lever.
- Generated leaves are shared by every import into a directory and kept when they exist, but their stems do not encode their pin count (`spice_x0`, a stub `sub` of 2 pins vs 3): two imports that need different leaves of one name get the first one's.
- `*@ expand` is ignored (every subckt stays a cell); cktImg's default block sides by name (supplies top, grounds bottom) are not used, only its directives.

## netlist

- Remaining corpus differences are owned and annotated in `tests/corpus/RATCHET.md` (SPICE 246/301 + 23 golden-unevaluated, CDL code-stripped 227/301 + 32, 22 gate-blocked).
- **import**: parametric bus widths: `ADD[dim-1:0]` (template parameters: needs widths resolved per instance, a variant cell per distinct value, and the cell's own labels resolved too; the only corpus bench, ram_tb, has an invalid-SPICE golden with literal `DIN[width-1:0]` ports) and `A[max:0]` (no parameter: the pin takes the attached net's width, an elab rule; test_ngspice also needs `~`). Per-instance `spice_sym_def` built by Tcl (`[abs_sym_path f]`, `[read_data_nonewline …]`) stays reviewed (inst_sch_select).
- **elab** (asked by import): XSPICE inverted-net labels `~D[5..0]` import as the bundle `~D5,…,~D0`, but the net grammar (netlist §4 `ident`) has no `~`, so the bundle is one scalar name and the pin is `E011` (sar_adc, tb_sar_adc, test_ngspice): allow an optional leading `~` in `ident` (netexpr.rs), the bit name keeping it (ngspice inverts the node).
- **elab**: xschem replicates a 1-pin instance on an N-bit bus (`.save v(count[k])` per bit) and sizes an unnamed net on an iterated instance by pin width × elements; both need a rule that keeps the sky130 adders' broadcast.
- `file` stays relative in templates (netlist §7.6.14): absolute paths would break byte-identical output across checkouts; decide on project-relative paths if a template needs one.
- Precompiled OSDI cells stay lang `veriloga` (no `file`): their body is simulation-only text the netlister keeps out of CDL, as xschem keeps it in its architecture block; lang `spice` would put `pre_osdi` loads into LVS netlists.
- Spectre has no second oracle (snapshots only): no Spectre-syntax simulator runs in CI. Verilog is compiled by Icarus in CI (`netlist/tests/iverilog.rs`).

## run

- Xyce runs get no Monte Carlo seed (`-randseed` is a command-line flag; the runner's simulator line comes from the user config).
- ngspice `d_cosim` with an Icarus `.vvp` is compiled and wired but not simulated end to end in a test (Icarus needs `libvvp` for the `ivlng` shim).

## Performance

Numbers and how to reproduce them: `docs/perf.md`. D39 status: pan/zoom at 100k objects
0.13 ms CPU, 0.08 ms GPU (RTX; 0.4–1.1 ms on the Intel iGPU); flat 1M netlist 0.85–0.93 s;
param edit 1.8 ms; idle 0%. Not met: a local edit at 100k objects (~50 ms, D39 has no row
for it; the goal set for this work was ≤ 1 ms).

- **elab**: a local edit is patched (0.08 ms p50 at 100k); still rebuilt: instance
  insert/delete (the graph and instance rows), name/ignore/inherit edits, cells with `E003`
  shorts or `ignore short`, label edits when `global true` labels exist. At 1M objects the
  id-column diff and violation-table memmoves dominate (1 ms p50). The 1M flat netlist:
  generated-name interning, the rows loop and page faults on fresh buffers are what remain.
- **elab**: edits that change an expression's dependencies rebuild the whole graph (17 ms at 30k nodes).
- **app**: per edit, decor is rebuilt (~4.5 ms at 100k): an incremental decor (changed
  marks and objects only). Plugin snapshots still hash the document's columns per edit to
  find the changed groups; the edit's own ops could name them.
- **render/app**: `CanvasDiff` hashes every row of a changed table (~3.4 ms for the 320k
  rows of a 100k-object scene and its decor): `scene::update` and the decor builder
  reporting their changed rows would make it O(change). A click at 1M objects costs 17 ms
  (`scene::marks` scans every primitive's owner).
- **render**: tile frames on the Intel iGPU are not measured since the tiles landed (the
  retained layer had 47 ms p99 there).
- **format**: 100k-object parse ~52–60 ms; doc open 89 ms (budget 200 ms, OK); the 100k parse test sits near 100 ms under load.

## Correctness / semantics

- **elab**: `E002` doesn't yet compare a SPICE `body`'s `.subckt` ports with the symbol pins; `global true` names are gathered from every reachable schematic, including unselected impls; `net_<id>` may collide with a user label of the same spelling; `CellState` doesn't observe edits to other documents.
- **ui**: a live string handle is never reused before its 31-bit counter wraps; the shaped-text cache would need a generation in its key past that.
- **import (readers)**: cdfDump untested against a real dump: producing one needs Cadence Virtuoso (`cdfDump` is a SKILL function), which no CI or this machine has; the reader follows the documented format only.

## Parity gaps vs xschem (UI)

- **ui/render**: tear-off menus; Alt+letter menubar mnemonics (xschem's Alt chords own those keys; F10 opens the menubar). (Context menus are positioned dialogs, `UiState::set_dialog_at`; chooser lists scroll.)
- **waves** (spec waves): live annotation of the schematic at cursor b (`live_cursor2_backannotate`, `cursor_2_hook`); per-graph cursors (`private_cursor`: the vertical cursors are shared and drawn only in locked panes); the x-axis scrollbar strip (B1 drag below the plot); an alternate sweep variable per trace (`sweep`); rainbow colouring of steps; per-graph raw files; the dialog's signal list and node text (traces are added from the browser instead). Digital lanes share the plot height (no `ypos1`/`ypos2` scroll), are drawn sample-and-hold (no interpolated threshold crossing) and skip a non-ascending step; histograms count samples without weighting them by their x spacing.

## Parity: intentionally dropped

xschem menu items and keys with no meaning in Schemify, removed from the default UI (`plugins/default-ui`) rather than left unresolved. The parity ratchet (`crates/app/tests/parity.rs`) holds every remaining name to an implementation; `tests/shell.rs` lists the dropped inventory chords.

- File → Start new Xschem process (`x`), Alt+Shift+E / Alt+Shift+I "in a new process": one process with tabs; `x` and the Alt+Shift keys open a new tab instead.
- Options → Debug mode: logs go to the status line and stderr; there is no Tcl debug level.
- Options → No XCopyArea drawing model, Fix for GPUs with broken tiled fill, Fix broken RDP mouse coordinates, `$` / Ctrl+`$`: X11 drawing workarounds; vello draws every frame.
- Options → Enable infix-interface, Intuitive Click & Drag interface: Schemify keeps xschem's default interaction (infix and intuitive), one gesture state machine.
- Options → Undo buffer on Disk: undo always lives in the on-disk journal (D28).
- Options → Unsel. partial sel. wires after stretch move: partially selected wire ends stay selected after a move (xschem's default); Esc or a click elsewhere drops them.
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

## Testing infrastructure

