# Render API: shader layout and a package-able Painter

Design note, 2026-10-25. Branch `refactor/gui-crates` at `f53edc01`. Nothing here is applied:
`gpu.rs`, `gpu.wgsl`, `ui.wgsl`, `batch.rs` and `painter.rs` are held by two agents of another
session (`gpu-step45`: vello dependency removal and the WebGL2 tier, steps 4 and 5 of
[render-backend.md](render-backend.md); `ui-retained`: diff-patched UI instance rows). This
note is applied after both merge, as part of the renderer's extraction into a Schemify-free
crate, which the user has not approved yet.

## Status (2026-10-25, end of day)

Applied on `refactor/gui-crates`, one commit each:

| Commit | What |
|---|---|
| `4a13b3e9`, `1a349ab9` | retained UI rows (taken over from `ui-retained`; typing p95 checked: no regression) |
| `8491b24f` | § 1: the shaders split into eleven files |
| `5742016f` | § 2: the canvas drawn by plain calls (`canvas_begin` … `canvas_end`); source handles and `draw_in` wait for a second caller |
| `907c2fec` | § 4: the native GPU ladder, a lost device reopened, a total failure reported (`RunError::NoGpu`) |
| `8c7d409a` | § 4: `+simd128` in the web app build only |
| `c3ff2697` | Dependencies: `render::block_on` replaces `pollster` |
| `61c60ca8` | Dependencies: `cargo update` (54 bumps) |

Open: ratex `default-features = false` (waits for erweixin/RaTeX#178 to be released); the
pack restructure (§ 5); step 6 of render-backend.md (buckets and far-zoom detail for 1M).

## Answer in short

- **Split the two shaders into eleven files by what they compute**, concatenated per shader
  module with `concat!(include_str!(…))`. No new dependency, no WGSL logic change, so every
  golden and GPU-vs-CPU oracle stays as it is. Three helpers are duplicated verbatim between
  the two files today (`corner`, `color_of`, `half_plane_area`); the split leaves one copy.
- **Give the renderer four tiers**, each reachable from the one above by replacing one call:
  the canvas tables (data), the retained rows and pipelines, the immediate `Painter`, and a
  convenience call. Three couplings in today's API block that: the `texts` callback in
  `Painter::canvas`, the fixed `SOURCES = 6`, and `Pipelines::draw` opening its own render
  pass.
- **The CPU fallback has two holes** (§ 4): a native window without a GPU stays blank, and
  the wasm build has no `+simd128`, so `vello_cpu` runs scalar in exactly the browsers that
  fall back to it.
- **SIMD is not the pack's bottleneck** (§ 5): the 100k whole pack runs at about 0.5 GB/s
  because each row's pen is computed twice through branches. Restructure first. The one
  textbook SIMD loop in the pipeline, waveform min/max, is already vectorized.
- The `vello` dependency removal is `gpu-step45`'s and is not repeated here.

## 1. The shader split

### Data

1. In: two WGSL files, `gpu.wgsl` (572 lines, the canvas: one camera uniform, five
   pipelines) and `ui.wgsl` (174 lines, the frame batch: one pipeline that branches on the
   instance kind). Out: one WGSL string per shader module, the same two modules as today.
2. Two modules, eleven files.
3. Not applicable (source text).
4. Read once at pipeline creation (`Pipelines::new`), at startup.
5. `'static` strings in the binary.
6. Not applicable.

### Files

A file holds one thing a shader computes. WGSL module-scope declarations are
order-independent, so concatenation order only matters to the reader: shared files first.

| File | Holds | Module |
|---|---|---|
| `shaders/instance.wgsl` | `corner` (the six vertices of an instance quad), `color_of` (RGBA8 in memory order to straight alpha) | canvas, batch |
| `shaders/area.wgsl` | `half_plane_area`: the exact share of a pixel on one side of a line | canvas, batch |
| `shaders/canvas/camera.wgsl` | `Cam` and its binding, `dev`, `to_screen` (exact integer subtraction), `to_clip`, `snap`, `SNAP_BIAS`, the far-zoom cut (`LOD_PX`, `lod_small`, `lod_box`), `degenerate` | canvas |
| `shaders/canvas/style.wgsl` | the row style word: `FILL`, `PARITY`, `width_px`, `dash_px`, `flags`, `snap_offset`, `shade` | canvas |
| `shaders/canvas/distance.wgsl` | `coverage` (the distance ramp), `sd_box`, `dash_near` | canvas |
| `shaders/canvas/background.wgsl` | `vs_bg`, `fs_bg`: background and grid dots | canvas |
| `shaders/canvas/segment.wgsl` | `SegIn`, `vs_seg`, `fs_seg`: lines, wires, polyline and polygon edges | canvas |
| `shaders/canvas/rect.wgsl` | `vs_rect`, `fs_rect`, `perimeter` (`SegIn` from `segment.wgsl`) | canvas |
| `shaders/canvas/arc.wgsl` | `ArcIn`, `vs_arc`, `fs_arc`, `arc_distance` | canvas |
| `shaders/canvas/fill.wgsl` | `TriIn`, `vs_tri`, `fs_tri`: polygon fill triangles | canvas |
| `shaders/batch.wgsl` | `Ui`, its bindings, the instance kinds, `box_cov`, `seg_cov`, `vs_ui`, `fs_ui` | batch |

`batch.wgsl` stays one file: it is one pipeline, and its kinds share one vertex and one
fragment entry point. It splits only if the batch becomes one pipeline per kind.

Each file opens with a header in the form the Rust modules use: what it computes, its in and
out, the Rust items it mirrors (`gpu::STRIDE`, `gpu::FILL`, `batch::RECT`, …), and the WebGL2
limit it respects where one applies. Each function keeps a `//` comment stating what the
signature cannot (units, exactness, which CPU function it matches).

### Loading

```rust
/// The canvas shader module: shared helpers, then one file per pipeline.
const CANVAS_WGSL: &str = concat!(
    include_str!("shaders/instance.wgsl"),
    include_str!("shaders/area.wgsl"),
    include_str!("shaders/canvas/camera.wgsl"),
    include_str!("shaders/canvas/style.wgsl"),
    include_str!("shaders/canvas/distance.wgsl"),
    include_str!("shaders/canvas/background.wgsl"),
    include_str!("shaders/canvas/segment.wgsl"),
    include_str!("shaders/canvas/rect.wgsl"),
    include_str!("shaders/canvas/arc.wgsl"),
    include_str!("shaders/canvas/fill.wgsl"),
);
```

`BATCH_WGSL` is the same with `instance.wgsl`, `area.wgsl` and `batch.wgsl`. A helper
defined twice is a naga error at module creation, so a duplicate cannot drift silently. The
one cost: naga reports line numbers in the concatenated string, not per file. The research
appendix weighs WESL and naga_oil against this.

### Oracle and change radius

- **Oracle**: a headless test parses and validates both concatenated modules through
  `wgpu::naga` (wgpu 30 re-exports naga) and asserts every entry point `Pipelines::new` names
  exists. If `gpu-step45` lands naga validation tests for WebGL2, they cover this with no
  change. The GPU-vs-CPU tests in `render/tests/gpu.rs` stay the pixel oracle.
- **Change radius**: 2 bodies (the two `create_shader_module` sites in `Pipelines::new`),
  plus doc comments that name `gpu.wgsl` or `ui.wgsl` (`gpu.rs`, `batch.rs`,
  architecture.md).

## 2. The API in tiers

### Usage code first

How `app::paint` draws the canvas today (`crates/view/app/src/paint.rs:210`):

```rust
let texts = |p: &mut P, k: usize| { /* labels of table k, culled by zoom and PrimIndex */ };
p.canvas(&frame, tables, (cp.scene, marks, marks_changed), palette, texts);
if !cp.overlay.is_empty() {
    p.overlay(view, &cam, cp.overlay, palette, text, scratch);
}
```

How it should read:

```rust
p.canvas_begin(&frame);                              // background, grid, clip to the view
for (k, (table, change)) in tables.into_iter().enumerate() {
    p.canvas_rows(sources[k], table, palette, change);
    if labels_visible(k) {
        canvas::draw_texts(p, view, &cam, &table.texts, listed(k), palette, text, scratch);
    }
}
p.canvas_marks(sources.marks, cp.scene, marks, marks_changed);
p.canvas_rows(sources.overlay, cp.overlay, palette, Change::All);
p.canvas_end();                                      // pops the clip
```

The caller keeps its scope (no closure capturing seven locals), decides per table whether
labels draw, and can interleave anything else between two tables. The overlay becomes an
ordinary source, so `Painter::overlay` goes.

### The tiers

| Tier | What | Today | Change |
|---|---|---|---|
| 0, data | `CanvasPrims` SoA tables, `Camera`, `Palette`, `CanvasDiff` (`schemify-canvas`, a leaf on kurbo alone) | public fields, no GPU | none |
| 1, retained | `InstanceRows::pack(src, table, palette, change)`, `Pipelines::upload`, `Pipelines::draw` | sources are `usize` below `SOURCES = 6` (four Schemify tables, marks, overlay); `draw` begins its own render pass | sources become handles the caller allocates (`InstanceRows::add_source() -> Source`, capacity asserted); add `Pipelines::draw_in(&self, pass: &mut wgpu::RenderPass, …)`, with `draw` kept as the call that opens the pass and calls it |
| 2, immediate | `Painter`: `fill_rect`, `fill_path`, `stroke_path`, `stroke_styled`, `glyphs`, `push_clip`, `pop_clip`, `image`, and the canvas calls | `canvas` takes a callback | `canvas_begin`, `canvas_rows`, `canvas_marks`, `canvas_end` as above |
| 3, diagonal | `canvas::draw_canvas` (one call: tables to pixels), the window's `run` | as today | optional: a `PaintScene` impl behind a feature (research appendix) |

Every retained construct has an immediate twin taking the same data: `canvas::draw_prims`
packs into scratch rows and draws them in one call, as it does today.

### The checklist against the extracted crate

| # | Question | Answer |
|---|---|---|
| 1 | Integration in ten minutes | Yes for tier 2 (`CpuPainter` needs no GPU); tier 1 needs the caller's device, queue and format |
| 2 | Each high call decomposes | `draw_canvas` → begin/rows/end; `draw` → `draw_in` (after the change) |
| 3 | Common case in one or two calls | `draw_canvas`, or `run` for a window |
| 4 | A without B | The batch draws without rows (`GpuPainter::rows: None`); rows draw without the UI. The atlas is created whether or not text is drawn (4 MiB of GPU memory) |
| 5 | Immediate twin per retained construct | Yes: `draw_prims` |
| 6 | Caller keeps flow control | No until the `texts` callback goes |
| 7 | Caller's types | kurbo, peniko and glyph runs (font, ids, positions): the community's vocabulary, and no shaper is imposed (glyphon imposes cosmic-text) |
| 8 | Transparent data | Rows are documented byte layouts (`InstanceRows::row`, `bytes`); the batch exposes `instances()` and `draws()` |
| 9 | Allocation apart from init | `Atlas::insert` takes caller coverage, so a caller can rasterize glyphs with its own rasterizer. `Batch` and `InstanceRows` grow on their own; no caller-supplied capacity |
| 10 | Source to the runtime | Yes |
| 11 | The discontinuity | Gradients, path clips and blend layers: no instance kind draws them, and the only route today is a CPU raster into an image. The wgpu major version is pinned by the public signatures. `ATLAS_SIDE` and `INSTANCES_MAX` are constants, not parameters |

### Change drills

From [backlog.md](../backlog.md) and render-backend.md § 5:

| Change | Edits | Radius |
|---|---|---|
| A new canvas primitive kind (a Bézier row) | a `KINDS` entry and `STRIDE`, an arm in `pack`, `draw_rows` and the pipeline list, a new `shaders/canvas/curve.wgsl` | 3 bodies plus a new file; the exhaustive matches list the rest |
| Buckets for 1M objects (step 6) | `pack` (bucket ranges), `draw_in` (visible ranges) | 2 |
| A second shader dialect (GLSL ES for WebGL2 through naga) | none in the WGSL files if they stay within the WebGL2 limits | 0 to 1 (`Pipelines::new`) |

## 3. Internals that stay

The tiers change the doors, not the rooms. What the extracted crate keeps as it is:

- **Two row stores, one pass.** Retained canvas rows (`InstanceRows`: 24–28 B per primitive,
  grouped by layer with free slots, patched in O(change)) and the per-frame batch (56 B
  instances in call order, rebuilt each frame, or diff-patched once `ui-retained` lands).
  Canvas stages are draws inside the batch, so a frame is one render pass.
- **Exact integer camera.** Rows stay `i32` milli-units; the shader subtracts the camera's
  integer centre before converting to `f32`, exact over the D33 range.
- **Coverage, not MSAA.** Exact pixel-area coverage for rects and stroke bands, 4 × 4
  subsamples at caps, coverage masks from `vello_cpu` for glyphs and small paths. This is why
  GPU frames match the CPU goldens within 1 level on average.
- **One rasterizer for masks and goldens.** `vello_cpu` fills the atlas and paints
  `CpuPainter`, so a mask is the golden's own pixels.

## 4. When the CPU rasterizes and when the GPU does

As of `f53edc01` (`window.rs`, `cpu_view.rs`, `app/src/paint.rs`, web spec §7):

| Situation | Who makes the pixels | What the CPU still rasterizes |
|---|---|---|
| Native window, adapter and device found | the GPU, one render pass | coverage masks only: glyphs, icons, check marks, math delimiters, strokes whose box is ≤ 96 px, fills (in 512 px tiles), by `vello_cpu` into the atlas, once per key; at most 32 exact glyph masks a frame during a zoom |
| Native window, no adapter or device | **nothing: the window stays blank**. `Gpu::new` fails, `eprintln!("gpu: …")`, and `redraw` returns early (`window.rs:375`) | — |
| Native window, device lost | **not handled**: the lost callback is set only on wasm | — |
| Web, WebGPU usable | the GPU, as natively | the same masks |
| Web, WebGPU unusable (no adapter or device, device lost, or the first frame never reaches the page) | `CpuPainter`: a full `vello_cpu` raster per frame, `putImageData` on an overlay canvas | everything |
| Web, WebGL2 (in progress, `gpu-step45`) | the GPU through wgpu's GLES backend, tried before the CPU | the same masks |
| PNG export (`Editor::take_raster` → `paint::raster_png`) | `CpuPainter` always, so an export is the same bytes on every machine | everything |
| Goldens and render tests | `CpuPainter`; GPU frames are checked against it within a tolerance | everything |
| Headless (`schemify cmd`, JSON-RPC) | nothing, unless a PNG export is requested | — |

So "full on the GPU" means the GPU fills every pixel of the frame, but glyphs and small paths
were rasterized on the CPU once, the first time they were drawn. "CPU rasterization" as a
mode means `CpuPainter` paints the whole frame.

### Making the CPU fallback solid

Ranked by what a user without a working GPU sees:

1. **A native fallback ladder** (the blank window above), agreed with the `schemify-19`
   session on 2026-10-25: a hardware adapter, then wgpu's software adapter
   (`force_fallback_adapter: true`: WARP on Windows, lavapipe or llvmpipe on Linux where Mesa
   installed it; macOS always has Metal). A device-lost callback natively, as on the web. A
   total failure is visible: a clear error and exit, not a blank window. Change radius: 3
   (`Gpu::new`'s caller in `resumed`, `redraw`, `user_event`'s `NoGpu` arm).
   **Deferred**: `CpuPainter` presented through `softbuffer` as a third rung. WARP and
   lavapipe cover the realistic machines without a GPU, and a new dependency waits until a
   machine where both fail is known (a backlog line with this reason).
2. **`+simd128` for the web app build only.** No build sets it (CI's `wasm` job,
   `release.yml`), so `vello_cpu`'s SIMD kernels run scalar in the browser, exactly where the
   CPU fallback runs. **Not** a target-wide `[target.wasm32-unknown-unknown] rustflags` in
   `.cargo/config.toml`: the plugin guests (`ui/build.sh`: the default UI and the test guests)
   build for the same target and run under wasmi, so the flag would need wasmi's SIMD support
   and change the committed `.wasm` files CI's `guests` job compares. Set it in the web
   build's own command (the `wasm` job, `release.yml`, the web build script). Every browser
   with WebGPU has supported wasm SIMD since 2021–2023 (Chrome 91, Firefox 89, Safari 16.4).
   Oracles: the guests rebuild byte-identical; the smoke test's golden match is unchanged.
   Benchmark: its CPU frame time before and after.
3. **Damage, not full frames.** `cpu_view` rasterizes the whole 1280×800 frame for a hover
   that changed the status line. Once `ui-retained` lands, its diff names the changed UI rows,
   and `CanvasDiff` names the changed canvas rows; their union of boxes is the damage. Keep
   the previous pixmap, `push_clip(damage)`, and paint only what intersects it. A pan or zoom
   still damages the canvas whole.
4. **Threads natively.** `vello_cpu`'s `multithreading` feature (rayon) for the native
   fallback only: the web has no threads without COOP/COEP headers, and goldens stay
   single-threaded so their bytes never depend on the core count.
5. **A forced CPU mode** (`SCHEMIFY_GPU=cpu`, read where `WGPU_ADAPTER_NAME` is read) so the
   native fallback is testable on a machine with a GPU, plus a test that the forced mode
   presents frames. The web smoke test already covers the web fallback in CI (SwiftShader
   shows no frames).
6. **Measure first.** perf.md has no CPU-fallback frame time. Record the 1280×800
   `editor_dark` frame and a pan at 100k through `CpuPainter` before items 2–4, so each one
   lands with its number.

## 5. SIMD on the CPU side

The GPU does the per-pixel work. The CPU loops that touch every row are few. Triage per
`simd-loops`, from the code at `f53edc01`:

| Loop | Where | Size | Dependency | Verdict |
|---|---|---|---|---|
| Waveform min/max per pixel column | `wave/src/decimate.rs` `minmax` | 10^6 samples | accumulator | **already vectorized** (8 lane accumulators over `chunks_exact`, `minpd`/`maxpd` checked in the asm, scalar oracle kept) |
| Whole-table pack | `gpu.rs` `repack` | 10^5–10^6 rows, on open, palette change, `Change::All` | scatter by layer (each row's slot is `start[l] + used[l]++`) | **restructure first**, see below; the scatter is an output-shape wall |
| Row hashing for the diff | `canvas/src/damage.rs` `fold`/`mix` | 10^5 rows, ~1 ms | a chain within a row, none across rows | lanes across rows exist for the fixed-width kinds, but `mix` is a 64-bit multiply, which AVX2 has no packed form of (AVX-512 does). Scalar until a measured need, as the module's own triage says |
| Mask alpha extraction | `batch.rs` `Raster::run` (`pix.data().iter().map(\|p\| p.a)`) | ≤ 96² px a mask, ~32 masks a frame | none | autovectorizes as a deinterleave; the mask's cost is `vello_cpu`'s raster (8–12 µs). Not worth hand SIMD |
| Largest label size | `app/src/paint.rs` (`texts.size…max()`) | 10^5, once per generation | accumulator | integer max: autovectorizes, and it is cached by generation |
| Visible bucket ranges (step 6, not built) | future `draw_in` | 10^3–10^4 buckets | none | an SoA box-vs-view compare to a bitmask: the textbook shape, `chunks_exact` and autovectorization. At 10^4 buckets it is microseconds even scalar, so write it plain and probe |
| CPU culling in `draw_rows` (the web's no-GPU path) | `canvas.rs` | 10^5 rows a frame | none | the one per-frame 10^5 loop on the CPU; cull on the SoA tables (`CanvasPrims`), not on the AoS rows, so the compare runs over contiguous `i32` columns |

### The pack: 0.5 GB/s where memory allows 20

perf.md measures the 100k pack at 4.8–6.7 ms for 2.9 MB of rows. That frame (open, palette
change, a diff that gave up) misses the 1 ms interactive budget by 5×. The cost is not
arithmetic that lanes would help:

1. `one_row` runs twice per row (the counting pass and the writing pass), and each run calls
   `pen`: two palette loads, an `f64` multiply and round, three branches.
2. `row` writes each field through a bounds-checked 4-byte slice copy.

The fix, in order (contributing.md step 6: delete the branch first):

1. **A pen table per pack.** `pen` depends only on (layer, style byte, closed): at most 32 ×
   128 × 2 entries. Build only the entries the table uses, or the whole table (8192 calls;
   not measured, but far below the 10^5 calls it replaces) when the palette changed, which is when a whole pack happens anyway.
2. **One pass computes a `u8` key column** (layer, or `LAYER_DEAD` for a row that draws
   nothing) and the per-layer histogram; the second pass writes rows from the key column and
   the pen table, with no call to `pen`.
3. **Fixed-size row writes**: a `[u32; 6]` or `[u32; 7]` converted with `to_ne_bytes` into a
   `chunks_exact_mut(STRIDE)` row, so the bounds check is hoisted and the store is two
   16-byte moves.
4. Only then, if the scatter still dominates: when a layer's rows are contiguous in the source
   (one run), the write is a plain SoA-to-AoS transpose, which autovectorizes. Detect runs
   from the key column.

Oracle: the existing `a_kept_canvas_equals_a_fresh_pack` and the pack tests compare patched
against fresh rows; add the old `repack` as the scalar oracle for the new one over random
tables, byte for byte. Benchmark: a headless pack of `perf.rs` `schematic(100_000)`, before
and after, against a 1 ms target.

### Target features

`.cargo/config.toml` sets no `target-cpu`, so x86-64 builds vectorize to SSE2 (2 × f64, 4 ×
i32 lanes). The choices:

- **Keep the baseline and dispatch at run time** for the few kernels that measure a gain
  (`fearless_simd`, per the `rust` skill). Distributed binaries keep running everywhere.
- **`-C target-cpu=x86-64-v3`** for everyone doubles lane width for free, but excludes CPUs
  without AVX2. It does not change float results: Rust never contracts into FMA on its own,
  so rule 4 (determinism) holds.
- **wasm**: `-C target-feature=+simd128` in the web app build only (§ 4: the plugin guests share the target and run under wasmi).

Recommendation: the baseline plus run-time dispatch for kernels with a benchmark, and
`+simd128` on wasm.

## 6. Room for improvement, ranked

| # | What | Why | Owner (today) |
|---|---|---|---|
| 1 | The pack restructure (§ 5) | 6.7 → under 1 ms on open and palette change | `render::gpu` |
| 2 | Split the shaders (§ 1) | readable, one copy of each helper | `render` |
| 3 | `texts` callback → begin/rows/end; caller-allocated sources; `draw_in` | flow control, and the precondition for extraction | `render`, `app::paint` |
| 4 | Buckets and the far-zoom overview (step 6) | 1M objects on the iGPU (69 ms today) | `render::gpu` |
| 5 | Extract the crate (`canvas` tables + `render` core, no `ui`/`wave`) | reuse outside Schemify | after `gpu-step45` and `ui-retained` |
| 6 | A `PaintScene` impl behind a feature | other Rust UIs can draw through it | only on a user's request: it needs gradients, path clips, blend layers and box shadows, which no instance kind draws |
| 7 | Precompiled shaders, no naga at run time | binary size | waits on wgpu #9052 |
| 8 | WESL instead of `concat!` | imports and conditional code | only if `#ifdef`-style variants appear |

## Research appendix

Gathered 2026-10-25.

### Splitting WGSL

- WGSL has no imports. `import` is only a reserved word in the spec
  ([WGSL](https://www.w3.org/TR/WGSL/)); namespaces are a proposal
  ([gpuweb#7310](https://github.com/gpuweb/gpuweb/pull/7310)). naga 30 parses plain WGSL.
- **WESL** (`wesl` 0.6.0, 2026-10-04): a build-dependency. `build.rs` runs
  `Compiler::default()…compile()` and `write_artifact`; `include_wesl!` embeds the linked
  WGSL. Its own parser, no naga; 0 bytes in the binary. The `Wesl` struct of older examples
  no longer exists ([docs.rs](https://docs.rs/wesl/0.6.0/wesl/),
  [crates.io](https://crates.io/api/v1/crates/wesl/0.6.0/dependencies)).
- **naga_oil** 0.23.0: archived 2026-10-21; Bevy moved to WESL
  ([repo](https://github.com/bevyengine/naga_oil)). Rejected.
- **`concat!(include_str!(…))`**: no dependency. One shared scope, so a name defined twice
  fails at module creation; naga's line numbers are in the joined string (a table of each
  file's first line maps them back); no conditional code.

### A standard 2D trait

- **anyrender 0.14.0** `PaintScene` (kurbo 0.13, peniko 0.6): `reset`, `push_layer` (fill
  rule, blend mode, alpha, transform, any clip shape, filters), `push_clip_layer`,
  `pop_layer`, `stroke` and `fill` (any shape, brush with its own transform), `draw_glyphs`,
  `draw_box_shadow`; not object-safe
  ([source](https://github.com/dioxuslabs/anyrender/blob/main/crates/anyrender/src/lib.rs),
  [docs.rs](https://docs.rs/anyrender/0.14.0/anyrender/trait.PaintScene.html)).
- vello_cpu 0.3's `RenderContext` (stateful: `set_paint`, `set_transform`, `fill_path`,
  layers) and vello_gpu 0.3's `Scene` share no trait; anyrender's adapter crates join them.
  The shared vocabulary is kurbo and peniko, which Schemify already speaks
  ([vello_cpu](https://docs.rs/vello_cpu/0.3.0/vello_cpu/struct.RenderContext.html)).
- piet 0.8 is in maintenance mode ([repo](https://github.com/linebender/piet)).

### Future platform support

- wgpu 30's WebGL2 limits: 16 vertex attributes, stride ≤ 255 B, 8 vertex buffers
  ([wgpu-types](https://docs.rs/wgpu-types/30.0.1/wgpu_types/struct.Limits.html)). The 56 B
  batch instance and the 24–28 B rows fit.
- `first_instance` on WebGL2 is emulated by rebinding each instance buffer at an offset
  (`wgpu-hal/src/gles/command.rs`, v30.0.1), so each per-layer draw rebinds. With about 40
  draws a frame that is cheap; with per-bucket draws (step 6) count them.
- WebGPU ships in Firefox on Windows (141) and macOS (147), not by default on Linux or
  Android; Safari 26 ships it everywhere
  ([status](https://github.com/gpuweb/gpuweb/wiki/Implementation-Status)). So the WebGL2 tier
  still matters for Linux browsers.
- Dropping naga at run time: `create_shader_module_passthrough` exists but still links naga;
  [wgpu#9052](https://github.com/gfx-rs/wgpu/issues/9052) is open.

## Dependencies (2026-10-25)

`cargo tree -p schemify-app -e normal,build` at `f53edc01`: 279 third-party packages native,
181 on wasm; 13 crates compiled in two versions. No direct dependency is unused. None of the
duplicates can be unified by a version bump on our side (`cargo update --dry-run` merges
none): each is two upstream crates asking for different majors.

| Duplicate | Old side | Fix |
|---|---|---|
| `phf` ×4 (0.11), `bit-set`, `bit-vec` (0.8) | ratex | [erweixin/RaTeX#178](https://github.com/erweixin/RaTeX/pull/178) |
| `thiserror` 1 | ratex, winit 0.30 (`calloop`, `smithay-client-toolkit`) | #178, then winit 0.31 |
| `rustix` 0.38, `linux-raw-sys` 0.4 | winit 0.30 | winit 0.31 |
| `syn` 2 | older proc-macros | upstream |
| `hashbrown` 0.16 | wgpu's `gpu-allocator` | upstream |
| `miniz_oxide` 0.8 | `png` 0.18 (and `cli`, which matches it) | upstream `png` |

Planned 2026-10-25, one commit each (items 2 and 3 are applied: `c3ff2697`, `61c60ca8`):

1. **ratex without mhchem**, once #178 is released: `default-features = false` on
   `ratex-parser` and `ratex-layout` in the root `Cargo.toml`, pinned to the release that
   carries the feature. Nothing in the repository uses
   `\ce` or `\pu`. Measured upstream: ratex-layout's tree 31 → 14 packages; in Schemify about
   13 fewer packages and 6 fewer duplicates.
2. **`pollster` → a `block_on` of our own** in `render` (a thread-parking waker, about 15
   lines of safe Rust, under 20 with a test; `window.rs` and `tests/gpu.rs` call it): 1 package.
3. **`cargo update`**: about 30 compatible patch updates (`icu` 2.3 → 2.4, `libc`, `cc`, …),
   then `cargo test --workspace --no-fail-fast`, the web smoke test and the guests check
   (`ui/build.sh` output unchanged); any single bump that changes a golden or a guest `.wasm`
   is reverted.

Kept, with the reason:

| Dependency | Packages it alone brings | Why it stays |
|---|---|---|
| `accesskit` + `accesskit_winit` | 60 (zbus and its async stack, for AT-SPI on Linux) | accessibility is not optional |
| `winit` | 34 | the window |
| `parley` | 31 (ICU segmentation) | text shaping and line breaking |
| `wgpu` | 19 | the GPU |
| `omnilua` | 8 | user config (D9) |
| `wasmi` | 6 | plugins (D3) |
| `png` | 5 | PNG decode and export |
| `vello_cpu` | 4 | CPU painter, glyph masks |
| `taffy`, `spicerack`, `kurbo`, `fontique`, `skrifa`, `libm` | 1–2 each | layout, netlist writer, geometry, system fonts, font metrics, deterministic math |

`spicerack`'s `num-complex` (1 package) could become a small complex type in SpiceRack itself;
not worth a cross-repository change alone. `vello` (4 packages) is `gpu-step45`'s.
