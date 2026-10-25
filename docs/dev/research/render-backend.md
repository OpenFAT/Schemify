# Render backend: should vello go?

Research note, 2026-10-24. Branch `refactor/gui-crates` at `d305f31f`. Machine: see
[perf.md](../perf.md) § Machine (i9-14900HX, RTX 4060 Laptop, Raptor Lake-S iGPU, NixOS,
river on Wayland). The external monitor (ASUS VG27WQ1B on DP-4) runs at **59.95 Hz**, though it
offers 165 Hz (`wlr-randr`).

## The question

The user sees laggy zoom, pan and arrow-key moves, and stale or blank canvas tiles. Their own
measurement with `SCHEMIFY_FRAME_LOG=1` on the RTX 4060 gave input-to-present p50 21 ms,
p95 39 ms over 314 frames, every frame composed without vello. Two questions follow:

1. Where does that time go? Is vello the cause?
2. Is there a renderer that would make the tile cache, the layer compositor and the
   intermediate blit unnecessary, within Schemify's constraints (D2, D5, D6, D39, web spec)?

## Answer in short

- **vello does not cause the latency.** At mouse input rate, about 90% of the time between an
  input and `present` is spent waiting for the Wayland compositor's frame callback. winit
  holds `RedrawRequested` until that callback arrives, because `Shell::render` calls
  `pre_present_notify`. The CPU work is under 1 ms, and `get_current_texture` takes 0.03 ms.
  Swapping the renderer would not move these numbers. The fixes are in `window.rs`'s pacing
  and in the display settings (§ 1).
- **vello does cause the stale and blank tiles and the iGPU cost.** Tile cache misses are
  re-rasterized through vello inside the frame (`Layers::raster`: p95 5–6 ms, p99 9 ms on the
  RTX 4060 during a pan). That cost, a per-frame budget, and the tiles still waiting for it
  are what the user sees as blank tiles.
- **Recommendation: replace vello on the GPU with our own instanced wgpu pipelines** over
  retained per-kind row buffers. These are patched from `CanvasDiff` rows and drawn with one
  camera uniform. Text goes through a glyph atlas filled from parley glyph runs. Keep
  `vello_cpu` as the CPU painter for goldens and the web no-GPU fallback.
- In the spike, pan and zoom at 100k objects cost **0.30–0.40 ms of GPU and 0.06–0.10 ms of
  CPU** per frame on the RTX 4060, and **2.9–5.5 ms of GPU** on the iGPU. There is no cache and
  nothing to refill. On the same rows, vello re-encoding the scene takes 2.1 ms GPU + 8.8 ms
  CPU (RTX 4060) and 30.7 ms GPU + 9.1 ms CPU (iGPU) at zoom-fit. That gap is why the tile
  cache exists.

## 1. Where the time goes (measured)

### Method

The instrumentation was a temporary patch to `crates/view/render/src/window.rs`. It was never
committed; the patch is kept in the session scratchpad as `window-instrumentation.patch`.

- A thread posts synthetic inputs through the event-loop proxy at jittered intervals:
  - `SCHEMIFY_SYNTH_MS=8` means 6–10 ms apart, about a mouse's rate.
  - `=50` means 37–62 ms apart, slower than the display.
- An input is an arrow key (`view.pan_left`/`right`) or a wheel notch (`SCHEMIFY_SYNTH=zoom`).
- Each frame stamps its oldest input and every stage up to the return from `present`.
- Input file: the perf schematic at 100k objects (`perf.rs` `schematic(100_000)`), opened with
  `schemify-gui --clean`, in a 1280×800 window on the RTX 4060 (`WGPU_ADAPTER_NAME=NVIDIA`)
  with Vulkan and mailbox present, `desired_maximum_frame_latency: 1`.
- The surface offers `[Mailbox, Fifo, Immediate]`.

### Inputs at mouse rate (8 ms): the user's numbers reproduced

ms, p50 / p95 / p99 / max:

| Stage | Mailbox, keys | Mailbox, wheel zoom | FIFO, keys | Immediate, keys | Mailbox, no `pre_present_notify` |
|---|---|---|---|---|---|
| input → `update` done | 0.26 / 0.35 / 0.39 / 0.48 | 0.28 / 0.37 / 0.43 / 0.63 | 0.26 / 0.35 | 0.21 / 0.27 | 0.28 / 0.36 |
| `update` → `RedrawRequested` (frame-callback wait) | **18.4 / 37.1 / 52.4 / 58.5** | **23.8 / 50.5 / 53.6 / 60.9** | **19.9 / 47.9** | **10.6 / 37.4** | 0.02 / 0.02 |
| paint (CPU) | 0.12 / 0.26 | 0.13 / 0.19 | 0.13 / 0.30 | 0.79 / 2.11 | 0.12 / 0.17 |
| `Layers::raster` (vello tile rasters) | 0.63 / 5.33 / 8.88 / 12.7 | 0.68 / 1.02 | 0.66 / 7.65 | 6.81 / 8.48 | 0.62 / 0.95 |
| `get_current_texture` | 0.03 / 0.04 | 0.03 / 0.05 | 0.03 / 0.05 | 0.03 / 0.04 | **0.05 / 21.6 / 25.6 / 29.0** |
| vello frame render | 0 (0 of 211 frames) | 0 / 0 / 0.43 / 6.2 (2 of 150) | 0 | 0 | 0 |
| blit + compose + submit | 0.32 / 0.65 | 0.32 / 0.71 | 0.31 / 0.59 | 0.51 / 1.04 | 0.51 / 0.83 |
| **input → present** | **20.2 / 46.3 / 54.2 / 60.5** | **25.4 / 52.7 / 55.8 / 62.7** | **22.5 / 51.2** | **19.2 / 47.4** | **2.09 / 23.5 / 27.1 / 30.5** |
| frames for 600 inputs | 211 | 150 | 167 | 205 | 498 |

### Inputs slower than the display (50 ms, mailbox, keys)

| Stage | p50 / p95 / p99 / max |
|---|---|
| `update` → `RedrawRequested` | 0.02 / 0.02 / 7.4 / 13.0 |
| `Layers::raster` | 0.82 / 6.27 / 9.47 / 10.0 |
| input → present | **1.92 / 8.51 / 11.0 / 14.7** |

### Reading

- **The wait is the compositor's frame pacing, not rendering.**
  - `Shell::render` calls `window.pre_present_notify()`, which makes winit request a Wayland
    frame callback (`winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs:301`). The
    next `RedrawRequested` is held until the callback arrives
    (`…/wayland/event_loop/mod.rs:486`).
  - At mouse rate, every frame waits 18–24 ms (p50), and up to 60 ms. Only 150–211 frames
    answered 600 inputs in about 4.8 s, so the callbacks came at 31–44 Hz on a 60 Hz output.
  - When inputs come slower than the display, the callback has already arrived and the same
    build answers in 1.9 ms.
  - The user's p50 21 / p95 39 ms matches the 8 ms column.
- **Removing the throttle moves the wait into `get_current_texture`.** Without
  `pre_present_notify`, the p50 drops to 2.1 ms. At p95, the swapchain has no free image while
  the compositor holds them, and acquire blocks for 21.6 ms. Mailbox only drops frames. It
  cannot make the compositor release images sooner.
- **The present mode is not the lever.** FIFO, mailbox and immediate are within noise of each
  other. Immediate is not faster, because the callback still gates the frame.
- **None of the stages after the wait depends on vello, except `Layers::raster`.** That stage
  re-rasterizes the tiles a pan uncovers through vello: p95 5–8 ms, p99 9–13 ms on the RTX 4060.
  This is vello's share of the latency, a tail rather than the median. The same work on the
  iGPU costs 6–21 ms per vello frame
  (HANDOFF.md table, including the 5.3 ms timestamp floor).

The callback rate below the refresh rate is unexplained. The candidates are the compositor
(river), or cross-GPU buffer import when the window renders on the RTX 4060 and the output is
scanned out by the other GPU. It is not settled here: the next measurement is
`wp_presentation` feedback, which gives the actual time a frame reached the screen.

### What would cut the latency, in order of payoff

1. **Run the monitor at 165 Hz.** It is at 59.95 Hz. Each callback or vblank wait shrinks by
   2.75×. This is a user setting with no code change.
2. **Pace from input, not from the callback alone.**
   - Render right away when the previous frame's callback has arrived, or when the last
     present is older than one refresh period.
   - Otherwise wait for the callback, but sample input as late as possible: run `update` in
     `RedrawRequested` rather than in `about_to_wait`. The frame then shows the newest input,
     not the oldest.
   - Measure from the **newest** input as well as the oldest. Today's log reports the oldest
     input of a coalesced batch, so it overstates what the hand feels.
   - Change radius: 2 bodies (`Shell::render`, `about_to_wait`).
3. **Keep a second swapchain image free** (`desired_maximum_frame_latency: 2` with mailbox) if
   `get_current_texture` still blocks once 2 lands. Not measured: GPU runs stopped for the
   shared desktop.
4. **Remove vello's in-frame tile rasters.** This takes the 5–13 ms tail and the blank tiles
   off the RTX 4060, and the 6–21 ms per frame off the iGPU. Sections 2–5 cover it.

Removing vello alone would change the p50 of the user's measurement by less than 1 ms. It would
change the p95/p99 tail by 5–9 ms, and it would end the stale and blank tiles.

## 2. What vello costs Schemify today

These pieces exist because vello re-encodes and rasterizes the whole scene each frame
(`Renderer::render_to_texture`). At 100k objects that is 9 ms of CPU encode, too slow per
frame (§ 4):

| Workaround | Where | Lines |
|---|---|---|
| World-anchored raster tile cache, per-frame record budget (`COST_PER_FRAME`, `TILE_COST`, `ZOOM_IN_COST`, `RING_SHARE`, `SPLIT_MAX`), stretched stand-ins mid-zoom | `render/src/tiles.rs` | 798 |
| Raster layer slots, frame log hash, "reusable" frame composer (quads over the last vello frame), top-part texture | `render/src/layers.rs` | 760 |
| Layer recording, patching and `live_layer` in the painter; LOD pixel masks; path batching per (layer, style) | `render/src/painter.rs`, `canvas.rs` | ~900 of 1917 |
| Intermediate `Rgba8Unorm` storage texture and `TextureBlitter` copy every frame (vello's compute shader cannot write most surface formats) | `render/src/window.rs` | ~80 |
| `keep_atlas`: a transparent gradient pixel added to scenes without images. Otherwise vello 0.11's `Resolver::resolve` returns `Images::default()` and the renderer swaps in a 1×1 atlas while the image cache still marks every image as resident, so the canvas layer vanishes (`vello_encoding-0.11.0/src/resolve.rs:191`) | `render/src/layers.rs` (paintfix) | — |
| Mark culling. Without it, select-all at 100k made vello request an **805 MB scene buffer**, above wgpu's 256 MB limit, and crash | `render/src/canvas.rs` `draw_marks` (paintfix) | — |
| Cache tests: composed frame = vello frame, patched layer = recorded layer | `render/tests/gpu.rs`, `cache.rs` | ~600 |

Known bugs of this machinery so far:

- a tile's fixed cost scaled by world area, so a zoom-out took 50–65 frames to refill
  (`486faef1`);
- blank tiles while the budget catches up;
- about 700 MB of growth per select-all frame in `paint::frame` and the layers (paintfix);
- the vanishing-layer atlas bug above.

The unselect frame is still about 20 ms of CPU, with render calls at p50 4.1 ms.

Beyond the workarounds, vello's compute renderer has three structural limits:

- It cannot run on **WebGL2**: it needs compute shaders, and WebGL2 has none. So a browser
  without WebGPU falls back to a full CPU raster per frame (web spec §7).
- Its README calls it "alpha", and the linebender repo now files it under `research/` as
  experimental (sources in § 3).
- It has no glyph cache: text is re-encoded as outlines every frame.

## 3. Candidates

The full survey is in the appendix, with every claim's source. Each candidate is judged against
the constraints: 100k–1M objects at the D39 budgets; idle at 0%; deterministic goldens;
wasm32 with WebGPU **and** WebGL2 inside 8 MB compressed; Linux, macOS and Windows; safe Rust in
our crates; text through parley and KaTeX fonts; UI widgets drawn by the same painter;
binary size; compile time; dependency health.

| Candidate | Version (date) | wgpu 30 device shared | WebGL2 | Text from parley glyph runs | 100k–1M primitives | Verdict |
|---|---|---|---|---|---|---|
| **Own instanced pipelines** (wgpu + WGSL, iced/gpui/re_renderer pattern) | — | yes | yes (instance vertex buffers, uniform, one texture) | yes (own atlas) | 0.3 ms GPU at 100k on RTX 4060 (§ 4) | **recommended** |
| vello (compute) 0.11 | 2026-10-02 | yes | **no** (compute) | glyph runs, no glyph cache | needs the tile cache (§ 4) | replace |
| vello_gpu 0.3 (ex vello_hybrid; sparse strips) | 2026-10-02 | yes | yes | glyph runs (glifo) | CPU strip work for the whole scene per frame; "does not match the compute-centric vello renderer's raw performance on dynamic and vector-heavy workloads" | fallback if a full vector API is ever wanted |
| vello_cpu 0.3 | 2026-10-02 | n/a | n/a | glyph runs | CPU only | **keep** for goldens and the web no-GPU fallback (`forbid(unsafe_code)`; `f32_pipeline` for snapshots) |
| lyon 1.0.19 + wgpu | 2026-03-08 | yes | yes | none | no built-in AA (MSAA); tessellation is CPU | only for rare complex polygons, tessellated once and cached |
| femtovg 0.27 | 2026-08-31 | yes (`WGPURenderer::new(device, queue)`) | yes (glow) | `fill_glyph_run` | immediate path-per-call API; no published numbers; CPU-bound at 100k+ by design (inferred) | second choice for UI only |
| tiny-skia 0.12 + upload | 2026-02-02 | n/a | n/a | none | CPU, "20–100% slower than Skia on x86-64" | no |
| skia-safe 0.153 | 2026-09-04 | no | emscripten only | yes | fast | no: about 23 MB of binary, C++ FFI, no `wasm32-unknown-unknown` |
| forma | archived 2024-07-18 | — | — | — | — | dead |
| Blitz / anyrender 0.14 | 2026-10-03 | yes | via vello_gpu | per backend | same as its backends | a trait over vello; adds nothing |
| glyphon 0.12 | 2026-07-09 | yes | yes | **no**: takes cosmic-text `Buffer`s only | — | would add a second shaper beside parley |
| iced_wgpu (release 0.14 on wgpu 27; master on 30) | 2025-12-07 | not the release | `webgl` feature | cryoglyph (cosmic-text) | instanced SDF quads plus cached lyon meshes | reference design |
| gpui renderer | 0.2.2 (2025-10-22) | no (blade on crates.io; wgpu on Linux in zed main since 2026-02) | no | OS shaping | instanced SDF quads, sprites, glyph atlas | reference design |
| re_renderer 0.40 (Rerun) | 2026-10-21 | yes | yes (data textures) | none | single-draw vertex-pulled line quads, 1-px analytic feather; points "<10mio interactive" | reference design for wires |
| epaint 0.37 (egui) | 2026-10-15 | yes | yes | own (skrifa) | CPU feathered tessellation every frame | reference for AA, not for 1M |

The three reference renderers (iced, gpui, re_renderer) and the spike draw primitives the same
way: one quad per primitive, expanded in the vertex shader, with analytic coverage in the
fragment shader (`clamp(0.5 - d, 0, 1)`). Static data stays on the GPU, and pan or zoom
changes only a uniform. Text in iced, gpui and glyphon goes through an alpha-mask atlas. None
of them publishes a crate Schemify could use as-is with parley text: glyphon is tied to
cosmic-text, iced and gpui to their frameworks.

## 4. The spike

The spike is a throwaway crate in the session scratchpad (`render-spike/`): `lib.rs` 300
lines, `prims.wgsl` 170 lines, plus `bench` and `window` bins.

What it implements:

- Four instanced pipelines: lines and wires with butt caps and dashes; rects as outline or
  fill (box SDF); circles and arcs (ring SDF with an angle mask); glyph quads from an R8 atlas.
- Rows are `i32` milli-units, 20–24 B each.
- The camera uniform carries an `i32` center and an `f32` fraction, so the shader subtracts
  in integers before converting to `f32`. That is exact over the D33 range: differences up to
  4.2 M stay below 2^24.
- It uses no storage buffers and no compute: only instance-rate vertex buffers, one uniform
  and one texture, so it is WebGL2-compatible as written.
- It does no culling and no LOD. Every row goes through the vertex shader every frame, so the
  numbers are its worst case.

The rows mirror `perf.rs` `schematic(n)`. Each tile has an nmos-like symbol (10 lines, a
rect, a circle and 12 glyphs), 6 wires, 2 labels and a text. At 100k objects that is 160k
lines, 10k rects, 10k arcs and 220k glyph quads: 8.0 MB of rows. At 1M it is 80 MB.

The vello baseline renders the **same rows**, re-encoded every frame: one stroke path per
layer (as `canvas.rs` batches), circles, rects, and glyph boxes as filled rects (cheaper than
real outlines, which favours vello). Off-screen rows are culled on the CPU first. Target:
1280×800 `Rgba8Unorm`, offscreen. GPU time comes from render-pass timestamps for the spike,
and from encoder timestamps minus the measured floor for vello. Pans are 120 frames, the
first 20 dropped; the zoom sweep is 100 frames at ×1.05 each.

### Frame cost, p50 (p95)

| Rows (objects) | View | Spike GPU | Spike CPU | vello GPU | vello CPU (encode + render calls) |
|---|---|---|---|---|---|
| 100k, RTX 4060 | zoom-fit (all visible) | **0.40 (0.50)** | 0.10 | 2.12 (3.89) | 8.8 (10.0) |
| | 1 unit = 1 px | 0.30 | 0.06 | 0.21 | 1.6 |
| | zoom sweep | 0.22 (0.33) | 0.04 | — | — |
| 1M, RTX 4060 | zoom-fit | 2.99 (3.71) | 0.18 | 46.8 (51.0) | 112 (124) |
| | 1 unit = 1 px | 2.23 | 0.12 | 0.79 | 8.3 |
| 100k, Intel iGPU | zoom-fit | **5.5 (7.6)** | 0.18 | 30.7 (33.6) | 9.1 (13.2) |
| | 1 unit = 1 px | 2.9 (3.0) | 0.23 | 0.52 (2.99) | 1.2 |
| | zoom sweep | 2.9 (5.2) | 0.12 | — | — |
| 1M, Intel iGPU | zoom-fit | 69 (91) | 0.25 | 131 (139) | 177 (196) |
| | 1 unit = 1 px | 29.0 | 0.20 | 2.1 (4.8) | 7.5 |

### Edits and memory

| | 100k | 1M |
|---|---|---|
| Rows on the GPU | 8.0 MB (+1 MB atlas) | 80.4 MB |
| RSS after upload (spike keeps a CPU copy, as the canvas tables do) | +14 MB | +87 MB |
| Pipelines + first upload | 65 ms | 62 ms |
| Patch 16 rows (a single-object edit) | 0.026 ms CPU | 0.014 ms CPU |
| Rewrite every line row (select-all worst case) | 3.2 MB: 0.50 ms CPU, 0.05 ms GPU | 32 MB: 3.7 ms CPU, 1.7 ms GPU |

### Reading

- **100k objects meets D39's 120 fps (8.3 ms) with no cache on both GPUs.** The worst case
  is zoom-fit on the iGPU at 5.5 ms p50 and 7.6 ms p95. The same view through vello costs
  30.7 ms of GPU plus 9.1 ms of CPU, and that is what forced the tile cache.
- **CPU per frame is a uniform write and five draw calls: 0.04–0.25 ms**, flat from 100k to
  1M. The D39 1 ms CPU budget for interactive frames then belongs to `update` alone.
- **Without culling, the spike is vertex-bound.** 4.1M instances × 6 vertices cost 2.2 ms on
  the RTX 4060 and 29 ms on the iGPU, at any zoom. vello wins zoomed-in views at 1M (0.8 ms
  against 2.2 ms GPU on the RTX 4060), but only because its baseline culls on the CPU and the
  spike culls nothing. The fix is the one re_renderer and gpui use: rows sorted into world
  buckets, with draws issued only for visible bucket ranges (§ 5, step 6). The cost then
  scales with visible rows, about the 100k numbers or less.
- **Zoom-fit at 1M on the iGPU (69 ms) needs a level of detail.** Glyph quads below 3 px are
  already dropped in the vertex shader, but they still cost vertex work: 55% of the rows.
  Skipping the glyph draw on the CPU when every text is below 3 px removes that. The remaining
  1.8M sub-pixel lines need a far-zoom overview: one instanced pass into a texture once per
  content change, shown while lines are under 2 px. That replaces today's LOD pixel masks.
  It is the one cache the design keeps, and it is invalidated whole, so it has no tile
  bookkeeping.
- **An edit is O(change) bytes.** A single-object edit is 16 rows, 320 B, 0.03 ms.
  Select-all is a marks buffer of the selected rows: 100k objects is about 2 MB, about 0.5 ms
  of CPU. Compare the 805 MB scene request and the 700 MB of growth today.

The spike's frame output was checked visually (a 1280×800 PNG of the 4 px/unit view): symbols,
wires, a filled pin rect, the gate circle and the glyph quads render with 1-px AA edges.

### Binary size

How the probe was built:

- `size_probe` (spike crate) creates a wgpu device and draws one pass. It is built three ways:
  - baseline: a clear pass;
  - the spike's instanced renderer;
  - vello's `Renderer` drawing a two-shape scene.
- Profile: `tools/size-check.sh`'s (fat LTO, one codegen unit, `panic=abort`, stripped),
  `opt-level=s`.
- Native: wgpu with Vulkan and GLES. Wasm: wgpu with WebGPU and WebGL2.

| | Native (bytes) | Δ vs baseline | wasm32 raw | wasm32 gzip -9 | Δ gzip |
|---|---|---|---|---|---|
| wgpu + clear (baseline) | 3,036,136 | — | 3,202,210 | 934,668 | — |
| + instanced renderer (spike) | 3,372,008 | **+336 KB** | 3,543,676 | 1,051,996 | **+117 KB** |
| + vello `Renderer` | 4,527,096 | **+1,491 KB** | 4,600,031 | 1,334,460 | **+400 KB** |

Both deltas include naga's WGSL front end, which the clear-only baseline does not link.
Replacing vello with the spike saves about 1.15 MB native and 280 KB of compressed wasm,
before UI and text code. Glyph rasterization reuses `vello_cpu`, which stays linked either way.

### Input-to-present latency with the spike

Not measured. GPU runs were stopped because the benchmark's 1M case made the shared desktop
lag. The spike's `window` bin is ready: `SPIKE_GPU=nvidia nice -n 19 target/release/window
100000` with `SPIKE_MS=8 SPIKE_N=300`. The § 1 analysis predicts it will show the same
frame-callback wait as the editor (p50 about 18 ms at 8 ms inputs), because the stage that
waits sits before any rendering.

## 5. Recommendation and target architecture

**Replace vello on the GPU path with Schemify's own instanced wgpu pipelines.** Keep
`vello_cpu` behind the same `Painter` trait for goldens and the no-GPU web fallback.

### Why not keep vello for UI and text only

As a midpoint (steps 1–2 below), vello for UI plus instanced pipelines for the canvas is
sound, and it is the cheapest way to get the canvas out of tiles. As an end state, it keeps
most of vello's cost:

- Every UI change is still a full vello frame into an intermediate texture plus a blit. A hover
  changes the status line, so hover frames are vello frames: 2.6 ms of iGPU above the floor
  (HANDOFF.md).
- The renderer still has no WebGL2 path.
- We still carry the `keep_atlas` workaround, the compute-shader stack and its binary size.

The UI is small (thousands of rects and glyph quads), which is exactly what an instanced
batcher draws in microseconds. Decide the final step (step 4) on the measured UI frame cost
after step 3.

### Data (the six questions)

1. **In → out.**
   - `CanvasPrims` (canvas crate, unchanged) + its `CanvasDiff`/`TableChange::Rows` edits →
     **`GpuRows`**: one instance buffer per kind (line, rect, arc, poly segment, glyph) plus a
     marks buffer.
   - Camera + palette + widths → one uniform.
   - UI layout + glyph runs → a per-frame **batch** of the same instance kinds.
   - Out: one render pass straight into the surface texture.
2. **How many.** Up to 2^22 rows per kind (architecture.md's canvas capacity). At 20–24 B
   that is ≤ 100 MB per kind, under wgpu's default 256 MB buffer limit. UI: about 10^3–10^4
   instances per frame.
3. **How wide.**
   - Positions `i32` milli-units (D33), subtracted from an `i32` camera center in the
     shader.
   - Layer, weight, dash and fill packed in one `u32` (as `LineStyle` plus layer). The layer
     bits read `LAYER_DEAD` for killed rows, which the vertex shader culls.
   - Colors and widths stay in the uniform, indexed by layer (32 entries), so a palette
     change is one uniform write.
4. **Access.** The GPU reads every visible bucket's rows each frame. The CPU writes only rows
   that `CanvasDiff` reports as killed or appended. Rows are AoS on the GPU (the vertex fetch
   wants one row) and SoA on the CPU (the producer's tables). The pack loop between them is
   one straight transform per kind.
5. **Lifetime.**
   - `GpuRows` live with the canvas until a full rebuild: a document switch, or a diff over
     `DIFF_EDITS_MAX`. Then the buffers are rewritten whole, 0.5 ms per 3.2 MB.
   - Glyph atlas entries are evicted least recently used; the atlas is ≤ 2048² (the WebGL2
     limit).
   - The far-zoom overview texture is invalidated whole by any edit.
6. **Parallel.** The GPU is the parallel part. The CPU side is a few `write_buffer` calls per
   frame.

### Owners, tables and the spine

| Table | Owner (writes) | Readers | Capacity |
|---|---|---|---|
| `CanvasPrims`, `PrimIndex`, `CanvasDiff` | `canvas` / editor scene (unchanged) | `render` | 2^22 rows per kind |
| `GpuRows` (per-kind buffers, bucket ranges, marks buffer) | `render::gpu` (new) | GPU | 2^22 rows per kind |
| Glyph atlas (key → atlas rect) | `render::gpu` | GPU | 2048² R8, LRU |
| Frame batch (UI instances, scissor stack) | `render::gpu`, per frame | GPU | 2^16 instances per frame |
| Paint caches | `app` (`paint::Caches`: the `GpuRows` handle replaces the tile cache and its `CanvasDiff`) | `app::paint` | 4 canvases |

The frame spine stays the one in architecture.md. Step 12 becomes:

```text
app::paint(Editor::picture)
  render::draw_ui(Main)              → batch: rects, glyph quads, icons (atlas), scissor clips
  render::canvas::draw(canvas rect)  → GpuRows.apply(diff rows); grid (analytic, one quad);
                                       visible bucket draws; marks; gesture overlay (batch)
  render::waves::draw_pane           → batch: segment instances per decimated trace
  render::draw_ui(Overlay)           → batch
window: one render pass into the surface texture, then present
```

The `Painter` trait keeps its drawing methods. `CpuPainter` (vello_cpu) stays. The new
`GpuPainter` replaces `ScenePainter`, and the canvas draw becomes one trait method with two
cases:

- the GPU painter draws its retained rows;
- the CPU painter runs today's `draw_prims` loop.

That is the seam's two cases today, so it passes the `codebase-clean-code` ladder at rung 4
(compile-time generic, no `dyn`).

### Deleted from `render`

| File | Today | After |
|---|---|---|
| `tiles.rs` | 798 | deleted |
| `layers.rs` | 760 | deleted |
| `painter.rs` | 948 | about 450: `layer`, `patch_layer`, `draw_layer`, `holds_layer` and `live_layer` go; `GpuPainter` comes in |
| `canvas.rs` | 969 | about 450: `draw_cached`, `CanvasCache`, LOD masks and path batching go; the CPU `draw_prims` stays for goldens |
| `window.rs` | 765 | about 650: the vello target, `TextureBlitter`, `reusable` and `compose` go |
| `gpu/` (new: rows, batch, atlas, `prims.wgsl`) | — | about 1,200 |
| tests: `cache.rs`, the cache halves of `gpu.rs` | ~600 | replaced by a GPU-vs-`CpuPainter` tolerance test per primitive kind, and a row-patch oracle (patched buffers equal a rebuilt buffer) |

Net change: about −2,300 lines. Dependencies removed: `vello`, `vello_encoding` and
`vello_shaders`. Added: none. The glyph atlas packs shelves by hand (about 60 lines;
`etagere` only if fragmentation measures badly). Glyph masks are rasterized by `vello_cpu`,
already a dependency, from parley glyph runs. `swash` is the upgrade if per-glyph raster cost
shows up.

### Text, math, widgets, goldens, web

- **Text.** Glyph masks are keyed by (font, glyph id, size bucket of 2^(1/4) steps, subpixel
  x phase of 1/4 px). Mid-zoom, the nearest bucket is drawn scaled. On settle, the missing
  buckets are rasterized: a few hundred distinct glyphs at most, against today's
  screen-area-proportional tile refill. Texts below 3 px are dropped, as now
  (`TEXT_MIN_PX`). This is the glyphon and gpui design, fed by parley instead of cosmic-text.
- **Math (D21).** RaTeX layout yields glyphs and rules: the same glyph quads and rect
  instances.
- **Widgets.** `fill_rect`, rounded rects, borders, carets and selections become rect
  instances. Icons (`draw_paths`) are small fixed paths, rasterized once per (icon, px) into
  the atlas by `vello_cpu`. `push_clip(Rect)` becomes a scissor rect; every clip in
  `widgets.rs` is axis-aligned.
- **Polylines and polygons.** Polylines become segment instances with round joins (a
  capsule SDF) and a cumulative length per row, so dashes stay continuous. Filled polygons
  (symbol bodies, arrowheads) are tessellated once per shape by ear clipping, since they are
  simple polygons. lyon only comes in if self-intersecting fills appear. AA comes from a
  1-px feather strip, as epaint does.
- **Goldens.** Unchanged. They come from `CpuPainter` (vello_cpu), which the trait keeps. GPU
  output is checked against it within a tolerance, as `gpu.rs` does today (mean |Δ| and
  far-pixel count).
- **Web.** The pipelines need no compute and no storage buffers, so the same code runs on
  WebGPU **and** on wgpu's WebGL2 backend:
  - stride ≤ 255 B, atlas ≤ 2048², uniform ≤ 16 KiB;
  - `first_instance` is emulated in the GLES HAL, which bucket draws need;
  - `cpu_view` stays only for "no GPU at all".
  - Size: wgpu with both web backends plus naga is 935 KB gzip in the size probe (§ 4), and
    the renderer adds 117 KB, inside the 8 MB budget.

### Migration plan, in mergeable steps

Each step lands with its oracle and its before and after numbers (perf.md). The change radius
is the count of existing function bodies edited.

| Step | What | Oracle | Change radius |
|---|---|---|---|
| 0 | **Pacing.** `update` in `RedrawRequested`; render without waiting when the last callback has arrived or one period has passed since the last present; log both the newest and the oldest input; then try `desired_maximum_frame_latency: 2`. Also ask the user to run the monitor at 165 Hz. | the frame log on the RTX 4060 and the iGPU at 8 ms synthetic inputs (this note's method); idle CPU still 0% | 3 (`Shell::render`, `about_to_wait`, `window_event`) |
| 1 (done) | **`render::gpu` rows and pipelines for canvas primitives**, drawn straight into the surface texture under the vello UI frame. The vello target becomes premultiplied and blended over the canvas pass instead of blitted opaque. `paint.rs` calls `canvas::draw` instead of `draw_cached` on the GPU painter. | per-kind GPU vs `CpuPainter` tolerance test; row-patch oracle (patched rows = rebuilt rows after random kill/append sequences); `frame_times_100k` GPU column | 6 (`Shell::render`, `Gpu::new`, `paint::frame`'s canvas call, `ScenePainter` canvas hook, `perf.rs` `Gpu::render`, `gpu.rs` render helper) |
| 2 (done) | **Delete the tile cache**: `tiles.rs`, the canvas use of `layers.rs`, `CanvasCache` in `paint::Caches`, LOD masks, and the cache tests and perf.md rows | goldens unchanged; `frame_times_100k`, `interactions_100k` | 4 (`paint::Caches`, `paint::frame`, `Editor::painted`'s partial-record wake, `wake_ms`'s "partly recorded" arm) |
| 3 | **Glyph atlas and UI batch.** `GpuPainter` implements every `Painter` method; canvas text moves to atlas quads | glyph mask vs `CpuPainter` glyph raster per size bucket; app goldens unchanged (CPU); hover and type frames' GPU time on the iGPU | 3 (`Painter` impl split, `draw_texts`, `widgets::draw_text`) |
| 4 | **Remove vello (GPU)**: `layers.rs`, the window's vello target and blit; the `vello` dependency goes, `vello_cpu` stays. Amend D5. | `tools/size-check.sh` before and after; all tests | 2 (`Gpu::new`, `Shell::render`) |
| 5 | **WebGL2.** Enable wgpu `webgl` on wasm32; the WebGPU-unusable path tries WebGL2 before `cpu_view` (web spec §7 amended) | the web smoke test on headless Chrome with WebGPU disabled shows GPU frames | 2 (`Gpu::new` on wasm, `UserEvent::NoGpu` arm) |
| 6 | **Buckets and far-zoom LOD** for 1M: rows grouped by `PrimIndex` cell, visible bucket ranges per frame, glyph draw skipped below 3 px, overview texture below 2 px per line | `frame_times_1m` GPU column on the iGPU under 8.3 ms; overview vs full draw tolerance test | 2 (`GpuRows::apply`, `canvas::draw`) |

Steps 0 and 1 are independent and can land in either order. Step 2 needs step 1, and step 4
needs step 3.

### Step 1: done (2026-10-24)

`render::gpu` (`gpu.rs`, `gpu.wgsl`), drawn by the window and the perf harness; numbers in
[perf.md](../perf.md) § GPU canvas pipelines. What differed from the plan:

- **Five kinds, not four**: fill triangles (28 B), rects (24 B), arcs (28 B), lines (24 B, one
  per table row), polyline and polygon edges (24 B, repacked whole when a chain table
  changes). A row is coordinates, an RGBA colour (0 = empty slot) and one style word: width
  in 1/16 px, dash, fill and parity flags. Colour and width are resolved at pack time, not
  indexed in the uniform, so a palette change repacks (it was already a full re-record).
- **Rows are grouped by palette layer, with free slots.** The CPU painter draws source by
  source and layer by layer, and that order decides how antialiased edges of different
  layers blend. A depth key could not keep it: about half of a 1 px diagonal line's pixels
  are partial. So each kind's array holds per-layer ranges with an eighth plus 16 free slots;
  a kill empties a slot, an append takes a free one, and a full layer or an edit script
  repacks the table. Draws are one per non-empty (source, layer, kind), using base-instance
  ranges (WebGL2 emulates `first_instance`).
- **Strokes are round-capped and round-joined everywhere**, matching kurbo's `Stroke::new`,
  which the CPU painter uses. Coverage is the exact pixel-area of the stroke's band along it,
  plus 16 subsamples at caps and dash ends; the plain distance ramp `clamp(0.5 − d)` was 2–3×
  further from vello_cpu.
- **Canvas text stays with vello**, over the canvas pass. A table whose largest label is
  under 3 px is skipped whole; otherwise the rows near the view come through `PrimIndex`.
- **The vello frame gets a hole** over the canvas (a "destination out" layer; a `Clear` layer
  clears whole 16 px tiles), and the window blends it over the canvas pass.
- **Not done here**: bucket ranges per `PrimIndex` cell and the overview texture (step 6). The
  far-zoom cut (≤ 2 px rows as their pixel box) is in the vertex shader. The text cut is the
  one above.
- **Equivalence oracles**: one per kind, marks, the far-zoom threshold, a mixed scene under a
  UI frame, and the 100k fit (with its own documented bound), plus a moved rect as the
  negative space. The pack is checked headless: patched rows equal a fresh pack after random
  kill and append edits.

### Step 2: done (2026-10-24)

Deleted: `tiles.rs`, `CanvasCache` and the tile half of `paint_canvas`, `canvas::draw_cached`,
`draw_prims_in` and its LOD bit masks, the whole `canvas::Drawn` enum (every outcome but
`Direct` was a tile outcome, so `Editor::painted` takes only `took_edits`), the editor's
`stretched`, `partial` and `zoom_until` state with `ZOOM_SETTLE_MS` and `CanvasPicture::crisp`,
and the tests whose subject went: `tests/cache.rs`, the composed-vs-vello and
patched-vs-recorded layer tests of `gpu.rs`, `zoom_on_a_light_cell_fills_the_view_in_a_few_frames`
and `the_paint_outcome_schedules_the_next_frame_through_painted`. Numbers in
[perf.md](../perf.md) § GPU canvas pipelines, step 2. What differed from the plan:

- **`CpuPainter` reads `InstanceRows`**: it keeps its own rows across frames, packed by the same
  `pack` the GPU's are, and `canvas::draw_rows` rasterizes them layer by layer with vello_cpu.
  `draw_prims` and `draw_marks` (the overlay, PNG export, render tests) pack into a scratch
  row set and draw the same way. An edge row now carries two more flags (`FIRST`, `CLOSED`) so
  the CPU strokes a polyline or polygon as the one path it is (joins, a continuous dash); the
  shader ignores them.
- **`layers.rs` keeps only the live layer**: the stamped layer (`Painter::layer`,
  `holds_layer`, `draw_layer`, `patch_layer`), the patch batches and the stretched stand-in
  (bilinear sampler, fill) are gone, and so are `CpuPainter`'s raster layers. `Layers` still
  logs the frame and composes the status bar's live layer without vello, until step 4.
- **Goldens**: the row path draws the render goldens byte-identical (`canvas_1k` drew directly
  before and after). Nine app goldens move by ±1 level on 4–18 pixels (mean |Δ| ≤ 0.00001),
  because the tile intermediate is gone (each tile was its own pixmap, its paths placed
  relative to the tile origin and clipped to it). They pass `check_golden` and are left
  unblessed on purpose, as are the goldens that were already stale at `1ed77d5e`
  (`cell_panel_modern`, `core_symbols_*`, `menu_file`, `canvas_1k`, 3–657 px within tolerance).
- **Ported oracles**: `a_kept_canvas_equals_a_fresh_pack` (app: a painter that patches its rows
  against a fresh one over seeded random edits), `an_idle_frame_does_no_work` now also asserts a
  zoom frame asks for no other, and `a_pan_or_zoom_frame_writes_no_instance_bytes` (app: pan and
  zoom frames leave zero dirty row bytes while the camera moves; a delete writes rows).
- **Not done here**: `CanvasDiff` still returns the world boxes of changed rows, which only
  the tile cache read (`paint::Caches::canvas_boxes`, marked `ponytail:`); dropping them is a
  `canvas` crate change of its own. The CPU still fills a layer's closed shapes as one
  non-zero path (the cancelling-winding artifact in § GPU against CPU); filling per shape
  changes golden pixels, so it waits for a decision.

## 6. Risks

| Risk | Mitigation |
|---|---|
| **The latency stays.** The frame-callback wait is in the compositor or driver path, not the renderer. | Step 0, measured with `wp_presentation` feedback; the monitor's refresh rate. Say so to the user up front. |
| Text quality from an atlas: bucket blur mid-zoom, hinting differences from vello's outlines | Size buckets at 2^(1/4) and re-raster on settle. The goldens stay vello_cpu, so a text regression shows in the GPU-vs-CPU tolerance test. |
| Joins, dashes and wide strokes differ from vello's | Capsule segments for polylines, cumulative dash phase per row, and the per-kind tolerance tests. Schematic strokes are 1–4 px. |
| Filled polygons need tessellation | Ear clipping once per shape, cached with the row. lyon only for self-intersecting fills. |
| Vertex-bound at 1M without buckets (29 ms on the iGPU, measured) | Step 6. 100k already meets budget without it. |
| WebGL2 backend is "experimental" in wgpu's own wiki | Keep `cpu_view` as the last fallback; the smoke test covers it. |
| Our WGSL (about 250 lines) is ours to maintain | It is small and covered by the per-kind oracles. iced, gpui and re_renderer have used this shape for years. |
| GPU output differs by driver | Goldens stay CPU; GPU tests use tolerances, as today. |
| Buffer growth past capacity | Capacity doubling with `copy_buffer_to_buffer`, asserted against 2^22 rows per kind. |

## 7. Proposed decisions.md amendment (not applied)

> | 2026-10-24 | D5, D39 | **Schemify draws with its own instanced wgpu pipelines; vello's GPU renderer is dropped** (research: `docs/dev/research/render-backend.md`). `render::gpu` owns one retained instance buffer per primitive kind (lines, rects, arcs, segments, glyph quads), patched from `CanvasDiff` rows. Pan and zoom write one camera uniform, and every frame is one render pass straight into the surface (no intermediate texture, no blit, no tile cache, no layer compositor). Text and math are drawn as quads from a glyph atlas filled from parley glyph runs; UI widgets go through the same instance kinds per frame. `vello_cpu` stays as the CPU `Painter` for goldens and the no-GPU web fallback. The pipelines use no compute and no storage buffers, so the web build draws on the GPU under WebGPU and WebGL2. Measured 2026-10-24 at 100k objects: pan/zoom 0.30–0.40 ms GPU and ≤ 0.1 ms CPU on the RTX 4060, 2.9–5.5 ms GPU on the Raptor Lake iGPU, against vello re-encoding at 2.1 ms GPU + 8.8 ms CPU and 30.7 ms GPU + 9.1 ms CPU. Input-to-present latency is set by the window's frame pacing, not by the renderer (same note, § 1). |

## Appendix: survey sources

The candidate survey with a primary source for every claim (crates.io API, repositories,
docs.rs, crate sources) follows, as gathered on 2026-10-24.

Sources: crates.io API (`https://crates.io/api/v1/crates/<name>` and `/<ver>/dependencies`), GitHub repos, docs.rs, crate sources (local registry or `static.crates.io` tarballs). "CIO" below = `https://crates.io/api/v1/crates/<name>`. Download counts are all-time / last 90 days.

### Summary table

| Candidate | Latest (date) | wgpu req | Share our wgpu 30 device? | WebGPU | WebGL2 | Text | AA | Fit |
|---|---|---|---|---|---|---|---|---|
| vello (compute) | 0.11.0 (2026-10-02) | ^30 | yes | yes | **no** (needs compute) | glyph runs via skrifa, no glyph cache | area / MSAA8 / MSAA16 | fails WebGL2; per-frame encode |
| vello_gpu (ex vello_hybrid) | 0.3.0 (2026-10-02) | ^30 (opt) | yes | yes | **yes**, native WebGL2 backend | glyph runs via glifo | sparse-strip analytic coverage | best-matching vector renderer; less mature |
| vello_hybrid | 0.2.0 (2026-08-07) | ^29.0.3 | **no** (wgpu 29) | yes | yes (`webgl` feature) | glyph_run | same | superseded by vello_gpu |
| vello_cpu | 0.3.0 (2026-10-02) | none | n/a (CPU) | n/a | n/a | glyph_run | analytic, u8 or f32 pipeline | keep for goldens |
| glyphon | 0.12.0 (2026-07-09) | ^30 | yes | via wgpu | via wgpu | cosmic-text 0.19 Buffers only | grayscale atlas | wrong shaper (not parley) |
| cryoglyph | 0.1.0 (2025-12-05) | ^27 | no | via wgpu | via wgpu | cosmic-text 0.15 | atlas | iced-internal fork |
| wgpu_glyph | 0.28.0 (2026-03-12) | ^28 | no | | | glyph_brush | | superseded |
| lyon | 1.0.19 (2026-03-08) | none | n/a | n/a | n/a | none | **none** (needs MSAA) | useful only as a tessellator |
| femtovg | 0.27.0 (2026-08-31) | ^30 (opt) | yes (`WGPURenderer::new(Device, Queue)`) | yes | yes (glow) | own shaping + `fill_glyph_run` | NanoVG-style | plausible, but path-per-call design |
| tiny-skia | 0.12.0 (2026-02-02) | none | n/a | n/a | n/a | none | analytic | CPU only, too slow for 120 fps |
| skia-safe | 0.153.3 (2026-09-04) | none (own GPU) | no | no | emscripten only | full | Skia | fails size, safe-Rust, wasm32-unknown-unknown |
| forma-render | 0.1.3 (2022-12-16) | | | | | | | archived; dead |
| anyrender(_vello) | 0.14 / 0.15 (2026-10-03) | ^30 | yes | per backend | via vello_gpu backend | per backend | per backend | abstraction only; adds nothing |
| iced_wgpu | 0.14.0 (2025-12-07); master on wgpu 30 | ^27 (release) | not the release | yes | `webgl` feature | cryoglyph | SDF quads + MSAA meshes | pattern to copy, not dep |
| gpui | 0.2.2 (2025-10-22) | blade 0.7 (release) | no | no | no | OS shaping + atlas | SDF | not a standalone renderer |
| re_renderer | 0.40.0 (2026-10-21) | ^30 | yes, in principle | yes | yes (data textures) | none | 1-px coverage feather in FS | pattern to copy for wires |
| epaint + egui-wgpu | 0.37.0 (2026-10-15) | ^30 | yes | yes | yes | own (skrifa) | feathered meshes | pattern to copy for AA |
| bevy gizmos | bevy_gizmos_render 0.20.0 | bevy's | no | | | | none in shader (MSAA) | pattern only |

### 1. Vello family (linebender/vello)

| | vello 0.11 | vello_gpu 0.3 | vello_cpu 0.3 |
|---|---|---|---|
| Version / date | 0.11.0, 2026-10-02 ([CIO vello](https://crates.io/api/v1/crates/vello)) | 0.3.0, 2026-10-02; crate created 2026-08-26 ([CIO vello_gpu](https://crates.io/api/v1/crates/vello_gpu)) | 0.3.0, 2026-10-02 ([CIO vello_cpu](https://crates.io/api/v1/crates/vello_cpu)) |
| wgpu | `wgpu ^30.0.0` (optional) ([deps](https://crates.io/api/v1/crates/vello/0.11.0/dependencies)) | `wgpu ^30.0.0` optional; `webgl` feature pulls `web-sys`, `js-sys`, `vello_gpu_shaders/glsl` ([deps](https://crates.io/api/v1/crates/vello_gpu/0.3.0/dependencies), [version](https://crates.io/api/v1/crates/vello_gpu/0.3.0)) | none |
| Backend | wgpu compute shaders: "needs a GPU with support for compute shaders" ([README](https://github.com/linebender/vello/blob/main/README.md), local `vello-0.11.0/README.md`) | "The CPU processes paths, tiles, and schedules sparse strips. The GPU rasterizes those strips and performs compositing." Native WebGL2 backend plus a wgpu backend, "without requiring compute shaders" ([docs.rs](https://docs.rs/vello_gpu/0.3.0/vello_gpu/), [repo](https://github.com/linebender/vello)) | CPU, SIMD on all major arches incl. WASM SIMD; optional multithreading (local `vello_cpu-0.3.0/README.md`) |
| Status | README: "Vello can currently be considered in an alpha state" (blur/filters, conflation artifacts, GPU memory strategy, glyph caching outstanding). The repo now files the compute renderer as `research/` and calls it "experimental" ([repo](https://github.com/linebender/vello)) | "slightly less mature than its CPU-only counterpart"; mask layers, complex filters and some blend modes panic; glyph caching experimental; "particularly the wgpu backend" not fully optimized ([docs.rs](https://docs.rs/vello_gpu/0.3.0/vello_gpu/)) | "the most mature choice"; complex filters panic; glyph caching experimental ([repo](https://github.com/linebender/vello), local README) |
| Perf | "177 fps for the paris-30k test scene on an M1 Max, at 1600 px square ... something of a best case" (README) | "does not match the compute-centric vello renderer's raw performance on dynamic and vector-heavy workloads" ([docs.rs](https://docs.rs/vello_gpu/0.3.0/vello_gpu/)) | benchmark charts at [laurenzv.github.io/vello_chart](https://laurenzv.github.io/vello_chart/) (local README) |
| Output path | `render_to_texture` needs an `Rgba8Unorm` texture with `STORAGE_BINDING`, then blit to the surface with `wgpu::util::TextureBlitter` (local `vello-0.11.0/src/lib.rs` lines 465-470) | renders to the surface directly | `Pixmap` |
| unsafe | 1 `unsafe` token in src, no `forbid` (local grep) | vello_hybrid 0.2: 2 `unsafe` tokens (local grep) | `#![forbid(unsafe_code)]` (`vello_cpu-0.3.0/src/lib.rs:152`) |

- Repo: 4.4k stars, 132 open issues, 2,104 commits ([repo](https://github.com/linebender/vello)). Directory layout: `vello_cpu`, `vello_gpu`, `vello_common`, `vello_gpu_shaders`, `glifo` (text), `research/` (compute renderer), `vello_bench` (same).
- **Rename:** `vello_hybrid` 0.2.0 (2026-08-07) still requires `wgpu ^29.0.3`, so it cannot share a wgpu 30 device ([deps](https://crates.io/api/v1/crates/vello_hybrid/0.2.0/dependencies)). Its successor `vello_gpu` 0.3.0 is on wgpu 30.
- Text: `glifo` 0.4.0 is the shared glyph crate ([CIO glifo](https://crates.io/api/v1/crates/glifo)). vello_gpu's `text` feature enables `glifo` ([version](https://crates.io/api/v1/crates/vello_gpu/0.3.0)). The renderers take glyph runs (font + glyph ids + positions), which is what parley produces.
- Determinism: vello_cpu's `f32_pipeline` is "slower but has more accurate results. This is especially useful for rendering test snapshots"; `u8_pipeline` is the default for speed (local README). The README makes no promise of bit-exact output across architectures or SIMD levels. GPU output from vello or vello_gpu depends on the driver.
- **Verdict:** vello 0.11 fails the WebGL2 constraint. vello_gpu 0.3 is the only vector renderer here that meets wgpu 30, WebGPU and WebGL2 together. It is still self-described as less mature, and it pays per-frame CPU strip work for the whole scene.

### 2. Text on wgpu

| | glyphon 0.12 | wgpu_glyph / glyph_brush | parley + swash/skrifa into our own atlas |
|---|---|---|---|
| Version | 0.12.0, 2026-07-09; 1.58M downloads ([CIO](https://crates.io/api/v1/crates/glyphon)) | wgpu_glyph 0.28.0 (2026-03-12, wgpu ^28); glyph_brush 0.7.12 (2025-02-21) ([CIO](https://crates.io/api/v1/crates/wgpu_glyph), [CIO](https://crates.io/api/v1/crates/glyph_brush)) | parley 0.12.0 (2026-10-09), swash 0.2.10 (2026-07-17), skrifa 0.49.0 (2026-10-21) ([CIO parley](https://crates.io/api/v1/crates/parley), [swash](https://crates.io/api/v1/crates/swash), [skrifa](https://crates.io/api/v1/crates/skrifa)) |
| wgpu | `wgpu ^30.0.0`, `cosmic-text ^0.19` ([deps](https://crates.io/api/v1/crates/glyphon/0.12.0/dependencies)) | ^28: no | ours |
| Design | cosmic-text shapes and rasterizes, etagere packs the atlas, wgpu samples it; middleware pattern, no extra render pass ([repo](https://github.com/grovesNL/glyphon)). Glyph quads are instanced (`VertexStepMode::Instance`, `src/cache.rs:65`). `BucketedAtlasAllocator` atlas grows up to `max_texture_dimension_2d` (`src/text_atlas.rs`) | README: "This crate has been superseded by glyphon" ([repo](https://github.com/hecrj/wgpu_glyph)) | parley `swash_render` example: walk `layout.lines()` → `PositionedLayoutItem::GlyphRun`, swash `ScaleContext` + `Render::new(&[ColorOutline, ColorBitmap, Outline]).format(Format::Alpha)` → mask or color bitmap ([source](https://raw.githubusercontent.com/linebender/parley/main/examples/swash_render/src/main.rs)). The `tiny_skia_render` example uses a skrifa `OutlinePen` into tiny-skia paths ([source](https://raw.githubusercontent.com/linebender/parley/main/examples/tiny_skia_render/src/main.rs)). Example list: `swash_render`, `tiny_skia_render`, `vello_cpu_render`, `editor` ([dir](https://github.com/linebender/parley/tree/main/examples)) |
| Pre-shaped parley glyphs? | No. `TextArea.buffer: &cosmic_text::Buffer`; only `custom_glyphs: &[CustomGlyph]` (icons through a raster callback) ([docs.rs](https://docs.rs/glyphon/0.12.0/glyphon/struct.TextArea.html)) | no | yes, natively |
| Maintenance | 772 stars, 15 open issues ([repo](https://github.com/grovesNL/glyphon)) | superseded | linebender / googlefonts, very active |
| unsafe | 2 `unsafe` blocks (byte casts) in `viewport.rs:50`, `text_render.rs:322` (crate source) | | swash and skrifa are deps, so their unsafe is allowed under our rule |

- **Verdict:** glyphon would bring in a second shaper (cosmic-text) next to parley. Do it ourselves instead: parley glyph runs → swash (or skrifa + our own raster) alpha masks → etagere atlas → instanced quads. That is the same pipeline glyphon and gpui run internally.

### 3. lyon (tessellation)

| Field | Value |
|---|---|
| Version | 1.0.19, 2026-03-08; 7.9M downloads ([CIO](https://crates.io/api/v1/crates/lyon)) |
| Maintenance | nical, 2.6k stars, 16 open issues, 1,671 commits ([repo](https://github.com/nical/lyon)) |
| Backend | CPU tessellator that outputs vertex/index buffers; renderer-agnostic ([repo](https://github.com/nical/lyon)) |
| AA | "There is currently no built-in support for antialiasing in the tessellators"; use MSAA/FXAA ([repo](https://github.com/nical/lyon)) |
| Perf | No absolute numbers. The new fill tessellator is "about 50% faster than libtess2" on the Rust logo and the Ghostscript tiger ([blog](https://nical.github.io/posts/new-tessellator.html)). Profile split is 77% tessellation and 16% event sorting ([#567](https://github.com/nical/lyon/issues/567)). Recent PRs give 5-7% each ([#969](https://github.com/nical/lyon/pull/969), [#970](https://github.com/nical/lyon/pull/970)) |
| Used by | iced `geometry` feature ([iced wgpu Cargo.toml](https://raw.githubusercontent.com/iced-rs/iced/master/wgpu/Cargo.toml)); gpui depends on `lyon ^1.0` ([deps](https://crates.io/api/v1/crates/gpui/0.2.2/dependencies)) |

- **Verdict:** tessellate polygons and arcs once, cache the meshes, and draw them with MSAA. Fine for the rare complex shapes. Do not use it for 1M wires.

### 4. femtovg

| Field | Value |
|---|---|
| Version | 0.27.0, 2026-08-31; 3.0M downloads ([CIO](https://crates.io/api/v1/crates/femtovg)) |
| Backends | `glow ^0.18` (OpenGL ES 3 / WebGL2) and optional `wgpu ^30` ([deps](https://crates.io/api/v1/crates/femtovg/0.27.0/dependencies)). `WGPURenderer::new(device: Device, queue: Queue)` accepts our device ([docs.rs](https://docs.rs/femtovg/0.27.0/femtovg/renderer/struct.WGPURenderer.html)) |
| Text | swash-based glyph atlas ([repo](https://github.com/femtovg/femtovg)). `fill_glyph_run(font_id, normalized_coords, glyphs, paint)` takes pre-shaped glyphs ([docs.rs](https://docs.rs/femtovg/0.27.0/femtovg/struct.Canvas.html)) |
| AA | NanoVG port; README only says "antialiased" ([repo](https://github.com/femtovg/femtovg)) |
| Maintenance | 949 stars, 52 open issues; used by Slint ([repo](https://github.com/femtovg/femtovg)) |
| Perf / size | no published many-primitive numbers. Slint's femtovg hello-world is about 3.5 MB ([slint#9570](https://github.com/slint-ui/slint/discussions/9570)) |

- **Verdict:** it meets the wgpu 30 and WebGL2 constraints, but its immediate path API is CPU-bound at 100k+ primitives (inferred from the NanoVG design; no numbers published). Second choice to a custom instanced renderer.

### 5. tiny-skia

| Field | Value |
|---|---|
| Version | 0.12.0, 2026-02-02; 59.7M downloads ([CIO](https://crates.io/api/v1/crates/tiny-skia)) |
| Maintenance | now under linebender: 1.7k stars, 319 commits ([repo](https://github.com/linebender/tiny-skia)) |
| Perf | "20-100% slower than Skia on x86-64 and about 100-300% slower on ARM"; often faster than cairo and raqote ([repo](https://github.com/linebender/tiny-skia)) |
| Size | about 200 KB versus Skia's 3-8 MB ([repo](https://github.com/linebender/tiny-skia)) |
| Safety | SIMD intrinsics are `unsafe`; "All pixels access is bound-checked". 148 `unsafe` tokens in 0.11.4 src (local grep) |
| Text | none ([repo](https://github.com/linebender/tiny-skia)) |

- **Verdict:** CPU raster plus upload cannot hold 120 fps at 100k primitives at 4K. vello_cpu already covers the golden-test role.

### 6. skia-safe

| Field | Value |
|---|---|
| Version | 0.153.3, 2026-09-04 ([CIO](https://crates.io/api/v1/crates/skia-safe)) |
| Build | downloads prebuilt binaries with `curl` from [rust-skia/skia-binaries releases](https://github.com/rust-skia/skia-binaries/releases). A source build needs LLVM, Python 3 and Ninja (plus Xcode CLT or VS 2022 build tools). Linux binaries need glibc ≥ 2.31 ([repo](https://github.com/rust-skia/rust-skia)) |
| wasm | "Only wasm32-unknown-emscripten"; wasm32-unknown-unknown is not supported ([repo](https://github.com/rust-skia/rust-skia)) |
| Size | Slint hello world: about 23 MB with Skia versus about 3.5 MB without; maintainer: "~23MB for Skia is well within the expected parameters" ([slint#9570](https://github.com/slint-ui/slint/discussions/9570)) |
| Maintenance | 1.8k stars, 42 open issues ([repo](https://github.com/rust-skia/rust-skia)) |

- **Verdict:** rejected. It breaks the 10 MB binary limit, cannot build for our wasm target, cannot share our wgpu device, and pulls in C++ FFI.

### 7. forma

- Archived 2024-07-18, read-only, "experimental", "not an officially supported Google product" ([repo](https://github.com/google/forma)). Last crate release 0.1.3 on 2022-12-16 ([CIO](https://crates.io/api/v1/crates/forma-render)). **Verdict:** dead.

### 8. anyrender / Blitz

- A 2D drawing abstraction with three traits: `PaintScene`, `WindowRenderer`, `ImageRenderer`. Backends: `anyrender_vello`, `anyrender_vello_cpu`, `anyrender_vello_hybrid` (which uses vello_gpu) and `anyrender_skia`. `blitz-paint` (HTML/CSS) sits on top. 116 stars, 12 open issues ([repo](https://github.com/dioxuslabs/anyrender)).
- anyrender 0.14.0 and anyrender_vello 0.15.0 were released 2026-10-03; anyrender_vello needs `vello ^0.11` and `wgpu ^30` ([CIO](https://crates.io/api/v1/crates/anyrender), [deps](https://crates.io/api/v1/crates/anyrender_vello/0.15.0/dependencies)).
- **Verdict:** a trait layer over the same vello renderers. No new capability for us.

### 9. iced_wgpu

| Field | Value |
|---|---|
| Version | 0.14.0, 2025-12-07, built on `wgpu ^27` and `cryoglyph ^0.1` ([deps](https://crates.io/api/v1/crates/iced_wgpu/0.14.0/dependencies)). master (0.15.0-dev) uses `wgpu "30"`, cryoglyph from git, `lyon "1.0"` and a cosmic-text fork from git ([Cargo.toml](https://raw.githubusercontent.com/iced-rs/iced/master/Cargo.toml)) |
| Quads | instanced; per-instance attributes; `rounded_box_sdf()`; AA via `clamp(0.5-dist, 0.0, 1.0)`, a 1-px feather ([solid.wgsl](https://raw.githubusercontent.com/iced-rs/iced/master/wgpu/src/shader/quad/solid.wgsl)) |
| Meshes | lyon tessellation behind the `geometry` feature. Optional MSAA pipeline (`msaa::Pipeline`, `sample_count`). Cached meshes are re-uploaded only when `upload.version != cache.version()` or the transform changes; entries are dropped once their `Arc` count falls to 1 ([triangle.rs](https://raw.githubusercontent.com/iced-rs/iced/master/wgpu/src/triangle.rs)) |
| Text | cryoglyph, "A fork of glyphon, mainly used by iced" ([repo](https://github.com/iced-rs/cryoglyph)) |
| Web | `webgl` feature ([wgpu/Cargo.toml](https://raw.githubusercontent.com/iced-rs/iced/master/wgpu/Cargo.toml)) |
| Numbers | none published that I found |

- **Verdict:** a good template for UI widgets (instanced SDF quads, cached meshes). Not useful as a dependency because it is tied to iced's types.

### 10. gpui (zed-industries/zed `crates/gpui`)

- Scene primitives: `Quad`, `Shadow`, `Underline`, `Path`, `MonochromeSprite`, `SubpixelSprite`, `PolychromeSprite`, `Surface`. Batching sorts by `(order, kind)`, and sprite batches split on `texture_id`. `Path` holds `Vec<PathVertex>` with xy and st coordinates plus content masks ([scene.rs](https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/src/scene.rs)).
- Rendering: SDF rects and rounded rects; shadows use Evan Wallace's closed-form erf blur; text is OS-shaped, alpha-only rasterized, cached per subpixel variant and bin-packed into an atlas, then drawn with instanced calls. Target: 120 fps, 8.33 ms ([zed blog](https://zed.dev/blog/videogame)).
- Backends: Linux moved from blade to wgpu in February 2026; macOS and Windows keep their native renderers ([PR #46758](https://github.com/zed-industries/zed/pull/46758)). The crates.io release 0.2.2 (2025-10-22) still depends on `blade-graphics ^0.7` ([deps](https://crates.io/api/v1/crates/gpui/0.2.2/dependencies)).
- **Verdict:** not a standalone renderer and has no web target. Its scene and atlas design is the model to copy for UI and text.

### 11. Other approaches

#### Rerun re_renderer (most relevant to wires and pins)

| Field | Value |
|---|---|
| Version | 0.40.0, 2026-10-21, `wgpu ^30` ([CIO](https://crates.io/api/v1/crates/re_renderer), [deps](https://crates.io/api/v1/crates/re_renderer/0.40.0/dependencies)) |
| Line design | "all quads are rendered in a single draw call"; only positions and per-strip config are uploaded; quads are spanned in the vertex shader. Instancing is avoided because "GPUs are notoriously bad at processing instances with a small batch size", so it issues one un-instanced triangle list indexed by `vertex_idx / 6`, with degenerate quads between strips. "All data we fetch in the vertex shader is uploaded as textures in order to maintain WebGL compatibility" (a position data texture plus a line-strip info texture). Joints are overlapping quads with cut-outs in the fragment shader; caps are triangles taken from spare quads (`src/renderer/lines.rs` module doc, crate 0.40.0) |
| Points | "Points are rendered as quads and stenciled out by a fragment shader ... no vertex buffer!"; data lives in textures for WebGL (`src/renderer/point_cloud.rs`) |
| AA | `compute_coverage` "centers a one-pixel antialiasing feather on the line's border": `coverage = 1 - saturate((sd + half_px)/px)` (`shader/lines.wgsl:285-357`) |
| Perf | Issue #1136 is still open. It started at "~two million points ... terrible frame rate at 1 million" with per-frame upload. 0.8.2 → 0.9.0 went from ~47 to ~15.3 ms/frame; "<10mio gives interactive framerates" on an M1 Max (2025); a 2026 draft PR caches static points and goes from 28.4 to 8.9 ms at 10M points ([#1136](https://github.com/rerun-io/rerun/issues/1136)) |
| unsafe | workspace lint `unsafe_code = "deny"` (`Cargo.toml:429`) |

- Takeaway: one draw call, vertex-pulled quads, data in textures for WebGL2, analytic 1-px coverage, and no per-frame re-upload of static data. This combination is how re_renderer reaches millions of primitives at interactive rates.

#### egui epaint

- 0.37.0, 2026-10-15; egui-wgpu is on `wgpu ^30.0` ([CIO](https://crates.io/api/v1/crates/epaint), [deps](https://crates.io/api/v1/crates/egui-wgpu/0.37.0/dependencies)).
- AA via feathering: "making each edge into a thin gradient into transparency", default 1.0 physical px; it "requires more triangles and is therefore slower" (`src/tessellator.rs:667-683`, crate 0.37.0). So it works without MSAA, but the work is CPU tessellation every frame.
- criterion benches include `tessellate_circles_100k` and a set of line benches (`benches/benchmark.rs`); no published results. Lint `unsafe_code = "deny"`. Text goes through skrifa (Cargo.toml).

#### Bevy gizmos

- bevy_gizmos_render 0.20.0 ([CIO](https://crates.io/api/v1/crates/bevy_gizmos_render)): instanced line segments with per-instance `position_a/position_b/color_a/color_b` (`VertexStepMode::Instance`, `src/lib.rs:527`) and corners from `vertex_index`, "based on https://wwwtyro.net/2019/11/18/instanced-lines.html". The fragment shader has no coverage AA (`alpha = 1`), so it relies on MSAA (`src/lines.wesl`).

#### Makepad

- Own platform layer targeting "wasm/webGL, osx/metal, windows/dx11 linux/opengl", not wgpu. 7.2k stars ([repo](https://github.com/makepad/makepad)). makepad-platform 1.0.0 was released 2025-05-13 ([CIO](https://crates.io/api/v1/crates/makepad-platform)). **Verdict:** a whole framework, not a renderer we can embed.

### wgpu WebGL2 limits and wasm size

- `Limits::downlevel_webgl2_defaults()` in wgpu-types 30.0.1 (`src/limits.rs:644`):
  - `max_storage_buffers_per_shader_stage: 0`, `max_storage_textures_per_shader_stage: 0`, all compute limits 0;
  - `max_uniform_buffers_per_shader_stage: 11`, `max_vertex_buffer_array_stride: 255`, `max_inter_stage_shader_variables: 15`;
  - inherited from `downlevel_defaults`: `max_texture_dimension_2d: 2048`, 16 KiB uniform binding.
- `DownlevelFlags`: `COMPUTE_SHADERS` ("WebGL2, and GLES3.0 devices do not support compute"), `VERTEX_STORAGE` ("storage buffers in vertex shaders"), `INDIRECT_EXECUTION` (absent on WebGL2), `BASE_VERTEX` (OpenGL 3.2+ / ES 3.2) (`limits.rs:1100-1140`).
- Consequence for a WebGL2 fallback: feed per-instance data through `VertexStepMode::Instance` vertex buffers, or through data textures as re_renderer does. Keep instance strides ≤ 255 B and the atlas ≤ 2048².
- The GLES HAL emulates `first_instance` by rebinding instance-stepped vertex buffers at an offset (`wgpu-hal-30.0.1/src/gles/command.rs:134-219`), so instanced draws with non-zero first instance do work.
- The wgpu wiki still calls the WebGL2 backend "still experimental and is missing many features" ([wiki](https://github.com/gfx-rs/wgpu/wiki/Running-on-the-Web-with-WebGPU-and-WebGL)).
- Wasm size:
  - A third-party measurement (2026-10-19) puts wgpu with the WebGL2 fallback at 1.77 MB raw / 561 KB brotli, and a WebGPU-only build (`default-features=false, ["webgpu","wgsl"]`) at 37 KB brotli. The GL backend accounts for about 1.7 MB raw ([openagents#12237](https://github.com/OpenAgentsInc/openagents/issues/12237); not a wgpu-team number).
  - wgpu's own records are qualitative. Naga is named as a "major contributor to size", and size "will change significantly depending on whether you're targeting the WebGPU backend, the WebGL backend, or both" ([discussion #2278](https://github.com/gfx-rs/wgpu/discussions/2278)). Tracking issue #9052 (open, 2026) aims to make naga optional and says removing it "will also reduce binary size significantly" ([#9052](https://github.com/gfx-rs/wgpu/issues/9052)).
  - Implication: ~0.6 MB brotli for wgpu with both backends fits easily in the 8 MB budget.

### Overall direction (synthesis)

- Wires, pins, rects, circles and arcs: a custom instanced or vertex-pulled SDF renderer (re_renderer, iced, gpui pattern) with a 1-px coverage feather. Upload static data once and change only a view uniform on pan/zoom. This keeps 0% CPU when idle and puts no per-frame CPU cost on the pan/zoom path.
- Polygons and rare complex paths: lyon tessellated once and cached, drawn with MSAA or an epaint-style feather.
- Text: parley glyph runs → swash alpha masks → our etagere atlas → instanced quads. KaTeX/RaTeX glyphs go through the same path.
- Goldens: keep vello_cpu with `f32_pipeline` for CPU-exact PNGs. Note that GPU output is not bit-exact across drivers, so GPU tests need tolerance or must use the CPU path.
- If a general vector renderer is still wanted, vello_gpu 0.3 (wgpu 30 + WebGL2) is the one to try. vello 0.11 compute cannot run on WebGL2.

