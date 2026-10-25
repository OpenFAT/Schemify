# Performance (D39)

Measured numbers for the D39 budgets, the machine they were taken on, and how to
reproduce them. Numbers move with load and GPU clock state; every row below is one run
on an otherwise idle machine unless it says otherwise.

## Machine

| | |
|---|---|
| CPU | Intel Core i9-14900HX (8P + 16E cores, 32 threads), 31 GB RAM |
| GPUs | NVIDIA GeForce RTX 4060 Laptop (discrete); Intel Graphics (Raptor Lake-S iGPU) |
| OS | NixOS, Linux 7.0, Vulkan through the Nix `vulkan-loader` |
| Build | `--release` (`opt-level 3`, debuginfo), stable Rust, edition 2024 |

On NixOS the tests need the Vulkan loader on `LD_LIBRARY_PATH`, e.g.
`$(nix eval --raw nixpkgs#vulkan-loader)/lib:/run/opengl-driver/lib`.

## How to run

| What | Command |
|---|---|
| Frame times (pan, zoom, select, edit) at 10k / 100k / 1M objects | `cargo test --release -p schemify-app --test perf -- --ignored --nocapture --test-threads=1 frame_times` |
| Hover, select, drag, drop, menu, typing at 100k / 1M objects (asserts the 1 ms budgets at 100k) | `… --test perf -- --ignored --nocapture --test-threads=1 interactions` |
| Every edit kind at 100k objects (delete, undo, redo, move, wire, rename, param, the first edit; asserts the 1 ms budgets) | `… --test perf -- --ignored --nocapture --test-threads=1 edit_frames` |
| …on a given GPU | prefix `SCHEMIFY_GPU=<adapter name substring>` (default: the first non-CPU adapter) |
| Param edit with 1k dependents | `… --test perf -- --ignored --nocapture var_edit` |
| Flat 1M-instance netlist (elaborate + emit) | `cargo test --release -p schemify-netlist --test perf -- --ignored --nocapture --test-threads=1` |
| Commit frames vs the O(document) work they leave to idle steps (hash, compaction, first journal) at 100k | `cargo test --release -p schemify-doc --test perf -- --ignored --nocapture commit_frames` |
| elab cost of a local edit (p50/p95/p99) at 10k / 100k / 1M objects | `cargo test --release -p schemify-elab --test perf -- --ignored --nocapture a_local_edit` |
| Canvas cache and patch oracles (GPU) | `cargo test --release -p schemify-render --test gpu -- --ignored --nocapture --test-threads=1` |

The frame benchmark drives the real frame spine (`Editor::update` + `Editor::paint` into a
`ScenePainter` with raster layers) and renders each frame as the window does: vello into
the target unless the frame can be composed from the layer textures (`Layers::reusable`).
It reports per frame kind:

- **CPU update+paint**: `update` plus painting the frame scene (what the UI thread spends).
- **render calls**: layer raster, vello render or composition, blit (CPU side of the GPU
  submission).
- **GPU**: timestamp queries around the frame's GPU work, including a measured
  timestamp floor (printed with the adapter).

Frames: **pan** is a middle-button drag, 6 px per frame, 720 px out and back (a few frames
leave the recorded margin and re-record: they are the p99); **zoom** is bursts of 6 wheel
notches plus the settle frame that redraws crisp (the p95/p99); **select** is a click on a
wire; **edit** is a wire delete and its undo.

## Results (2026-10-11)

Before = `main` at `aec8c6a` (every frame re-encoded); retained layer = PR #1 (one layer per
canvas with a margin); tiles = `pr/perf-tiles` (world-anchored tiles, per-frame record budget,
level of detail, batched patches). p50 / p95 / p99 in ms, RTX 4060 unless it says otherwise.

### 100k objects (260k primitives)

| Frame | CPU before | CPU retained layer | CPU tiles | GPU retained layer | GPU tiles |
|---|---|---|---|---|---|
| pan | 10.9 | 0.06 / 0.20 / 12.8 | 0.092 / 0.116 / 0.403 | 0.074 / 0.086 / 5.5 | 0.083 / 0.096 / 0.366 |
| zoom | ~11 | 0.09 / 13.6 / 18.2 | 0.224 / 0.926 / 0.992 | 0.081 / 5.9 / 5.9 | 0.360 / 0.544 / 0.551 |
| select | ≈ a full frame | 1.95 / 2.9 / 3.2 | 1.10 / 1.86 / 2.50 | 0.47 / 0.50 / 1.0 | 0.47 / 0.51 / 2.0 |
| edit | 112 | 48 / 60 / 75 | 65 / 72 / 95 (app side, untouched here; run-to-run 48–74) | 1.05 / 1.3 / 1.3 | 1.98 / 3.4 / 3.4 |

GPU before: 1.9 ms (RTX) and 26–37 ms (Intel iGPU) per frame, every frame. The retained
layer's Intel numbers: pan 0.37–1.1 / 0.39–2.3 / 47–51, zoom 0.40–1.1 / 47–50 / 47–55 (its
timestamp floor moves from 0.02 to 0.11 ms with the clock state). The tiles were not
re-measured on the Intel iGPU (one release run per change on the shared machine).

### 10k and 1M objects (tiles, RTX 4060)

| Frame | 10k CPU | 10k GPU | 1M CPU | 1M GPU |
|---|---|---|---|---|
| pan | 0.18 / 0.27 / 0.31 | 0.073 / 0.085 / 0.36 | 0.20 / 0.29 / 0.64 | 0.081 / 0.095 / 0.57 |
| zoom | 0.16 / 0.29 / 4.0 | 0.093 / 0.51 / 1.2 | 0.99 / 2.0 / 2.5 | 0.61 / 0.67 / 0.69 |
| select | 0.36 / 0.47 / 1.0 | 0.34 / 0.36 / 1.9 | 6.7 / 6.8 / 7.2 | 0.51 / 0.56 / 2.0 |
| edit | 4.9 / 7.4 / 7.6 | 1.8 / 1.9 / 2.0 | 570 / 630 / 718 | 8.9 / 12.4 / 21 |

Open + first build: 22 ms (10k), 248 ms (100k, under load; 166–174 ms idle), 1.98 s (1M).
The 10k zoom p99 is one frame: the first crisp frame after a zoom records every visible tile
when no stand-in covers them yet.

### Canvas floats over the layers (issue #45, 2026-10-12)

The view switcher floating over the canvas (`c11ca50`) is painted after the canvas layer, and
`Layers::reusable` refused any frame that painted after its layers: from then on every pan
and zoom frame went through vello (0 of 240 pan frames composed). Now what a frame paints
over its layers (floats, popups) is also recorded on its own (`Layers::top`), rasterized
once per change into a frame-sized texture, and blended over the layer quads; composed
frames match vello's within 1/255 (`gpu.rs` `composed_frames_equal_vello_frames`). A float
inside a clip opened before the layers, or under a later layer draw, still goes through vello.

`grid_keeps_up_with_pan_and_zoom` (symbol contact sheet, GPU p50 / p95 ms per frame, Intel
iGPU, timestamp floor ≈ 1.9 ms included; it fails unless every camera frame is composed):

| Frame | 1× before | 1× after | 2× before | 2× after | 2× after, RTX 4060 |
|---|---|---|---|---|---|
| pan | 5.5 / 7.2 | 1.1 / 2.1 | 14.1 / 17.8 | 4.1 / 5.7 | 0.13 / 0.14 |
| zoom | 6.7 / 10.6 | 1.3 / 2.7 | 21.4 / 26.5 | 4.4 / 9.4 | — |

`frame_times_10k` pan on the Intel iGPU: 5.7 → 2.1 ms (floor 1.9). The grid itself is not a
cost here: turning it off changes neither number. After a zoom settled, the view needed 4 frames
at 2× to record every tile (16 tiles per frame, top-left first), so a zoom-out showed bare
background at the bottom for those frames. `TILES_PER_FRAME` is now 64 and `COST_PER_FRAME`
alone bounds the frame: a settled zoom is whole in the next frame at 1× and after 2 extra frames
at 2× (`grid_keeps_up_with_pan_and_zoom`); the 10k/100k/1M budgets still hold.

Retained layer at 1M for comparison: pan 0.08 / 0.22 / 107 CPU, 0.063 / 0.075 / 39 GPU; zoom
0.07 / 103 / 109 CPU, 0.082 / 39 / 39 GPU.

### Netlist, flat 1M instances (elaborate + emit, document open)

| | before | after |
|---|---|---|
| elaborate | 1.28 s | 0.70–0.79 s (0.75 after the merge) |
| emit | 0.15 s | 0.14 s |
| total (budget 1 s) | 1.43 s | 0.85–0.93 s (0.90 after the merge) |

Second pass (2026-10-11, `pr/elab-incremental`; machine shared with other agents at load
average 40–90, so these are upper bounds): elaborate 0.49 s cold / 0.35–0.44 s warm, emit
0.08–0.11 s, total 0.60 s cold / 0.43–0.51 s warm (0.94 s before, at load 5). Large cells
use a second thread (connectivity beside the graph, element names beside the geometry,
emission in row chunks); output is byte-identical to the serial run (oracle
`netlist/tests/metamorphic.rs` `threads_change_nothing`).

### Local edit, elab cost (`CellState::recompute`), p50 / p95 / p99 ms

200 random edits plus their undos (wire delete/insert/move, label renames that merge by
name, device moves) on a tiled schematic with shared `gnd`/`vdd!` nets.

| Objects | before (full rebuild) | after (patched) |
|---|---|---|
| 10k | 3.1 / 3.2 / 3.4 | 0.011 / 0.027 / 0.035 |
| 100k | 34.6 / 36.4 / 37.0 | 0.079 / 0.163 / 0.186 |
| 1M | — | 0.94 / 2.37 / 3.17 |

### Other D39 rows

| Budget | Measured |
|---|---|
| param edit → recompute + redraw ≤ 8 ms, 1k dependents | 1.8 ms (plugin-backed functions 3.2 ms) |
| idle CPU 0% | no wake-up after a pan or zoom frame (asserted by the frame benchmark and `an_idle_frame_does_no_work`) |

## What makes a frame cheap

- **Instance rows** (`render::gpu`, render-backend steps 1–2): every canvas primitive is one
  retained row, grouped by palette layer, packed once and patched where the tables' reported
  changes (or `CanvasDiff`'s edit script) say. A pan or zoom writes no row, only the camera
  uniform (`a_pan_or_zoom_frame_writes_no_instance_bytes`); every frame draws the whole view,
  so there is no tile cache, no stand-in and no settle frame. The CPU painter (goldens, the
  GPU-less web) rasterizes the same rows (`canvas::draw_rows`), so every per-row decision is
  made once, in the pack.
- **Index and level of detail**: canvas texts are read only near the view (`PrimIndex`, a CSR
  grid, about 16 rows per cell), and a table whose largest label is under 3 px is skipped
  whole; rows ≤ 2 px on screen are drawn as the whole pixels their box touches (the vertex
  shader on the GPU, `draw_rows` on the CPU).
- **Vello-free frames** (`render::Layers`): when nothing in the UI changed but the camera and
  the live layers (the status bar), the window blits the last UI target and composes the
  live layers as textured quads instead of running vello.
- **Incremental scene and checks**: `scene::update` re-stamps only changed objects; check
  passes post their delta (`Annotations::patch`); undo/redo recompute over the undone entry's
  ops instead of rebuilding.
- **Glyphs shaped once per string** (`CanvasScratch`).

## Where the time still goes

At 100k objects an edit (delete a wire, undo) now costs 23 / 28 / 45 ms CPU (p50/p95/p99,
RTX run on `pr/perf-edit`; 1M: 251 / 331 / 452 ms). Elab spends ~0.1–0.2 ms (was ~28 ms).
Plugin snapshots refill only the groups whose fingerprint moved (a hash pass over the
document's columns instead of copies of every table). An edit's patches read the rows near
them through `PrimIndex`, which follows the diff's edit script instead of being rebuilt.
What is left: ~4.5 ms rebuilding decor, ~3.4 ms diffing the canvas tables (the diff hashes
every row of a changed table), ~1.5 ms in the scene update, and the fingerprint pass. See
`docs/dev/backlog.md` (Performance) for the next steps.

## Frame budget pass 2026-10

Goal: every interactive frame (pan, zoom, hover, select, drag, typing in a field, opening a
menu) well under 1 ms of CPU at 100k objects, with no hitch. Measured on `gui-arch-audit`
(`af27b3f6`), RTX 4060, load average 4–7 (other agents building), by
`crates/view/app/tests/perf.rs` `frame_times_100k` (pan, zoom, select, edit) and the new
`interactions_100k` (hover, select, drag, drop, menu, type). Per-step times came from
temporary `Instant` laps around each spine step; the edit loop was also profiled with
`perf record`.

### Before: per interaction (CPU update+paint, p50 / p95 / p99 ms; "render calls" is the CPU
side of the GPU submission, which the window spends on the same thread)

| Frame | 100k CPU | 100k render calls | 1M CPU |
|---|---|---|---|
| pan | 0.154 / 0.274 / 0.423 | 0.170 / 0.425 / 1.279 | — |
| zoom (+ settle frames) | 0.317 / 0.776 / 0.833 | 0.420 / 0.748 / 0.851 | — |
| hover | 0.382 / 0.586 / 1.460 | 0.300 / 0.622 / 1.472 | 0.377 / 1.233 / 1.352 |
| select (click) | 0.780 / 0.909 / 1.135 | 0.614 / 0.682 / 0.775 | 7.0 / 11.8 / 12.1 |
| drag (move preview) | 0.601 / 0.619 / 0.622 | 0.303 / 0.323 / 0.334 | — |
| type (props dialog) | 0.427 / 0.451 / 0.481 | 0.317 / 0.366 / 0.377 | 0.424 / 0.445 / 0.446 |
| menu (open) | 0.430 / 5.7 / 5.7 | 0.747 / 0.763 / 0.763 | 0.279 / 52 / 52 |
| edit (Del, undo) | 16.0 / 20.1 / 33.7 | 0.715 / 0.769 / 0.787 | — |
| drop (end of drag) | 27–29 | 0.72 | 311 |

`render.scene_100k_ms` (the uncached 100k-primitive scene encode, `render` crate) measured
4.54–4.56 ms in three runs at load ≈ 6: over its 4 ms budget, confirmed. The editor never
encodes that scene per frame (tiles); the benchmark guards the fallback path.

### Where each frame goes (100k, mean ms per step)

- **Every frame whose status line changes** (hover, drag, type, select, menu): the app's state
  contribution is regenerated as text twice per frame (top of the frame and step 10, 0.04–0.08
  ms together), and because `status.msg` shows the pointer's coordinates its text differs, so
  `Shell::refresh` recomposes every contribution (`ui::compose`, 0.12–0.15 ms) and lays the
  whole UI out again (`ui::layout`, 0.07–0.10 ms). That is 0.25–0.3 ms of a 0.38 ms hover frame.
- **select**: `scene::hit` tests every object (0.42–0.55 ms at 100k, 6–9 ms at 1M), then
  `scene::marks` scans every primitive's owner for the selection bitmap (0.15 ms; 1.75 ms at 1M).
- **drag**: `scene::push_moved` scans every primitive for the selected ones each frame (0.2 ms;
  O(primitives)).
- **hover hitch** (p99 1.4 ms): the first ~6 input frames after a zoom settles record the 1-tile
  ring around the view (0.47–0.75 ms each). Ring tiles are recorded only on frames something
  else asked for, so they ride on the next input frames.
- **zoom**: the settle frames record tiles within `COST_PER_FRAME` (≈ 0.8 ms).
- **menu hitch** (5.7 ms; 52 ms at 1M): the first plugin event after an edit refills the plugin
  snapshot (`Host::snapshot`: wires, labels and nets groups), because the edit frame delivered
  `DocChanged` before `elab` produced the new check pass and the snapshot key holds the pass.
  Every edit therefore pays the snapshot twice: once in its frame, once at the next plugin event.
- **edit** (16 ms, 100k): plugin delivery + snapshot 3.0, `checks::decor` 4.3 (it re-places all
  70k violation marks of the fixture, parsing each mark's id and looking it up), canvas diff
  3.1 (hashes every row of the scene, decor and marks tables), tile patches 1.0–2.6, elab +
  annotation patch 1.7, scene update 1.2, UI 0.3. `perf`: `draw_prims_in` 17 %, `checks::decor`
  11 %, snapshot (`Host::snapshot`, `put_le`, `fill*`) 15 %, `CanvasDiff::update` + `damage::row`
  15 %, `Annotations::patch` 5 %, `scene::update` 3–8 %, `Document::find_id` 3–7 %.
- **first edit of a document** (13 ms extra): its journal is created with an atomic rewrite of
  the whole document text (`Session::flush_journal`, file I/O in the frame).
- **accesskit**: `access_tree` costs 0.01–0.02 ms and runs only when an assistive client is
  active (`update_if_active`). Not a cost.

### Window path (`crates/view/render/src/window.rs`)

`PresentMode::AutoVsync`. Input is batched: `about_to_wait` runs one `update` per event batch
and requests a redraw only when it returns true; ticks come only from `wake_ms`, so an idle
editor runs no frame (asserted by the zoom benchmark). A pointer move over the canvas always
returns true (the status line shows the pointer), and every such frame changes the frame hash
outside the layers, so it goes through vello (0.3 ms CPU in render calls) instead of composing
the layers: only 15 of 240 hover frames composed. Allocation per steady frame: the state
contribution's text, the `format!`s of the status line, a fresh overlay table, the request
vectors (a few dozen small allocations; never per object).

### Plan, ranked by what a user feels

1. **Plugin snapshot paid twice per edit** (hitch 5.7 ms / 52 ms at 1M on the first plugin event
   after an edit). The snapshot's per-group key moves when the check pass moves, so the group
   fingerprint stays, but the refill is keyed by (doc epoch, pass): key the wires/labels group
   by its own columns only and the nets group by the pass, so the pass change refills just the
   nets group. Spine step 4 unchanged.
2. **Hit test** (select 0.45 ms → ≈ 0.05; 1M 6–9 → ≈ 0.6): an integer box prefilter over
   `Objects::geom` (branchless, vectorizes) collects candidates in ascending order; the exact
   distance test runs on them only, so the answer equals the full scan's (oracle test).
3. **UI state as a set patch** (≈ 0.12–0.15 ms off every hover, drag, type and menu frame):
   when the state contribution differs from the last one only in `set` values, `ui` patches
   those props in the composed tables instead of recomposing (oracle: equal to `compose`);
   layout stays (a label's width can change). Step 10 then composes only on a
   structural change (tabs, dialogs, docs page).
4. **The selection's primitive rows** (drag 0.2 ms → O(selection); select's marks scan): kept
   per selection change, keyed by the scene generation and the selection; the move preview
   transforms only them.
5. **Tile recording per frame**: the ring ahead of the view is recorded only while the frame's
   own recording left budget, and the per-frame cost budget drops so a settle or ring frame
   stays under ≈ 0.5 ms; the rest goes to the next frames through `wake_ms` (amortized, no
   frame hitches).
6. **Edit frames** (16 ms): decor's violation marks resolved once per annotation change (mark →
   object), so a scene change re-places them without parsing ids; the canvas diff of the decor
   then sees only the moved marks. Remaining after this pass: the scene diff, elab, the canvas
   diff's hashing of the scene table and the snapshot copy of the changed group, each an O(n)
   pass over 100k rows (≈ 1–3 ms each). The upgrade is builders that report their changed
   rows (scene → canvas diff → tiles), so an edit is O(change); it is a larger change than
   this pass.
7. **First-edit journal** (13 ms once per document) and **UI frames through vello** (0.3 ms of
   render calls per hover frame): left as they are and named here.

### What was done (in plan order, as measured)

1. Snapshot: the frame whose check pass moved after its delivery asks for one tick
   (`wake_ms` 0) that refills the snapshot, keyed so that only the net columns and the Nets
   group refill while the document's epoch stands (its fingerprints are reused). Menu after an
   edit 5.7 → 0.5 ms (1M: 52 → 0.5); the tick costs 2.2–3.8 ms at 100k (26–38 ms at 1M) on a
   frame no input waits for.
2. Hit test: the integer prefilter, measured branchy (a 64-object mask word was slower without
   SSE4.1 min/max). 0.45 → 0.16 ms per click at 100k, 6–9 → 1.6 ms at 1M. A grid over the
   objects (prototyped) answers in O(cell) but cost ~40 ns per object to build after every
   scene change (4 ms at 100k), so it was dropped.
3. UI state: `ui::patch_last` (not `patch_sets`), plus the top-of-frame compose only when the
   state changed outside a frame (`ui_stale`), the refused-commands block kept by a key, and the
   tokenizer reusing its buffers.
4. The selection's rows: `scene::selected_prims` once per selection or scene change.
5. Tiles: `COST_PER_FRAME` 80 000 → 48 000, the ring at most a quarter of it. 40 000 gave zoom
   p99 0.52 ms but ~23 frames to a whole view; 48 000 gives 0.54 ms and ~15 frames (80 000: 0.83
   ms, ~7).
6. Edit frames: only the snapshot part (above). The rest is the O(n) list in item 6 (scene diff
   1.2 ms, canvas diff 3.1, decor 4.3 on this fixture's 70k violation marks, elab 1.8, patches
   1–2.6). The fixture marks every wire with a violation; a design with few violations spends
   ~4 ms less here.

### After (2026-10-18, same machine at load average ~20, CPU update+paint p50 / p95 / p99 ms)

| Frame | 100k before | 100k after | 1M after |
|---|---|---|---|
| pan | 0.154 / 0.274 / 0.423 | 0.089 / 0.098 / 0.309 | 0.089 / 0.180 / 0.356 |
| zoom (+ settle frames) | 0.317 / 0.776 / 0.833 | 0.251 / 0.480 / 0.518 | 0.433 / 0.909 / 1.113 |
| hover | 0.382 / 0.586 / 1.460 | 0.193 / 0.210 / 0.238 | 0.194 / 0.207 / 0.223 |
| select, fit to window | 0.901 / 1.641 / 2.023 | 0.485 / 0.630 / 0.687 | 3.754 / 4.033 / 4.153 |
| select, zoomed in | 0.780 / 0.909 / 1.135 | 0.496 / 0.581 / 0.698 | 4.496 / 4.609 / 4.614 |
| drag (move preview) | 0.601 / 0.619 / 0.622 | 0.198 / 0.206 / 0.209 | 0.200 / 0.224 / 0.234 |
| type (props dialog) | 0.427 / 0.451 / 0.481 | 0.241 / 0.247 / 0.262 | 0.235 / 0.248 / 0.269 |
| menu open | 0.430 / 5.7 / 5.7 | 0.199 / 0.505 / 0.505 | 0.201 / 0.514 / 0.514 |
| edit (Del, undo) | 16.0 / 20.1 / 33.7 | 13.7 / 16.9 / 28.5 | 151 / 541 / 552 |
| drop (end of drag) | 27–29 | 27.4 | 307 |

The render calls (the CPU side of the GPU submission, on the same thread) add 0.16–0.45 ms at
p50 (hover 0.29: its frames still go through vello, item 7). Allocations, counted with
valgrind on the `inv` fixture: an update with no input and nothing due 19 (11 KB; was 133), a
hover frame 43 (43 KB; was 208). The window runs no frame while `wake_ms` is `None`
(`an_idle_frame_does_no_work`, and the zoom benchmark's idle assertion).

What bounds the rest:

- **1M select** (≈ 4.5 ms): three O(objects) scans per click (hit 1.6, connection points 1.4,
  selection rows 1.6 ms). A spatial index answers in O(cell), but it must follow
  `scene::update` (its build is 40 ms at 1M), so it belongs with the edit work below.
- **Edits** (13–14 ms at 100k): every step is an O(n) pass (scene diff, canvas diff hashing,
  decor, elab, snapshot copy). Making an edit O(change) means builders that report their
  changed rows down the chain (scene → canvas diff → patches; annotations → decor); each step
  is 1–4 ms today, so no single fix gets an edit under a frame.
- **First edit of a document**: + 13 ms (`doc` writes the journal with the whole document).
- **`render.scene_100k_ms`**: 4.49–4.59 ms, over its 4 ms budget, confirmed. 45 % of it is vello's
  path encoding of 100k visible paths; the editor never encodes them per frame (tiles), so it
  guards the uncached fallback only.

## Edit frames pass 2026-10

Goal: every edit frame (delete, undo, redo, the drop of a move, a property change, a wire drawn)
under 1 ms of CPU at 100k objects, the ticks after it too, and no hitch on the first edit of a
document. Measured on `main` `c61f0e85` (RTX 4060 host, no GPU columns: CPU only, load 6–8) by
the new `edit_frames_100k` (`crates/view/app/tests/perf.rs`: a dozen frames of each kind on the
`interactions` fixture, zoomed in, each followed by the ticks the editor asks for). Per-step
times came from temporary `Instant` laps around each spine step.

### Before (CPU update+paint, p50 / p95 ms; tick = the frame `wake_ms` asks for right after)

| Edit | 100k frame | tick |
|---|---|---|
| first edit of the document (a delete) | 28.4 | 3.4 |
| delete | 13.9 / 14.1 | 2.3 |
| undo | 12.1 / 12.3 | 2.3 |
| redo | 12.4 / 12.6 | 2.3 |
| move (drop) | 12.5 / 19.2 | 2.3 |
| wire (wire tool click) | 10.7 / 10.9 | 2.3 |
| rename (instance name, props dialog) | 13.0–14.0 (60–71 on a duplicate name) | 2.4 |
| param (instance `W`, props dialog) | 52–60 | 2.4 |

### Where an edit frame goes (100k, ms)

- `checks::decor` **4.3–4.6**: rebuilds every decor row; the fixture has 70k violation marks
  (every wire floats), each parsed from its target id, looked up (`find_id`, `scene.find`) and
  drawn as a rect and a text.
- canvas diff **3.0–3.1**: `CanvasDiff::update` hashes every row of the scene (≈ 200k rows) and
  of the decor (≈ 140k), then diffs the hash sequences.
- plugin snapshot **1.2** in the frame (`DocChanged` refills the wires group and fingerprints
  every instance, wire and label column) plus **2.1** in the tick after (the new check pass:
  net columns, the Nets and Violations groups).
- `Annotations::patch` **1.0**: moves all 70k marks (80 bytes each) to place two.
- `scene::update` **0.5–1.3**: the positional row diff of every table (0.43), then per removed
  object two scans of every owner column and of `conn`, a renumbering scan, the sorted index.
- tile patches **0.05–1.6**: after 16 edit scripts (or 4096 added rows) the `PrimIndex` asks to
  be rebuilt and every patch culls every row (0.8 ms per patch) until a tile records, which then
  rebuilds the index: **7 ms** for the scene, 1.3 for the decor (the first click after open, 30 ms,
  is this plus the first diff fill and snapshot).
- elab **0.4** (connectivity patch and the violation delta), UI **0.1**.
- first edit: **+12.6** in `apply`: the journal's first `base` record hashes the whole file
  (SHA-256 of the 100k-object text) in the frame.
- param edit: elab **13** (values changed: the whole check pass reruns) and `scene::build`
  **29** (values changed: every instance re-stamped), the decor and diff as above.

### What was done (each result equals its full rebuild; oracle tests named in the commits)

1. **`doc` change log** (`Document::cursor`, `changes_since`): every row write (insert,
   delete and the row its swap moves, put, reorder) is logged as `(table, what, row, id)`,
   bounded at `LOG_MAX`; a compaction, a reload or a copy breaks every earlier cursor.
2. **Scene from the log, rows that stay put**: `scene::update` re-stamps only the touched
   objects; a removed object's rows are killed in place (`LAYER_DEAD`), objects carry a stamp
   so their rows are found by binary search; the scene reports its killed rows
   (`Scene::edits`), the canvas diffs the scene table from them (`TableChange::Rows`) and the
   `PrimIndex` follows them (`PrimIndex::follow`) instead of a rebuild every 16 edits.
   `selected_prims` finds the selection's rows by stamp (select 0.49 → 0.27 ms).
3. **Violation markers per object** (`checks::Markers`, the fifth canvas table): an edit
   re-places only the touched objects and the patched marks' targets. `elab::Annotations` is
   an ordered map (a patch is O(changes · log n)).
4. **Plugin snapshot patched**: document groups from the change log, net columns and Nets by
   comparison with what they hold, Violations from the pass's own delta
   (`CellState::changes`, placed with `Violation::key_cmp`).
5. **elab**: connectivity findings in a map keyed by raw handles (no whole-table moves).
6. **First edit**: the disk hash the journal's `base` record names is taken 128 KB a tick
   after the document opens (`Project::idle_step`), so the first commit appends its records.

### After (2026-10-18, same machine, load ~5, best of 3 runs; CPU p50 / p95 ms)

| Edit | before frame | after frame | after tick |
|---|---|---|---|
| first edit of the document | 28.4 | 2.71 | 1.88 |
| delete | 13.9 / 14.1 | 0.568 / 0.592 | 0.67 |
| undo | 12.1 / 12.3 | 0.506 / 0.531 | 0.64 |
| redo | 12.4 / 12.6 | 0.519 / 0.528 | 0.66 |
| move (drop) | 12.5 / 19.2 | 0.611 / 0.627 | 0.66 |
| wire | 10.7 / 10.9 | 0.402 / 0.429 (p99 0.87) | 0.56 |
| rename | 13.0–71 | 55.0 | 0.68 |
| param | 52–60 | 58.2 | 0.29 |

(The rename and param rows are not like for like: the old benchmark's property edits often
typed the value the field already held, which commits nothing; they now type a new value each
time.) `frame_times_100k`'s edit frames went 13.7 → 0.73 ms p50 and `interactions_100k`'s drop
27.4 → 1.6 ms (its single drop is the document's first edit). The other interactive frames are
unchanged or faster. `edit_frames_100k` asserts p95 and the ticks under 1 ms for delete, undo,
redo, move and wire, and the first edit under 4 ms.

What bounds the rest:

- **Rename and param edits** (55–58 ms): a rename rebuilds the connectivity (elab's
  `rebuild_nets`, ~40 ms; names can merge nets) and a value change re-stamps every instance
  (`scene::build`, ~30 ms, symbol texts show values) and reruns the whole check pass. The
  upgrade is elab reporting which instances' values changed (the scene re-stamps those) and
  a rename patching the components it names.
- **First edit** (2.7 ms, tick 1.9 ms): first-use growth of the tables an edit patches (the
  plugin snapshot's columns, elab's tables) and the first incremental passes; it is once
  per document.
- **Amortized ceilings** (`ponytail:` in the code): the scene compacts with a full build
  once dead rows pass a quarter of its tables (~30 ms at 10^5, every tens of thousands of
  removed rows), the markers re-place everything at the same share, and a `PrimIndex` that
  followed 2^16 appended cell entries is rebuilt on the next tile (~7 ms per 10^5 rows).
- **The violation table** stays a sorted `Vec<Violation>`: a patch moves its tail (≈ 0.1 ms
  at 70k rows), the largest remaining O(n) step of an edit frame on this fixture.

## Rename and param edits 2026-10

Goal: the last two edit kinds, an instance rename and an instance parameter change through
the properties dialog, under 1 ms of CPU at 100k objects like the others. Measured on `main`
`1bb4b6a4` (load < 1) with `edit_frames_100k` and temporary `Instant` laps around each spine
step and inside `CellState::recompute`.

### Before (100k, ms per frame, 12 frames each)

| Step | rename | param |
|---|---|---|
| elab connectivity (`rebuild_nets`) | 34–46 | 0 |
| elab graph | 0 | 1.3 (an override added: `rebuild_graph`) or 0.02 (an existing one changed) |
| elab check pass (`check`, all rows) | 19.5–21.6 | 19.4–21.5 |
| `scene::build` (values changed: every instance re-stamped) | 0.03 (update) | 28.7–29.0 |
| violation markers (re-placed after a full scene build) | 0.01 | 6.1–7.3 |
| UI compose + layout | 0.28 | 0.27 |
| everything else | 0.15 | 0.15 |
| **frame p50** | **55.5** | **58.3** |

The tick after either is 0.3 ms. The fixture has 10k instances (no overrides), 60k wires,
20k labels; the dialog's parameter field adds an override the first time (`W` on a bare
`dev:nmos`), then changes it.

### Plan (from the table)

1. **Value checks per instance** (elab): a value edit reruns the check pass only on the
   instance rows whose values it changed (E008, gating, E039, W007, E030) and on the own
   scope's E008 rows, and patches those rows of the table in place; the pass delta is the
   replaced rows. Removes the 20 ms of `check` from param edits.
2. **Value delta** (elab → app): `graph.recompute` notes the nodes whose value or
   `unverified` flag actually changed; `CellState` reports their instance rows, and
   `scene::update` re-stamps those instances with the edit's own objects instead of a
   `scene::build`. Removes the 29 ms build and the 6 ms marker re-placement.
3. **Overrides added or removed without a graph rebuild** (elab): the node keeps its slot; its
   new program's dependencies replace the old ones when each has a lower topological rank
   (the order stays valid), else the graph rebuilds as today. Dependency lists move to the
   end of the edge arrays (start/end per node instead of CSR); the reverse edges of a new
   dependency move the same way. The affected-set walk uses epoch marks (no O(nodes) clear).
4. **Instance renames patch connectivity** (elab): a name only gives the instance its
   element names and the duplicate-name findings (E009) unless its element count changes
   (a bus `X[3:0]`); with the same count the element names are rewritten in place and only
   the E009 rows of the old and new names are recomputed (one scan of the element names),
   posted through the connectivity delta. A count change rebuilds as today. Label and port
   renames already patch (`live`): names join by name, merges and splits are local.
5. **Param on a shared var** is O(dependents): evaluation, checks and re-stamps of every
   dependent instance; measured separately, above 1/8 of the objects the scene rebuilds.

### What was done (each result equals its full rebuild; oracles named in the commits)

1. **Per-instance checks** (`CellState::recheck`): E001, W007, E030, E008, gating and E039
   rows are functions of one instance (`check::INST_CODES`); a value edit reruns them on the
   instances it touched or revalued and replaces their rows in place.
2. **Value delta** (`Graph::recompute` reports the nodes whose value or `unverified` flag
   changed; `CellState::revalued` their instance rows): `scene::update` re-stamps those
   instances with the edit's objects; only a graph rebuild builds the scene again.
3. **Overrides added or removed keep the graph** (`Graph::set_deps`): dependency lists are
   start/end ranges, a node's new list is written in place or appended when every new
   dependency has a lower topological rank, and each new dependency's dependent list moves to
   the end with the node; the affected-set walk uses epoch marks.
4. **Renames patch element names** (`Live::rename`): same element count → the names are
   rewritten and the E009 rows of the old and new names recomputed (one scan of the element
   names), posted through the connectivity delta. The patch also runs when a rejected
   transaction permuted geometry rows (`Live::rows_moved`), which a value-only or rename-only
   edit used to leave stale.

### After (2026-10-18, same machine, load < 1; CPU update+paint p50 / p95 ms)

| Edit | before | after | after tick (max) | 10k objects after |
|---|---|---|---|---|
| rename (half to a duplicate name) | 55.5 / 58.2 | 0.49 / 0.61 | 0.53 | 0.41 |
| param (an override added, then changed) | 58.3 / 61.2 | 0.43 / 0.44 | 0.18 | 0.39 |
| delete | 0.58 / 0.59 | 0.56 / 0.60 | 0.63 | 0.28 |
| undo | 0.52 / 0.54 | 0.50 / 0.51 | 0.59 | 0.23 |
| redo | 0.53 / 0.55 | 0.50 / 0.54 | 0.64 | 0.23 |
| move (drop) | 0.58 / 0.59 | 0.59 / 0.61 | 0.62 | 0.27 |
| wire | 0.41 / 0.45 | 0.39 / 0.43 | 0.52 | 0.22 |
| first edit of the document | 2.35 | 2.42 | 1.97 | 0.80 |

The rename and param frames are now: elab 0.05–0.1 ms, UI compose 0.2–0.3 ms, scene and markers
for one instance 0.05 ms, paint the rest. `frame_times_100k`, `interactions_100k` and the docs-page
var benchmark are unchanged (edit 0.68, hover 0.11, click 0.24, var with 1k dependents 2.0 ms).

A var that many instances read (`shared_var_edit_frames_100k`, 12 edits each, p50 / p95 ms):

| Dependents (of 10k instances) | zoomed in | fitted to the window |
|---|---|---|
| 1k (every tenth instance) | 7.3 / 20.6 | 19.7 / 32.5 |
| 10k (every instance) | 48.6 / 51.4 | 58.3 / 59.0 |

At 1k dependents the update is 2.2–3.0 ms (elab 0.2, scene re-stamps 1.1–1.7, markers 0.7–1.0)
and paint 4–5 ms zoomed in (the canvas diff and index following ~30k killed and appended rows),
16–17 ms fitted (every patched tile on screen re-records); the p95 is the scene's and the
markers' compaction (a full build, ~30 ms, once a quarter of the rows are dead: every few such
edits). At 10k dependents every edit re-stamps 10k instances (13–15 ms) or compacts (30 ms).

What bounds the rest:

- **Shared vars** are O(dependents) by nature; the constant was the whole-instance re-stamp
  (done 2026-10-24, next section).
- **Bus renames** (a new element count) and `inherit`/`ignore` edits rebuild connectivity
  (~40 ms at 100k): connections move per element.
- **A dependency on a later node** (an override reading a var that sorts after the instance in the
  topological order) rebuilds the graph (~1.3 ms at 100k), as do cycles; appended dependency lists
  past 4× the built edges rebuild once to compact.

## Shared-variable edits 2026-10

`shared_var_edit_frames_100k` (12 edits of a docs-page var, values 3…14), measured on
`refactor/gui-crates` `b02afcb1` with temporary `Instant` laps around elab, the scene, the
markers and the canvas diff and pack. This machine had no GPU adapter in the test's sandbox,
so the numbers are CPU update+paint only; load average 10–35 (other agents' builds).

### Before (ms per edit frame)

| Step | 1k dependents | 10k dependents |
|---|---|---|
| elab (`recompute`) | 0.2 | 1.2 |
| `scene::update` (remove + re-stamp every dependent: ~30 rows killed and appended each) | 1.3–1.8 | 13–16 |
| `scene::build` (a quarter of the rows dead: every ~8th edit at 1k, every 2nd at 10k) | 31 | 30 |
| markers (re-placed on every touched object; compacted with the scene) | 1.0 (6.5) | 6.2–6.8 |
| canvas diff of the killed and appended rows | 0.4 | 4–10 |
| canvas pack + texts (GPU row patch, index follow; a repack when free slots run out) | 0.8 or 7.5, alternating | 9 |
| **frame p50 / p95** | **11.0 / 11.6** | **47.7 / 49.0** |

### Fix

A revalued instance no write touched keeps its rows: `scene::update` re-renders its symbol
texts and writes each into its own text row in place when the row would keep its anchor, size,
turn and length (`revalue_texts`); only an instance whose text changes length is re-stamped.
The rewritten rows are reported through `Scene::edits` like killed rows (texts are drawn
from the table every frame, so the GPU rows and the spatial index have nothing to follow), no
row dies, so the scene never compacts in these frames, and `Scene::touched` lists only the
re-stamped objects, so the markers stay put (a changed mark still reaches them through the
patched marks' targets). Oracle: `incremental_scene_equals_full_build` (every step equals a
full build; it now also asserts that some revalued instances were rewritten in place, and
goes red when the rewrite is skipped).

### After (same session, load ~9)

| Step | 1k dependents | 10k dependents |
|---|---|---|
| elab | 0.2 | 1.1–1.2 |
| scene (template render per dependent, in place) | 0.5–0.6 | 4.5–5.0 |
| markers | 0.001 | 0.001 |
| canvas diff + pack + texts | 0.02–0.04 | 0.14 |
| **frame p50 / p95** | **0.94 / 1.12** (fitted), **0.89 / 0.94** (zoomed) | **6.5 / 46** (fitted), **6.7 / 47** (zoomed) |

The fixture's `nmos` symbol shows `W` only, so a `wl` edit (an `L`) rewrites no text: its
frame is the check and re-render of 1k dependents; a `wv` edit rewrites 10k `W=` texts. The
1k p50 is asserted against D39's 8 ms param-edit budget. At 10k the p50 is under it too
but not asserted (no margin under load); its p95 is the one edit whose value grows a digit
(9 → 10): every dependent is re-stamped (20 ms) and the scene then compacts (30 ms). The
upgrade for that frame is killing and appending only the text rows of an instance (its other
rows keep their stamp), and compacting on an idle tick.

## Commit frames hand O(document) work to idle steps (2026-10-24)

Three things a commit did in its own frame at 100k objects (HANDOFF latent bug 5), now done
in bounded `Project::idle_step` calls on ticks without input (`Editor::wake_ms` asks for them):

| Work | In the commit frame before (ms) | Commit frame after (ms) |
|---|---|---|
| sha256 of the file for a first journal's `base`, commit before the hash steps ended | 62 | 0.09 (placeholder `base`, journal re-sent once hashed) |
| Compaction once garbage ≥ live data (`to_format` + `from_format`) | 113 | worst commit of 1000 edits 17 (O(change)); the 113 ms run in one idle tick |
| A new document's first journal: `hist` of its history (100k-wire paste) | 162 | 0.99 |

`cargo test --release -p schemify-doc --test perf -- --ignored --nocapture commit_frames` (one
run, shared host). A caller that never idles (the CLI) still compacts inside a commit, at
garbage ≥ 3× live data. The compaction stays one O(document) idle tick, and its change-log break
makes the next frame rebuild the scene: both off the interactive path.

## Frame pacing 2026-10

Step 0 of [research/render-backend.md](research/render-backend.md) § 5 (migration plan) and
recommendation 1 of [research/xschem-responsiveness.md](research/xschem-responsiveness.md):
draw from the newest input. Two commits in `crates/view/render/src/window.rs`: the frame log
(`render: frame log stamps the newest input and counts updates per frame`) and the loop
(`render: draw from the newest input: update runs at the granted redraw`).

### The loop's order

Before: `about_to_wait` answered every event batch at once, `Editor::update` then
`request_redraw`; winit held the `RedrawRequested` until the compositor's frame callback for
the previous present arrived; `render` then painted whatever the editor held. After: an input
event only asks for a redraw (`window::pace`: input → `Redraw`); `Editor::update` runs at the
top of `render`, when the redraw is granted, once over every event queued until then; a tick (a
`wake_ms` deadline, a `Waker` post) still updates at once in `about_to_wait` (`Update`), since a
hidden window is granted no redraw and the run it polls must go on; nothing pending sleeps
(`Sleep`, idle CPU 0% unchanged; `pacing_rule` in `window.rs` is the headless oracle).
`desired_maximum_frame_latency` stays 1 (measured below).

What the measurement found the old order doing, and the new one fixes:

- An input that arrived in the same dispatch as the frame callback was painted one frame late:
  the present ran before its update (110 of 946 frames, 12%, at ~6 ms keys; 13 of 817 at ~16 ms
  keys). The late frame costs one callback period: 23–32 ms on the research's 59.95 Hz output
  with callbacks at 31–44 Hz, 4.2 ms at 240 Hz. The first frame log charged such an input to
  the frame it missed, which is why the log now stamps consumed input only.
- One update ran per event. Real pointer motion over the window gave 3 updates per frame
  (p50) and 8 at most on the old build at 240 Hz; the new order runs one (its p95 of 2 is a
  tick's update with queued input riding along).

What it does not change: a frame already showed the newest input whose update had run before
the grant. The research's 20–25 ms p50 was the *oldest* input's wait, the coalescing span at
8 ms inputs, not the shown input's lag.

### Method

`SCHEMIFY_FRAME_LOG=1 WAYLAND_DEBUG=1 WGPU_ADAPTER_NAME=NVIDIA nice -n 19 schemify-gui --clean
edit big10k.shcomp` (the perf schematic at 10k objects, `perf.rs` `schematic(10_000)`),
1280×800, RTX 4060, Vulkan, mailbox, release build. 1600 arrow keys in blocks of 20 Left and 20
Right (`view.pan_left`/`right`) typed through the compositor by `wtype -k Left -s 1 …` (the
virtual-keyboard protocol; ~6 ms apart, wtype's floor), about 10 s a run. Each `frame:` line
gives oldest- and newest-input-to-present, the updates that consumed input since the last
present and the frame period; `WAYLAND_DEBUG` names the `wl_output` the surface entered and
its mode. Scripts: the session scratchpad's `pace-run.sh` and `pace-stats.py`. The machine was
in use (load 2–5) and the monitors changed during the session: DP-4 from 59.95 Hz to 165 Hz,
eDP-1 at 240 Hz; every run with a named output landed on eDP-1 at 240 Hz. The research's
20–53 ms figures were taken at 59.95 Hz with callbacks at 31–44 Hz, synthetic 8 ms events
and 100k objects, so they are not directly comparable with the rows below.

### Numbers (ms, p50 / p95 / p99 / max; updates = updates with input per presented frame)

| Run | Output | Frames | Oldest → present | Newest → present | Updates p50 / p95 / max | Period p50 |
|---|---|---|---|---|---|---|
| before, keys at ~16 ms | not logged (43.6 frames/s for 64 keys/s: the 59.95 Hz output) | 817 | 7.12 / 17.46 / 19.91 / 24.20 | 5.22 / 15.79 / 18.69 / 23.47 | 1 / 2 / 2 | — |
| before | not logged | 994 | 4.46 / 9.36 / 13.41 / 21.48 | 4.30 / 7.82 / 11.75 / 21.48 | 1 / 2 / 3 | 4.64 |
| before, 2nd | eDP-1 240 Hz | 980 | 6.34 / 11.10 / 13.88 / 33.43 | 5.39 / 9.87 / 12.27 / 33.43 | 1 / 2 / 7 | 8.33 |
| after, latency 1 | not logged | 983 | 5.55 / 11.31 / 14.33 / 27.10 | 4.82 / 9.85 / 11.84 / 27.10 | 1 / 2 / 2 | 6.81 |
| after, latency 1, 2nd | eDP-1 240 Hz | 459 (one 0.5 s stall) | 7.47 / 10.48 / 13.24 / 30.01 | 4.45 / 8.44 / 9.89 / 30.01 | 1 / 2 / 2 | 8.37 |
| after, latency 2 | eDP-1 240 Hz | 1241 | 6.18 / 11.74 / 15.59 / 54.34 | 5.00 / 9.65 / 12.28 / 44.41 | 1 / 2 / 2 | 7.65 |

Reading:

- At 240 Hz, before and after are within run-to-run noise: the newest input's p50 is 4.3–5.4 ms
  before and 4.5–5.0 after, its p95 7.8–9.9 against 8.4–9.9. The callback period is 4.2 ms, so
  the frame-late input costs about 0.5 ms on average here; the gain is in the 60 Hz case the
  research measured (one 23–32 ms period for 12% of frames) and in CPU (one update per frame).
- Frame latency 1 against 2: latency 2 raised the p50 (4.45 → 5.00) and the p95 (8.44 → 9.65),
  within noise, and lowered nothing. In the new order the acquire follows the callback, when
  the compositor has released an image, so a spare image buys nothing. 1 stays.
- The refresh rate alone: the research's oldest-input p50 20–25 / p95 46–53 ms at 59.95 Hz
  against 4.5–6.3 / 9.4–11.1 at 240 Hz here (different input and scene, same build order): the
  callback period went from 23–32 ms to 4.2 ms. The first row, typed before the monitors
  changed, shows the 59.95 Hz output at 16 ms keys: oldest 7.1 / 17.5, newest 5.2 / 15.8.

## GPU canvas pipelines 2026-10

Step 1 of [research/render-backend.md](research/render-backend.md) § 5: on the GPU the canvas is
drawn by Schemify's own instanced wgpu pipelines (`render::gpu`) instead of vello tiles. Each
primitive is one retained instance row, grouped by palette layer, patched from the tables'
reported changes and drawn under a camera uniform: a pan or zoom writes 96 bytes. Canvas text
is still drawn by vello over the canvas pass (the glyph atlas is step 3). Measured with
`frame_times_100k` and `interactions_100k`, one run each, `nice -n 19`,
`SCHEMIFY_GPU=NVIDIA` (**NVIDIA GeForce RTX 4060 Laptop GPU**, Vulkan, timestamp floor
0.013 ms). Before: `refactor/gui-crates` at `4a61dc49` (load ≈ 8). After: load ≈ 46 (other
agents building), so the tails (p99) are noisy.

### 100k objects (260k primitives), p50 / p95 / p99 ms

| Frame | CPU update+paint before | after | GPU before (tiles) | GPU after (pipelines) |
|---|---|---|---|---|
| pan | 0.190 / 0.287 / 0.469 | 0.092 / 0.159 / 0.218 | 0.089 / 0.111 / 0.409 | 0.625 / 0.916 / 0.918 |
| zoom (+ settle) | 0.458 / 0.885 / 1.051 (210 frames) | 0.116 / 0.144 / 0.150 (60 frames: no settle frames) | 0.451 / 0.643 / 0.665 | 0.543 / 0.657 / 0.658 |
| select | 1.179 / 1.606 / 1.758 | 0.391 / 0.575 / 0.796 | 0.515 / 0.596 / 0.620 | 0.659 / 0.660 / 0.814 |
| edit (Del, undo) | 2.684 / 4.841 / 5.099 | 0.957 / 3.882 / 64.5 (one frame under load) | 2.259 / 3.916 / 4.116 | 1.132 / 1.135 / 1.137 |
| hover | 0.697 / 1.006 / 1.429 | 0.225 / 0.377 / 0.413 | 0.252 / 0.548 / 1.116 | 0.480 / 0.487 / 0.492 |
| click select | 1.268 / 1.414 / 1.692 | 0.397 / 0.537 / 0.542 | 0.367 / 0.516 / 5.687 | 0.325 / 0.328 / 0.478 |
| drag | 0.440 / 0.612 / 0.766 | 0.213 / 0.277 / 0.295 | 0.583 / 0.685 / 0.691 | 0.799 / 0.802 / 0.802 |
| type | 1.057 / 1.252 / 1.412 | 0.250 / 0.371 / 0.374 | 0.373 / 0.515 / 0.819 | 0.647 / 0.651 / 0.652 |
| menu | 0.952 / 1.596 | 0.230 / 0.502 | 0.461 / 1.504 | 0.659 / 0.821 |
| unselect (select-all box) | 26.4 / 34.4 / 36.0 | 6.2 / 8.9 / 9.2 | 10.0 / 15.2 / 15.3 | 1.09 / 1.10 / 1.10 |

The "render calls" column (CPU side of the submission) went from 0.43 / 1.0 / 1.8 ms p50 (pan,
zoom, hover) to 0.13 / 0.15 / 0.35 ms. Before, `frame_times_100k` failed its zoom p99 budget
(1.05 ms) and `interactions_100k` its hover p95 (1.006 ms) on this run; after, both pass.

- **GPU time per frame is now flat**: 0.54–0.66 ms p50 for every camera frame, the whole canvas
  drawn every frame (no tile to copy, no stand-in, no partial frame). The research spike's
  target was 0.30–0.40 ms at 100k on this GPU; this misses it by about 0.25 ms. The difference:
  the spike drew no grid and no UI blend, and its strokes had no exact-area coverage; here each
  frame also blends the 1280×800 UI texture over the canvas and runs subsampled coverage at
  stroke caps (`gpu.wgsl`). A tile-copy pan was cheaper on the GPU (0.09 ms) but left blank
  tiles and in-frame raster spikes (p99 0.41, 5.7 ms on a click).
- **Bytes written per edit**: 4 KiB over 24 edit frames, about 170 B a frame (the killed and
  appended rows' slots). The first frame uploads the whole canvas: 7.0 MiB at 100k. A
  select-all repacks the marks source (24 B per marked row: 6 MB at 100k): 135 MB over the 12
  select-all rounds.
- **Canvas text** (labels, pin names, values) still goes through vello: a table whose largest
  label is under 3 px at the current zoom is skipped whole, otherwise the rows near the view
  are listed through `PrimIndex`. Scanning every label row instead cost 0.35–0.8 ms of CPU per
  frame at 100k (80k label rows, all under 3 px at the fit view), measured with temporary laps;
  with the skip and the index the pan frame is the 0.09 ms above.
- `gpu.rs` `gpu_100k_canvas_matches_cpu_and_reports_frame_time` (render crate, 1600×900, the
  `hundred_k` fixture at fit, submit to completion): 1.42 ms on the RTX 4060 and 11.1 ms on the
  Intel iGPU for the canvas pass, against vello's 4.10 and 26.2 ms for the same scene encoded
  per frame; the 100k pack takes 4.8–6.7 ms (2.9 MB of rows).

### GPU against CPU (equivalence, `crates/view/render/tests/gpu.rs`)

Mean |Δ| per channel / pixels off by more than 96 levels, identical on the RTX 4060 and the
Intel iGPU (tolerance: mean ≤ 1.0, at most 0.1% of pixels off):

| Scene | mean | far |
|---|---|---|
| lines (plain, weighted, dashed) | 0.037 | 0 |
| rects (outlined, filled, styled, dashed) | 0.024 | 0 |
| arcs (circles, arcs both ways, filled, dashed) | 0.321 | 0 |
| polylines and polygons (filled, concave) | 0.059 | 0 |
| marks over the thousand | 0.663 | 37 |
| far-zoom cut at the 2 px threshold | 0.003 | 0 |
| the thousand under a UI frame (texts, popup) | 0.974 | 128 |
| 100k at fit (own bound: mean ≤ 4.0) | 3.24 | 254 |
| negative space: one rect moved 60 px | 0.579 | 1898: fails |

What the remaining differences are:
- Coverage: exact band area along a stroke and 16 subsamples at its caps (`gpu.wgsl`), against
  vello_cpu's exact area of the flattened outline.
- The CPU strokes a layer as one path, so overlapping strokes cover a pixel once; the GPU
  blends instance by instance. At the 100k fit, where nearly every pixel is an overlapping
  1 px edge, that alone is a mean of 1.17 (the CPU against itself drawing instance by
  instance).
- Text goes through vello on the GPU against vello_cpu on the CPU.
- One CPU artifact, fixed since: it filled a layer's closed shapes as one non-zero path, so
  a filled rect and polygon of opposite winding that overlap cancelled (14 px in the
  thousand). Each closed shape is filled on its own now, as the GPU fills each instance.

### Step 2: the tile cache deleted (2026-10-24)

`tiles.rs`, `CanvasCache`, `draw_cached` and its LOD masks are gone; `CpuPainter` draws the
canvas from the same `InstanceRows` the GPU does (`canvas::draw_rows`). The GPU path's frame
work did not change, so the numbers hold. One run each of `frame_times_100k` and
`interactions_100k`, release, `nice -n 19`, `SCHEMIFY_GPU=NVIDIA` (**NVIDIA GeForce RTX 4060
Laptop GPU**, Vulkan, timestamp floor 0.013 ms), before (`1ed77d5e`) and after back to back at
load ≈ 8–18. CPU update+paint, then GPU, p50 / p95 / p99 ms:

| Frame | CPU before | CPU after | GPU before | GPU after |
|---|---|---|---|---|
| pan | 0.088 / 0.145 / 0.157 | 0.076 / 0.111 / 0.123 | 0.664 / 0.915 / 0.916 | 0.630 / 0.915 / 0.918 |
| zoom | 0.086 / 0.114 / 0.119 | 0.074 / 0.100 / 0.101 | 0.547 / 0.660 / 0.662 | 0.552 / 0.660 / 0.674 |
| select | 0.462 / 0.667 / 0.767 | 0.508 / 0.582 / 0.595 | 0.663 / 0.663 / 0.818 | 0.665 / 0.683 / 0.843 |
| edit (Del, undo) | 0.924 / 2.000 / 4.291 | 0.903 / 2.144 / 2.653 | 1.138 / 2.652 / 2.813 | 1.139 / 1.142 / 2.653 |
| hover | 0.189 / 0.250 / 0.292 | 0.222 / 0.248 / 0.295 | 0.481 / 0.486 / 0.488 | 0.481 / 0.486 / 0.489 |
| click select | 0.391 / 0.429 / 0.775 | 0.366 / 0.491 / 0.734 | 0.326 / 0.328 / 0.486 | 0.326 / 0.327 / 0.485 |
| drag | 0.191 / 0.219 / 0.229 | 0.245 / 0.314 / 0.340 | 0.804 / 0.809 / 0.816 | 0.809 / 0.813 / 0.814 |
| drop | 3.02 | 3.69 | 0.80 | 0.80 |
| menu | 0.190 / 0.505 | 0.250 / 0.730 | 0.666 / 0.830 | 0.665 / 0.823 |
| type | 0.241 / 0.302 / 0.317 | 0.281 / 0.375 / 0.378 | 0.651 / 0.656 / 0.656 | 0.652 / 0.656 / 0.658 |
| unselect (select-all box) | 6.23 / 6.31 / 6.39 | 6.26 / 9.22 / 9.44 | 1.10 / 1.22 / 2.77 | 1.10 / 1.10 / 1.22 |

- Every interactive budget (D39, < 1 ms CPU) passes before and after; the differences are
  within this machine's run-to-run spread at that load (one run each, as asked). The drag
  frames draw the move preview through `draw_prims`, which now packs the overlay into scratch
  rows before drawing them (a full pack of a small table per frame); that, or the noise, is
  the 0.05 ms.
- **Rows written by a pan or zoom: zero.** The zoom report uploads 0 KiB; the pan report's
  7 003 KiB is the first frame's whole canvas, which falls in it. Headless oracle:
  `a_pan_or_zoom_frame_writes_no_instance_bytes` (app `tests/canvas.rs`: the frames' dirty row
  bytes are 0 while the camera moves; a delete writes rows).
- Resident memory over the select-all rounds is flat in both (389 MiB before, 432 after, ±0
  across rounds); the 43 MiB offset was not investigated beyond that (the GPU path allocates
  no new per-frame table).
- Equivalence means are unchanged on the RTX 4060 (both sides read the same rows now, and the
  CPU's row path draws byte-identical pixels to the old direct path): lines 0.037, rects 0.024,
  arcs 0.321, polys 0.059, marks 0.664, far-zoom 0.003, mixed scene under the UI 0.974, 100k
  at fit 3.244, the moved-rect negative space 0.579 (fails, as it must).
- **Goldens**: nine app goldens (`editor_*`, `docs_*`, `primitive_modern`,
  `cell_panel_modern`, `core_symbols_*`) differ from before by ±1 level on 4–18 pixels each,
  because the CPU tile intermediate is gone (each tile was its own pixmap, its paths placed
  relative to the tile origin and clipped to it). They pass `check_golden` and are **not
  re-blessed**, on purpose. Already stale at `1ed77d5e`, within tolerance and also not
  re-blessed: `cell_panel_modern` (657 px), `core_symbols_*` (355 px), `menu_file` (256 px),
  render `canvas_1k` (3 px).

### Step 3: the glyph atlas and the UI on the GPU (2026-10-24)

The UI frame, the texts and the canvas are one render pass straight into the surface: the
`GpuPainter`'s batch (`render::batch`: rects, stroke segments, images and atlas masks, each
with its clip) with the canvas stages among its instances. Glyphs, icons, check marks, arrows
and math delimiters are coverage masks rasterized once by vello_cpu into a 2048² R8 atlas
(`render::atlas`). vello's GPU renderer draws nothing; the status bar's live layer, the
`Layers` compositor, the vello target and its blit are gone. One run each of
`frame_times_100k` and `interactions_100k`, release, `nice -n 19`, `SCHEMIFY_GPU=4060`
(**NVIDIA GeForce RTX 4060 Laptop GPU**, Vulkan, timestamp floor 0.013 ms). Before:
`b02afcb1` at load ≈ 15–21; after: load ≈ 5–9. "render" is the CPU side of the render calls
(before: the layer raster and vello's encode and submit, or the composition; after: the uploads
and the one pass). p50 / p95 / p99 ms:

| Frame | CPU before | CPU after | render before | render after | GPU before | GPU after |
|---|---|---|---|---|---|---|
| pan | 0.089 / 0.150 / 0.162 | 0.088 / 0.138 / 0.144 | 0.133 / 0.205 / 0.238 | 0.070 / 0.120 / 0.162 | 0.624 / 0.915 / 0.920 | 0.631 / 0.931 / 0.933 |
| zoom | 0.091 / 0.150 / 0.157 | 0.096 / 0.159 / 0.165 | 0.135 / 0.201 / 0.206 | 0.077 / 0.130 / 0.131 | 0.548 / 0.660 / 0.662 | 0.548 / 0.668 / 0.669 |
| select | 0.340 / 0.405 / 0.615 | 0.466 / 0.630 / 0.644 | 0.091 / 0.111 / 0.381 | 0.092 / 0.126 / 0.162 | 0.662 / 0.664 / 0.820 | 0.668 / 0.669 / 0.670 |
| edit (Del, undo) | 0.909 / 2.818 / 3.693 | 0.836 / 2.249 / 4.265 | 0.508 / 1.014 / 1.053 | 0.068 / 0.107 / 0.111 | 1.136 / 1.147 / 2.658 | 0.668 / 0.669 / 0.670 |
| hover | 0.259 / 0.319 / 0.368 | 0.216 / 0.264 / 0.287 | 0.533 / 0.655 / 1.496 | 0.070 / 0.085 / 0.117 | 0.480 / 0.643 / 0.645 | 0.329 / 0.332 / 0.334 |
| click select | 0.487 / 0.646 / 0.759 | 0.368 / 0.493 / 0.515 | 0.164 / 0.175 / 0.634 | 0.082 / 0.090 / 0.104 | 0.325 / 0.327 / 0.482 | 0.330 / 0.331 / 0.332 |
| drag | 0.297 / 0.321 / 0.325 | 0.237 / 0.326 / 0.346 | 0.977 / 1.074 / 1.438 | 0.077 / 0.115 / 0.118 | 0.803 / 0.809 / 0.811 | 0.330 / 0.333 / 0.333 |
| drop | 12.3 | 4.00 | 1.20 | 0.15 | 0.81 | 0.33 |
| menu | 0.240 / 0.821 | 0.197 / 0.737 | 0.470 / 1.287 | 0.066 / 0.136 | 0.663 / 0.828 | 0.336 / 0.370 |
| type | 0.364 / 0.402 / 0.416 | 0.235 / 0.294 / 0.305 | 0.626 / 0.730 / 0.739 | 0.058 / 0.076 / 0.077 | 0.650 / 0.655 / 0.655 | 0.331 / 0.333 / 0.334 |
| unselect (select-all box) | 6.18 / 6.35 / 6.63 | 6.29 / 8.98 / 9.47 | 0.565 / 0.608 / 0.613 | 0.067 / 0.094 / 0.102 | 1.100 / 1.104 / 1.219 | 0.629 / 0.629 / 0.629 |

- **No frame calls vello.** Before, hover, select, pan and zoom were composed without a vello
  render (the live layer), but the status bar's layer was still rasterized by vello on every
  hover, and drag, menu, type and edit frames rendered the whole UI with vello (render 0.5–1.0
  ms, GPU 0.65–0.80 ms). After, every frame is the one pass: render calls 0.06–0.09 ms p50,
  GPU 0.33 ms for the zoomed-in interaction frames and 0.55–0.67 ms at the 100k fit (the
  canvas pass dominates there, as in step 1).
- **Bytes per frame**: the batch's instances, 56 B each, 13–14 KiB a frame (≈ 250 instances;
  39 KiB with the Tools menu open). Atlas bytes are zero on almost every frame: p50 0, p95
  0.1–0.2 KiB on hover, drag and type (a status-bar or field glyph seen the first time), 2.4
  KiB on the edit and drop frames, 4.4 KiB on the first menu open (its glyphs). Rows as before:
  0 on pan and zoom, 4 KiB over 24 edits.
- **CPU per frame** is unchanged or lower; the select row's +0.13 ms is within this machine's
  spread (its interactions twin, click select, went down 0.12 ms), as is unselect's tail
  (`Editor::update`'s box select, 6–13 ms by the note at `interactions`).
- **Zoom over labels**: every zoom frame gives every visible label a new size, so a new atlas
  key per glyph. Rasterizing them all cost 1.0–1.6 ms a frame with 200 labels in view (100–180
  masks, `render/tests/perf.rs` `canvas_label_zoom_frames`). A frame now rasterizes at most 32
  exact masks (`EXACT_PER_FRAME`) and draws the rest from the same glyph's mask at its last
  size, scaled; the window asks for frames without input until the picture is exact
  (`Batch::settled`). Measured: 0.36–0.46 ms per zoom frame, then 3–4 frames of 0.07–0.37 ms
  after the wheel stops, then 0.03 ms (no mask rasterized: idle stays at zero).
- `render/tests/perf.rs` `scene_build_100k_primitives` (now the GPU painter's frame build:
  UI shell, 100k rows unchanged, labels): 0.03 ms median, against vello's scene encode before.

GPU against CPU for the batch (`render/tests/gpu.rs`, RTX 4060; tolerance mean ≤ 0.5/255 and
no pixel off by more than 96 for text and UI; the canvas kinds keep their step-1 bounds):

| Scene | mean | far |
|---|---|---|
| canvas labels at zoom 1 / 2.3 / 5.1 / 0.6 | 0.051 / 0.020 / 0.006 / 0.022 | 0 |
| UI text, device scale 1 / 1.5 | 0.343 / 0.268 | 0 |
| glyphs at whole and quarter pixels (bound: every pixel within 3 levels) | 0.021 | 0 |
| prose (headings, code, rule, list, inline and display math, an image) and a formula | 0.120 | 0 |
| default UI shell with a menu / with a dialog | 0.209 / 0.066 | 0 |
| the thousand under a UI frame, labels over their table, overlay, popup (≤ 1.0) | 0.680 | 29 |
| the whole editor frame, opened / hovered / Tools menu (app `perf.rs`) | 0.048 / 0.048 / 0.096 | 0 |
| negative space: a wrong glyph | 0.664 | 20: fails |
| negative space: a run shifted 1 px | — | > 0: fails |

The remaining text difference is the subpixel phase: a mask is vello_cpu's own raster of the
glyph with its origin moved to the nearest 1/8 px (hinted glyphs keep vello_cpu's whole-pixel
baseline), so glyphs drawn at exact eighths match within blending's rounding. At 1/4 px dense UI
text measured 0.72. Amplified (×8) diffs are written by the tests to
`target/tmp/{ui_text,ui_shell,prose_math,canvas_labels,editor_frame}.diff.png`; they show
faint glyph edges only.

**Goldens**: unchanged. The CPU painter's code paths are untouched (its `overlay` is the old
inline `draw_prims` under the clip, the status bar is drawn directly as a painter without
layers always drew it); every render and app golden passes `check_golden` and none was
re-blessed.

## Binary size

D39 (amended 2026-10-11): the headless core `schemify` ≤ 2 MB, the editor `schemify-gui` ≤
10 MB, both as `flake.nix` ships them: stripped, fat LTO, one codegen unit, `panic = "abort"`,
no debug info, and on Linux `-C force-unwind-tables=no -C link-arg=-Wl,-z,pack-relative-relocs`.
Report: `tools/size-check.sh` (builds both for the host with that env and prints their sizes; sizes are informational, D39 amended 2026-10-12).

Measured 2026-10-11 on x86_64-linux, rustc 1.95, bytes of the shipped (stripped) binary.
"Before" is `main` at 5da0192: one `schemify` binary with the editor built in; its core is the
headless build (`--no-default-features`).

### Totals

| | Core `schemify` | Editor `schemify-gui` |
|---|---:|---:|
| Before (5da0192) | 3,330,976 | 22,869,880 |
| After | 2,565,216 | 16,010,696 |
| Change | −766 K (−23 %) | −6,859 K (−30 %) |
| Rough target (informational, D39 amended 2026-10-12) | ≈ 2 MB | ≈ 10 MB |

### Per cut

Measured by building with and without the cut where marked "built"; otherwise attributed from
an unstripped build's symbol sizes (`nm --size-sort`, symbols grouped by the crate they name),
marked ≈. Attributions overlap a little (LTO inlines across crates), so rows do not add up
exactly to the totals.

| Cut | Core | Editor | How |
|---|---:|---:|---|
| Split (D39): the core links no GPU, window, text-shaping, accessibility, LaTeX or plugin-host crate | (the editor half is gone) | — | `crates/model/cli/tests/deps.rs` checks the resolved graph for every target |
| `rustfft` → own radix-2 + Bluestein FFT (`expr/src/fft.rs`, libm twiddles) | ≈ −661 K | ≈ −661 K | rustfft, transpose, strength_reduce, primal_check symbols |
| Full Noto Sans → `NotoSans-UI.ttf` subset; KaTeX Main (already embedded) draws → ≤ ≥ | 0 | −1,428 K | file sizes 1,581,884 → 153,536 |
| System-font fallback for glyphs outside the subset (fontique `system`, dlopen'ed fontconfig) | 0 | ≈ +65 K | fontique, fontconfig-sys symbols |
| winit without `wayland-csd-adwaita` | 0 | ≈ −309 K | tiny-skia, ttf-parser, ab_glyph, sctk-adwaita symbols |
| Lua 5.4 user config + package manager (phase 2: mlua, vendored Lua, `config`, `pack`) | ≈ +350 K | ≈ +330 K | core built before/after; probe: mlua +434 K at opt 3 |
| mlua + C Lua → omnilua (pure Rust, every target; 2026-10-12): core 2,565,216 → 3,589,264 | +1,024 K | — | core built before/after; omnilua's `lua_*` crates ≈ 830 K of symbols at opt 3 |
| omnilua's crates at `opt-level = "z"` (cold: startup, keymap/hook calls) | −369 K (3,589,264 → 3,220,624) | — | core built before/after |
| `[profile.release.package]`: cold crates at `opt-level = "z"`, GPU setup at `"s"` | −352 K | −2,945 K | built with and without the table |
| Linux: no unwind tables, packed relative relocations (DT_RELR) | −131 K | −1,364 K | built with and without the flags |
| Shared sorts (2026-10-12): elab/netlist name sorts through `Names::sort_by_text`, run's plan tables through one `(name, position)` sort, connect's `by_name` not re-sorted | −135 K (3,220,624 → 3,085,456) | not measured | core built before/after (main 953362e); `core::slice::sort` symbols 461 K → 322 K |

Not cut, measured: the GLES backend (glow, khronos-egl, wgpu-hal `gles`, naga GLSL out) is
≈ 176 K; it is the only GPU path on Linux machines without Vulkan, so it stays. `cargo tree -d`
finds no duplicate in the core; the editor has small ones from third-party version skew
(rustix 0.38/1.1 via winit, hashbrown 0.16/0.17, miniz_oxide 0.8/0.9, phf 0.11/0.14,
thiserror 1/2, bit-set/bit-vec), each tens of K, fixed only by upgrading those dependents.

Trade-offs taken: on GNOME (no server-side decorations) the window frame is smithay's plain
fallback (move, resize, close, maximize, minimize; no title text, no Adwaita styling); KDE,
wlroots compositors and X11 decorate as before. Panics in shipped Linux binaries print their
message but no backtrace (the binaries are stripped, so backtraces had no symbols anyway).
Shipped Linux binaries need glibc ≥ 2.36 (nix's glibc).

### Frame time (the `opt-level` table)

`cargo test --release -p schemify-app --test perf -- --ignored --nocapture`, three runs each,
same tree with and without `[profile.release.package]` (release profile, `debug = 0`; the
machine was shared with other builds, so ±7 % is noise):

| Metric | Without table (median of 3) | With table (median of 3) |
|---|---:|---:|
| 100k objects: open + first build | 191 ms | 186 ms |
| pan/zoom frame (update + scene encode) | 10.84 ms | 10.89 ms |
| edit round trip | 113 ms | 115 ms |
| update | 0.28 ms | 0.29 ms |
| var edit, 1000 dependents | 1.90 ms | 1.92 ms |
| var edit, gmid-backed | 2.03 ms | 2.05 ms |

No regression beyond noise: the table touches no crate on these paths.

### Where the editor's bytes are now (≈, symbol sizes, K)

| Group | K |
|---|---:|
| Our crates (with the generics they instantiate) | 2,825 |
| GPU: wgpu, naga, vello | 2,266 |
| Text: parley, harfrust, skrifa, read-fonts, fontique, ICU | 1,618 |
| Accessibility: accesskit, zbus/zvariant, async runtime | 1,141 |
| LaTeX: ratex + regex, fancy-regex, regex-lite, aho-corasick | 927 |
| Windowing: winit, wayland, x11 | 913 |
| Plugins: wasmi, wasmparser | 803 |
| std (generic + runtime) | 854 |
| Lua (mlua + C) | ≈ 250 |
| Other third-party | ≈ 400 |

Plus ≈ 3.0 M of `.rodata` (fonts 0.65 M, vello's WGSL shader sources with comments 0.45 M,
ratex's mhchem JSON 0.18 M, string literals), 0.6 M of std's own unwind tables and 0.6 M of
`.data.rel.ro`.

### What is left above the rough targets, and why

Under the constraints of this change (AccessKit kept, hot crates at `opt-level = 3`, no forks
or patches of dependencies), neither budget is reachable. The remaining levers, largest first,
with owners:

| Lever | Est. saving | Owner / cost |
|---|---:|---|
| Hot crates at `opt-level = "s"` (ours, text, vello, wasmi) | editor ≈ −2 M, core ≈ −0.4 M (all-`s` core measured 2,401,304 before the flags) | frame time to be measured; user decision |
| One shared sort instantiation: `elab` alone instantiates 312 K of `slice::sort` code (113 instances), `format` 77 K, `netlist` 55 K, `run` 43 K, `doc` 25 K | both ≈ −0.4 M | elab, format, netlist, run, doc owners |
| ratex: put mhchem (`\ce`, three regex engines + serde_json + 175 K of JSON) behind a feature | editor ≈ −0.8 M | upstream ratex-parser |
| vello: ship minified WGSL | editor ≈ −0.2 M | upstream vello_shaders |
| Drop the GLES backend | editor −0.18 M | loses non-Vulkan Linux GPUs |

Even all of them leave the editor near 12 M: the 10 M budget needs a feature-level decision
(e.g. the D-Bus accessibility client or the LaTeX widget as plugins) or an amended D39.
