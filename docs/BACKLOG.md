# Backlog

Open items collected from workstream reports. Each item names its owner crate. Remove an item in the change that resolves it.

## Cross-crate API requests

- **doc**: resolve a reference from a cell row; "is this row open" query; optionally have `apply_edits` report the changed records so callers don't pass ops back (elab).
- **doc**: a `journal` file kind or a public lexer in `format` (doc currently parses journal/op text through format's newer-version mode).
- **expr**: topological sort that reports every cycle in one pass (elab cuts one cycle per pass).
- **plugin**, **ui**: switch the local line-record lexers to `schemify-format`.

## app / editor

- **doc**: keep rank 2 (symbol graphics) in indexed interfaces (`parse_prefix(bytes, 2)`); the editor currently opens every instantiated cell to get its symbol.
- **render**: `draw_canvas` shares one text cache between scene and overlay (re-shapes labels every frame; the editor works around it, the demo doesn't); window has no exit hook or timer wake-up.
- **ui**: text fields commit only on Enter (the app copies typed text every frame).
- **plugins/build.sh**: rebuilding `hello`/`gmid_stub` is not byte-reproducible.
- Editor gaps vs xschem: stretch/kissing moves, polygon/arc/circle drawing, schematic graphics (format has none), context menu, persistent wire mode, snap cursor/crosshair, search, Save As, rotated texts, scrolling lists (24 rows max), system clipboard, crash-recovery prompt, plugin timers and I/O capabilities (spawn/fetch/clipboard) in the app.
- Edit round trip rebuilds the whole scene (40 ms at 100k objects): incremental scene rebuild.
- Integration points still stubbed: elab (symbols via elab, net highlight `k`, eval, net ids in snapshots), netlist (`n`), run (`s`, plugin run requests), web OPFS.

## run

- `format`: make var `dist` and optimize `seed` known attributes (they work as preserved unknowns today).
- ngspice's random seed isn't set per MC sample, so PDK `agauss` statistics aren't reproducible yet.
- Inline Verilog-A/Verilog `body` sources aren't compiled (only `file`); OpenVAF/Icarus steps tested with stand-ins only (tools not installed); compile blocks `poll` the first time.
- Optimizer runs only at the bench's default config/corner with unit goal weights.
- Relative `.lib` paths resolve against the job directory.

## Performance

- **elab**: a flat 1M-instance bench extrapolates to ~0.93 s of elaboration (D39 netlist budget is 1 s end-to-end): write rows directly instead of building temporaries.
- **elab**: edits that change an expression's dependencies rebuild the whole graph (17 ms at 30k nodes).
- **format**: 100k-object parse ~52–60 ms; doc open 89 ms (budget 200 ms, OK); the 100k parse test sits near 100 ms under load.
- **render**: 100k-primitive scene build is 3.9 ms on a fast CPU (budget 4 ms for the 8 ms frame); 26 ms GPU time on the Intel iGPU (120 fps unmet there). Options: parallel encoding, reusing the last scene while panning.

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
