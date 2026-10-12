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
| idle CPU 0% | no wake-up once the zoom settles (asserted by the frame benchmark) |

## What makes a frame cheap

- **Tiles** (`canvas::draw_cached`, `tiles.rs`): the canvas (background, grid, primitives,
  decor, selection marks) is recorded as world-anchored 256-px tiles in one atlas layer. A
  whole-pixel pan copies the tiles it has and records the ones it uncovers (plus a 1-tile
  ring ahead); a zoom stretches the old tiles until 120 ms after the last notch, then records
  the new zoom's tiles within a per-frame cost budget (`COST_PER_FRAME`), the old ones
  standing in meanwhile. A tile the cost estimate (the old cells over the same world) says is
  too costly is recorded in up to 16 parts over several frames.
- **Index and level of detail** (`PrimIndex`, `draw_prims_in`): a tile reads only the rows
  its world box meets (a CSR grid, about 16 rows per cell); rows ≤ 2 px on screen are drawn
  as the whole pixels they touch (per-layer bit masks, one rect per run) instead of paths.
- **Vello-free frames** (`render::Layers`): when nothing outside the layers changed (the
  frame log's hash), the window blits the last target and composes the layers as textured
  quads instead of running vello.
- **Patches** (`canvas::CanvasDiff`, `Painter::patch_layer`): after an edit or a selection,
  the rows that changed (a Myers diff of per-row hashes) give world boxes; only those parts
  of the visible tiles are redrawn, on vello's tile grid. A frame's patches and new tiles are
  rendered in one vello pass into a scratch texture and copied into the atlas.
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
`docs/BACKLOG.md` (Performance) for the next steps.

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
| Split (D39): the core links no GPU, window, text-shaping, accessibility, LaTeX or plugin-host crate | (the editor half is gone) | — | `crates/cli/tests/deps.rs` checks the resolved graph for every target |
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
