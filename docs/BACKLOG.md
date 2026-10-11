# Backlog

Open items collected from workstream reports. Each item names its owner crate. Remove an item in the change that resolves it.

## Cross-crate API requests

- **doc**: resolve a reference from a cell row; "is this row open" query; optionally have `apply_edits` report the changed records so callers don't pass ops back (elab).
- **doc**: a `journal` file kind or a public lexer in `format` (doc currently parses journal/op text through format's newer-version mode).
- **expr**: topological sort that reports every cycle in one pass (elab cuts one cycle per pass).
- **plugin**, **ui**: switch the local line-record lexers to `schemify-format`.

## app / editor

- **Tests open fixtures in place**, so a journal left by using the GUI on a fixture (`fixtures/*/.schemify/journal`) is replayed as crash recovery and breaks `tests/docs.rs` (happened 2026-10-10). Copy fixtures into a temp dir per test.

- **doc**: keep rank 2 (symbol graphics) in indexed interfaces (`parse_prefix(bytes, 2)`); the editor currently opens every instantiated cell to get its symbol.
- **render**: window has no exit hook.
- **ui**: text fields commit only on Enter (the app copies typed text every frame).
- **plugins/build.sh**: rebuilding `hello`/`gmid_stub` is not byte-reproducible.
- Editor gaps vs xschem: stretch/kissing moves, polygon/arc/circle drawing, schematic graphics (format has none), context menu, persistent wire mode, snap cursor/crosshair, search, Save As, rotated texts, scrolling lists (24 rows max), system clipboard, crash-recovery prompt, plugin I/O capabilities fetch/clipboard in the app.
- No user-config reader: the `sim ngspice` command line (format spec §4) is not read; the CLI and the editor use `ngspice -b -r {raw} {netlist}` (tests pass `editor::Options::sim`).
- Plugin `eval` requests still answer `E_GONE` (elab evaluator not wired to the host).

## docs page (D19–D21)

- Plot slots show the selected job only (no per-sweep or per-corner overlay) and only for the bench's own page or a cell it instantiates directly.
- D20 write-back covers vars only; own param defaults that call plugin functions keep their `cached` as written. Plugin functions return numbers/bools only (a string result needs an owned `Value`).
- The cache amend replays the edit through undo (two extra journal records); a `doc` "extend last entry" edit would save them.
- Values are kept for the active level's document; other open documents sync their caches when activated. A structural edit (instance insert/delete) rebuilds the cell state (136 ms at 100k objects when the cell has vars).
- Prose: no images, footnotes or nested emphasis inside links; tables use equal column widths; a `text_area` does not scroll inside itself (the page does).
- `gmid-lut`: queries at VDS = VDD/2 only; the sky130 pfet sample tops out near gm/Id 15 at L 0.15/0.18 µm; characterization uses `spawn:ngspice`, not `run` requests (the sweep needs the full grid, not bench outputs).
- Integration points still stubbed: symbols via elab, plugin `eval`, web OPFS.

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

- **Code inside a component's subcircuit** (5 corpus benches): xschem places code blocks (e.g. `.param`, `.include`) inside a component's schematic and emits them inside its `.subckt`. Today `.shcomp` cannot carry `code` (rank 6 is bench-only), so the importer moves it to the bench copy. Recommendation: allow `code <name> <lang>` in `.shcomp`, emitted inside every specialization of the component's subckt (format rank 6 + netlist §7). Needs a format/elab decision.

## plugins

- Plugin events run synchronously on the frame thread: a cktimg import of a large deck (up to 4·10^10 fuel with the new manifest `fuel` override) blocks the UI for seconds. Run long events on a worker.

## spicecmp

- Evaluate subckt defaults after the call's overrides (ngspice 45 order; the netlist importer plugin found it).
- Treat stray tokens on `.subckt` lines (`delay=200 ps`) consistently with ngspice.

## cktimg plugin

- **Fuel**: cktImg needs ~10^8 wasmi fuel per millisecond of native layout; 11 of 80 fixtures exceed the default 10^8 per event and abort with `E_FUEL` (up to > 4·10^10). Needs the per-plugin fuel override the plugin ABI (§2) specifies but the app does not implement yet (`plugins.load(.., FUEL_DEFAULT)` for every plugin), and/or a faster cktImg layout (`docs/upstream/cktimg-performance.md`). With a bigger budget, one cell per event (a timer between cells) keeps the editor responsive.
- **Size**: `cktimg.wasm` is ~630 KB (`plugins/build.sh` allows 700 KB for it, 200 KB for the rest); `docs/upstream/cktimg-optional-zon.patch` takes it to ~480 KB.
- **Upstream**: `docs/upstream/cktimg-lib-step.patch` (then `build.rs` can call `zig build lib`), `cktimg-optional-zon.patch`.
- Block symbols of generated subckt cells are the plugin's own (inputs left, the rest right); cktImg's `*@ left/right/top/bottom` directives are not read yet (a probe placement of one instance would give cktImg's block geometry).
- XSPICE `A/N/P/Y` and node-less `X` cards are kept as code text, not drawn.

## netlist

- Remaining corpus differences are owned and annotated in `tests/corpus/RATCHET.md` (SPICE 246/301 + 23 golden-unevaluated, CDL code-stripped 227/301 + 32, 22 gate-blocked).
- **format + elab + netlist** (largest remaining cause, 5 rows): netlist text inside a component's definition. xschem writes a subcircuit schematic's code blocks / `S {}` text inside its `.subckt` (XSPICE `A` devices, `.model` cards, extracted netlists); a `.shcomp` cannot carry `code`, so the importer moves it to the bench copy. Proposal: allow `code` records in a `.shcomp`, emitted inside each of its definitions.
- **import**: instance `format`/`lvs_format` overrides (9 rows) as per-instance variant cells; alternate `schematic=` per instance and back-annotation (4); parametric bus widths `A[max:0]`, `ADD[dim-1:0]`; XSPICE inverted-net labels `~D[5..0]`; `spice_sym_def=".include f.cir"` pin order from the included `.subckt`.
- **elab**: xschem replicates a 1-pin instance on an N-bit bus (`.save v(count[k])` per bit) and sizes an unnamed net on an iterated instance by pin width × elements; both need a rule that keeps the sky130 adders' broadcast.
- `file` stays relative in templates (netlist §7.6.14): absolute paths would break byte-identical output across checkouts; decide on project-relative paths if a template needs one.
- Precompiled OSDI cells stay lang `veriloga` (no `file`): their body is simulation-only text the netlister keeps out of CDL, as xschem keeps it in its architecture block; lang `spice` would put `pre_osdi` loads into LVS netlists.
- No corpus oracle yet for spectre/verilog (snapshots only).

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
- **format**: verbatim blocks merge as a unit (line-level diff3 is the upgrade); a clean merge can still violate kind rules (doc/elab must check).
- **plugin**: an earlier plugin's `set X p` survives a later full redefinition of node `X`; no auto-quarantine of repeatedly trapping plugins.
- **expr**: FFT twiddles use platform math (calculator output only, never netlists).
- **ui**: status text that changes every frame grows the string table; IME candidate window not positioned.
- **wave**: per-sweep-step readouts; FFT traces share the time-domain x axis.
- **import (readers)**: `.spiceinit` `sourcepath` lines, Magic `include`, `.if` blocks; cdfDump untested against real dumps.

## Parity gaps vs xschem (UI)

- **ui/render**: toolbar icons (text labels now), menu keyboard navigation, tear-off/context menus, scrolling, split dragging, dropdown popups.
- **waves**: horizontal cursors, dataset tracking, measurement tooltip, digital/bus mode, histogram, graph properties dialog, per-graph x locking, typed cursor positions, colour picker.

## Testing infrastructure

- Fuzz harnesses exist for format, expr and import, but there's no nightly toolchain installed locally; CI nightly placeholders need wiring.
- Determinism CI job (byte-identical netlists across Linux/macOS/Windows/wasm) is a placeholder until `netlist` lands.
- sky130 model files aren't fetched (open_pdks builds them); the ngspice second oracle for sky130 needs `volare`.
