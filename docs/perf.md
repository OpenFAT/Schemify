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
| …on a given GPU | prefix `SCHEMIFY_GPU=<adapter name substring>` (default: the first non-CPU adapter) |
| Param edit with 1k dependents | `… --test perf -- --ignored --nocapture var_edit` |
| Flat 1M-instance netlist (elaborate + emit) | `cargo test --release -p schemify-netlist --test perf -- --ignored --nocapture --test-threads=1` |
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

Before = `main` at `aec8c6a`; after = this branch. p50 / p95 / p99 in ms.

### 100k objects (260k primitives)

| Frame | CPU before | CPU after | GPU after, RTX 4060 | GPU after, Intel iGPU |
|---|---|---|---|---|
| pan | 10.9 (every frame re-encoded) | 0.13 / 0.22 / 22 | 0.076 / 0.092 / 3.7 | 0.37–1.1 / 0.39–2.3 / 47–51 |
| zoom | ~11 | 0.07 / 12.2 / 15.5 | 0.082 / 3.1 / 4.6 | 0.40–1.1 / 47–50 / 47–55 |
| select | not measured (≈ a full frame) | 3.4 / 3.9 / 4.2 | 0.47 / 0.50 / 1.1 | 1.1–2.3 / 1.1–3.4 / 3.2–3.4 |
| edit | 112 | 49 / 53 / 68 | 1.05 / 1.3 / 2.6 | 3.3–3.8 / 3.9–8.0 / 3.9–8.1 |

GPU before: 1.9 ms (RTX) and 26–37 ms (Intel) per frame, every frame (the whole scene through
vello). Open + first build: 178 ms before, 162–172 ms after.

The Intel ranges are two runs: its timestamp floor moves from 0.02 to 0.11 ms with the GPU's
clock state, and so does everything above it.

### 10k and 1M objects (after, RTX 4060)

| Frame | 10k CPU | 10k GPU | 1M CPU | 1M GPU |
|---|---|---|---|---|
| pan | 0.14 / 0.24 / 3.1 | 0.069 / 0.080 / 0.73 | 0.11 / 0.20 / 103 | 0.065 / 0.081 / 39 |
| zoom | 0.09 / 3.2 / 3.4 | 0.072 / 2.4 / 3.2 | 0.06 / 104 / 110 | 0.080 / 39 / 40 |
| select | 0.48 / 0.76 / 0.79 | 0.26 / 0.40 / 0.81 | 16.6 / 16.8 / 16.8 | 1.6 / 1.7 / 2.0 |
| edit | 4.6 / 8.4 / 12.9 | 0.81 / 1.0 / 2.6 | 531 / 602 / 720 | 9.8 / 19 / 19 |

Open + first build: 17 ms (10k), 1.95 s (1M).

### Netlist, flat 1M instances (elaborate + emit, document open)

| | before | after |
|---|---|---|
| elaborate | 1.28 s | 0.70–0.79 s |
| emit | 0.15 s | 0.14 s |
| total (budget 1 s) | 1.43 s | 0.85–0.93 s |

### Other D39 rows

| Budget | Measured |
|---|---|
| param edit → recompute + redraw ≤ 8 ms, 1k dependents | 1.8 ms (plugin-backed functions 3.4 ms) |
| idle CPU 0% | no wake-up once the zoom settles (asserted by the frame benchmark) |

## What makes a frame cheap

- **Raster layer** (`canvas::draw_cached`): the canvas (background, grid, primitives, decor,
  selection marks) is recorded once into a layer with a 25% margin; a whole-pixel pan copies
  it, a zoom stretches it until 120 ms after the last notch, then records crisp.
- **Vello-free frames** (`render::Layers`): when nothing outside the layers changed (the
  frame log's hash), the window blits the last target and composes the layers as textured
  quads instead of running vello.
- **Patches** (`canvas::CanvasDiff`, `Painter::patch_layer`): after an edit or a selection,
  the rows that changed (a Myers diff of per-row hashes) give world boxes; only those parts
  of the layer are redrawn, on vello's tile grid.
- **Incremental scene and checks**: `scene::update` re-stamps only changed objects; check
  passes post their delta (`Annotations::patch`); undo/redo recompute over the undone entry's
  ops instead of rebuilding.
- **Glyphs shaped once per string** (`CanvasScratch`).

## Where the time still goes

At 100k objects an edit spends ~28 ms in elab (connectivity 20 ms, checks 7 ms), ~7 ms
refilling plugin snapshots (two delivery rounds), ~4.5 ms rebuilding decor, ~3.4 ms diffing
the canvas tables, ~1.5 ms in the scene update and ~1 ms painting the patch. See
`docs/BACKLOG.md` (Performance) for the next steps.
