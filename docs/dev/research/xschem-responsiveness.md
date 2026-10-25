# Why xschem feels responsive, and what Schemify should take from it

Research note, 2026-10-24, on branch `refactor/gui-crates` at `2f17e1cb`. It runs on the
same machine as [render-backend.md](render-backend.md): river on Wayland, with the monitor
at 59.95 Hz. The xschem build is 3.4.7 from nixpkgs.

## Answer in short

- **xschem draws with the X11 core protocol into a retained back pixmap, then copies the
  pixmap to the window in a single `CopyArea`.** Every frame is a full redraw: a clear, then
  grid points, then segments batched 512 per `PolySegment` and grouped by one GC per layer
  colour, then one blit. Lines are aliased. Only text goes through cairo, as XRender
  `CompositeGlyphs` against a server-side glyph cache. It uses no OpenGL, no Present, no
  SYNC, and no pan-by-blit.
- **It never waits on anything except input.** It draws inside the event handler, and Tk
  collapses queued motion events, so each frame shows the newest pointer position. Under
  Xwayland, the frame-callback wait happens *after* the drawing, in Xwayland, which commits
  whatever the window holds at that moment. In Schemify the wait comes *before* the drawing.
- **It skips detail at far zoom.** Text disappears when it would be about 3 px tall. Below
  about 2 px an instance becomes its bounding-box outline: one `PolyRectangle` in place of
  about 30 primitives.
- **It is not faster on this machine by the numbers.** In a working view (about 150
  instances visible), input to Xwayland commit is p50 16–19 ms, p95 25–37 ms. Schemify's
  input to present is p50 20–25 ms, p95 46–53 ms. At full view of 10k instances, xschem
  takes **53–57 ms p50**, because each frame redraws all of them on the CPU path. That is
  about 18 fps, far outside D39.
- **What the hand feels is completeness and the newest input.** Each xschem frame is
  complete and correct, with no stale or blank tiles, and it always shows the latest
  position. Wheel zoom in a working view reaches the commit in 7.6 ms p50. Schemify's
  visible defects (blank tiles, and the latency of the oldest coalesced input) are the
  difference, more than the raw medians are.

## Method and clean-room compliance (D1)

Everything here is black-box observation of xschem or published prose:

- **Ran** the nixpkgs binary (`/nix/store/pxbims…-xschem-3.4.7/bin/xschem -r FILE`) with
  `HOME` set to a scratch directory, under `nice -n 19`, for 10–25 s per run.
- **X11 protocol** recorded by `xtrace` 1.4.0 (the X11 proxy from nixpkgs, not glibc's function
  tracer) placed between xschem and the server. Requests, events and realtime timestamps
  were logged, and the requests were counted per frame.
- **Syscalls** recorded with `strace -f -tt -T -e trace=…` (I/O and wait calls only, no
  symbols).
- **Wayland side.** A nested rootful `Xwayland :97 -noreset` (24.1.11, the same binary as the
  desktop's) ran with `WAYLAND_DEBUG=1`. This logs `wl_surface.commit`, the frame
  callbacks and `done`, with realtime µs mod 2^32, checked against `date`. This is
  Xwayland's own traffic, not xschem's.
- **Input** was injected with `xdotool` (XTEST): middle-drag pans of 6,4 px at about 15 ms
  (the fastest xdotool manages) or about 57 ms spacing, wheel notches every 200 ms, and
  arrow keys.
- **Headless runs** under `Xvfb :99` (21.1.x, software fb) gave the request structure
  without the GPU path.
- **Prose** came from the shipped manual (`share/doc/xschem/xschem_man/`):
  `performance.html`, the key list in `commands.html`, and the option and command lists in
  `developer_info.html`, read for behaviour and the `.sch` format. The man page was also
  read.
- **Never opened**: any `*.c`, `*.h`, `*.tcl`, `*.awk`, the xschem git repo, xschem's
  generated `~/.xschem/xschemrc`, or any web page with code. No disassembly, no symbols, no
  `perf` or `gdb`. Two pieces of outside information were used. `ldd` lists the linked
  libraries (X11, xcb-render, xcb-shm, Xrender, cairo, Tcl/Tk; no GL). General knowledge of
  Tk (motion-event collapsing) and of Xwayland (commits gated on `wl_surface.frame`) comes
  from their own documentation and behaviour.
- **Input file**: `big10000.sch` holds 10,000 instances of `devices/{nmos4,pmos4,res,capa}.sym`
  on a 200-unit grid, each with one labelled wire. A 49,729-instance file was used for one
  scaling run. Both were generated from the documented file format (`C {sym} x y rot flip
  {props}`, `N x1 y1 x2 y2 {lab=…}`).

Scripts and raw traces are in the session scratchpad, `xschem-research/`:
`run_xvfb.sh`, `run_xwl.sh`, `frames.py` (per-frame primitive counts),
`lat.py` (event → blit → commit → done), `lat2.py`, `lod.sh`, `gen.py`.

### Caveats

- **The latency is measured from the X event's arrival at xschem.** `xtrace` logs it, the
  same reference point as Schemify's synthetic input on its event loop. XTEST injection
  through Xwayland adds its own delay before the event arrives. A proxy-free run timed from
  `xdotool`'s return could not separate that delay, so it is not used for the main
  numbers. For drag, the proxy-free run gives p50 9–14 ms to commit, lower than the xtrace
  runs. So the xtrace numbers are **upper bounds** for xschem, because the proxy decodes
  0.1–0.7 MB per frame.
- A pan drag of 3,2 px per event made xschem redraw only on every other event. It looks
  like a minimum pan step. The runs above use 6,4 px.
- **The Schemify numbers are not re-measured.** They come from render-backend.md § 1, the
  same day and machine, at 100k objects and 8 ms synthetic inputs. They depend on the
  scene only through `Layers::raster`; the median is the frame-callback wait. No GPU
  benchmark was run for this note.

## Measurements

### 1. What a frame is (Xvfb and Xwayland, identical structure)

Here is one pan frame in a working view (xtrace, condensed):

```
PolyFillRectangle  back-pixmap  {-2,-2,1284,706}         clear
PolySegment        back-pixmap  2 segs (dashed)           origin axes
PolyPoint ×7       back-pixmap  ~512 pts each             grid
PolySegment ×N     back-pixmap  512 segs each, one GC per layer colour
PolyRectangle ×M   back-pixmap  pins / boxes
CreatePicture + CompositeGlyphs8/32 ×K                    cairo text via XRender, glyphs cached server-side
CopyArea           back-pixmap → window  1280×702         present
```

- **It draws through the X11 core protocol, not cairo or GL.** Lines, points and rectangles
  are core requests. Text is the only cairo path. There are no GLX, DRI or Present
  requests. The extensions queried are BIG-REQUESTS, MIT-SHM, RENDER and XKEYBOARD.
- **Backing store**: the drawing goes to a retained server-side pixmap, and a single
  `CopyArea` replaces the window's content. A partly drawn frame is never visible. The
  manual lists `draw_window` ("direct draw into window", default 0) and a Ctrl-`$` toggle
  that switches between "XCopyArea vs drawing primitives for drawing the screen".
- **There is no pan-by-blit.** Every pan frame clears and redraws the whole view. `CopyArea`
  is only pixmap → window.
- **Batching**: segments go 512 per request (4,108-byte requests) and points 512 per
  request. Bounding-box rectangles at far zoom go only 10 per request. Xlib/xcb flushes about
  33 `writev`s per frame.
- **No waiting**: strace shows one `poll(POLLOUT)` per `writev`, each returning at once.
  The only blocking calls are `poll(POLLIN)` waits for input while idle. The draw is
  synchronous in the input handler.
- **Motion coalescing**: 100 motion events 8 ms apart produced 51 frames (Xvfb, full
  view). On Xwayland, 61 motions produced 46 frames. Each frame used the newest position.
  There is no queue of stale frames.

### 2. What it skips at far zoom (Xvfb, 1280 px canvas, `xschem zoom_box`)

| View | Visible instances | Segments | Rects | Glyph runs | Frame (Xvfb) |
|---|---|---|---|---|---|
| ≈0.43 px/unit (working view) | ~150 | 3,050 | 699 | 868 | 9–14 ms |
| ≈0.28 px/unit | ~400 | 7,215 | 1,648 | **0** | 5 ms |
| ≈0.14 px/unit | ~1,300 | 29,096 | 6,630 | 0 | 17 ms |
| full view, 10k | 10,000 | 10,002 (wires only) | **10,000 bbox outlines** | 0 | 14–22 ms |
| full view, 49.7k | 49,729 | 49,731 | 49,729 | 0 | 54 ms |

- **Text** (symbol labels, size 0.2) is dropped somewhere between 0.43 and 0.28 px/unit,
  where it would be about 3 px tall.
- **A symbol** that is 1–2 px across is drawn as its bounding-box outline. In the working
  view, a fully drawn instance costs about 20 segments, 4.6 rects and 5.8 glyph runs; at
  full view it costs 1 rect plus its wire, roughly 30× fewer primitives.
- **A frame with nothing visible** still costs 8–12 ms in Xvfb (10k and 50k files). The
  frame cost has a floor that grows with the file, not with what is visible.

### 3. Latency on the river desktop (nested Xwayland, GPU path)

ms, p50 / p95, measured from the X event's arrival at xschem:

| Case | event → blit (xschem draw) | blit → Xwayland commit | commit → frame `done` | **event → commit** | event → `done` |
|---|---|---|---|---|---|
| working view, drag at ~15 ms | 12 / 19 | 4.3 / 17.1 | 15.6 / 19.2 | **18.6 / 36.7** | 35.5 / 53.4 |
| working view, drag at ~57 ms | 15 / 23 | 1.3 / 2.2 | 9.5 / 17.5 | **16.0 / 25.2** | 28.4 / 43.9 |
| working view, wheel zoom | 7 / 21 | 0.8 / 1.5 | 15.1 / 27.6 | **7.6 / 22.1** | 23.2 / 48.0 |
| full view 10k, drag at ~15 ms | 44 / 57 | 13.1 / 14.3 | 12.7 / 21.9 | **57.3 / 71.0** | 69.8 / 85.5 |
| full view 10k, drag at ~57 ms | 40 / 63 | 13.2 / 16.6 | 12.8 / 22.3 | **53.2 / 78.3** | 68.3 / 100.7 |

Schemify, from render-backend.md § 1 (input → return from `present`, 100k objects, RTX 4060):

| Case | input → `update` | callback wait | CPU + raster + submit | **input → present** |
|---|---|---|---|---|
| keys at mouse rate (8 ms), mailbox | 0.26 / 0.35 | 18.4 / 37.1 | ~1.1 / ~6 | **20.2 / 46.3** |
| wheel zoom at 8 ms | 0.28 / 0.37 | 23.8 / 50.5 | ~1.1 / ~2 | **25.4 / 52.7** |
| keys at 50 ms | | 0.02 / 0.02 | | **1.92 / 8.51** |

The two "→ commit" and "→ present" columns compare the same thing: the moment the frame is
handed to the compositor. The compositor's own step (commit → `done`, 10–16 ms p50) comes
after both.

Reading:

1. **xschem's draw is the pole, not pacing.** In the working view, the draw is 7–15 ms and
   Xwayland adds 1–4 ms (p50) before committing. At full view, the draw is 40–44 ms, about
   3× the Xvfb time: core X primitives through Xwayland's GL backend (glamor) are slower
   than software fb, as the manual's `performance.html` measures for real X drivers. Those
   frames then miss a callback, adding 13 ms.
2. **The wait sits in a different place.** xschem never waits before drawing. Xwayland
   waits for the callback only before *committing*, and it commits the newest content.
   When inputs come slower than the display, blit → commit is 1.3 ms. Schemify waits
   18–24 ms (p50) *before* `update` and paint, and then shows the input that was oldest at
   the start of the wait.
3. **Is xschem simply not frame-paced?** Its own code is not, but on Wayland its output is.
   Xwayland gates commits on `wl_surface.frame`, so xschem cannot tear or show partial
   frames here. The commit spacing in the 15 ms drag was 24 ms (about 42 Hz), the same
   31–44 Hz callback rate render-backend.md saw for Schemify. River delivers callbacks below
   60 Hz to both. Immediate drawing helps xschem only because the draw overlaps the wait.
4. **Newest versus oldest input.** xschem's frames carry 2–3 coalesced inputs, and the
   oldest is 14–20 ms older than the newest (Xvfb: newest 20, oldest 34 ms p50). Schemify's
   log reports the oldest input, so it overstates what the hand feels by about the same
   amount.

## Feel versus correctness: what xschem gives up

| xschem trade | Seen | Schemify can adopt? |
|---|---|---|
| Aliased, integer-pixel core X lines | every segment | **No.** wgpu has no exact X raster rules. Determinism (hard rule 4) is kept by CPU goldens, and AA costs nothing on the instanced pipeline. |
| Full CPU-side redraw of everything visible each frame, O(file) floor | 8–12 ms empty frames, 46 ms at 10k full view | **No.** Fails D39 (≥ 120 fps at 100k). Schemify's retained GPU rows (step 1) are the opposite choice, and the right one. |
| Text dropped below ~3 px, symbols as bbox below ~2 px | § 2 table | **Yes.** Step 6 already plans the glyph cut. Add the bbox cut. Both are pure functions of zoom, so they stay deterministic and testable headless. |
| Draw immediately on input, never wait | strace: only idle `poll(POLLIN)` | **Yes**, as step 0 (below). Idle stays at 0%, because frames still run only on input. |
| Tearing / immediate present | not visible under Xwayland (commits are whole buffers) | **No need.** render-backend.md measured `Immediate` within noise of FIFO and mailbox, and it tears on the web. |
| Retained back buffer, single atomic present | `CopyArea` per frame | **Already true** of a swapchain. The lesson is "never present an incomplete canvas", which tiles violate today. |
| Pan-by-blit | **not used** by xschem | **Not worth it.** xschem doesn't need it, and the instanced pipeline redraws a pan in 0.3–0.4 ms GPU. |

## Recommendations, mapped onto render-backend.md's migration plan

The expected gains are from the measurements above. "Hand latency" means latency from the
newest input.

1. **Step 0, done the way xschem and Xwayland do it: never sample input before the wait.**
   - Run `update` in `RedrawRequested`, so the frame latches the newest coalesced input.
   - Render at once when the last callback has arrived.
   - Log both the newest and the oldest input.
   - Keep a spare swapchain image (`desired_maximum_frame_latency: 2`), so acquire does not
     block. Xwayland rotated through three `wl_buffer`s in these traces.

   Expected gain:
   - At inputs slower than the display, input → present is already 1.9 ms (measured).
   - At mouse rate, hand latency drops by the coalescing span, 14–20 ms at p50 (xschem's
     newest-versus-oldest gap). Schemify's p50 goes from 20–25 ms to about 5 ms.
   - The p95 still pays one callback wait (17 ms in Xwayland's own case at 15 ms inputs).
     Expect 46–53 → about 20–30 ms.
   - Running the monitor at 165 Hz shrinks every remaining wait by 2.75×.

   Change radius: 3, as planned.
2. **Steps 1–2 (instanced pipelines, delete the tile cache) are the "complete frame every
   time" property.** It is the most visible thing xschem has that Schemify lacks. Expected
   gain:
   - No stale or blank tiles.
   - The p95/p99 tail loses the 5–13 ms `Layers::raster` (RTX 4060) and 6–21 ms (iGPU).
   - Unlike xschem, the frame cost does not grow with the file: 0.3–0.4 ms GPU at 100k
     against xschem's 46 ms at 10k.
3. **Step 6, extended with xschem's two LOD cuts.**
   - Skip glyphs below about 3 px (already planned).
   - Draw an instance whose screen size is below about 2 px as one bbox-outline rect row
     instead of its symbol rows: about 30× fewer primitives per instance in xschem's case.
   - Select the cut per instance from the camera scale on the GPU (a size test in the vertex
     shader against the instance's bbox column), or per `PrimIndex` bucket on the CPU.
     Either way it is a pure function of (zoom, bbox), with a headless oracle: bbox draw
     versus full draw, compared by tolerance at the threshold.

   Expected gain: at 1M objects zoom-fit, the vertex-bound 29 ms (iGPU) falls by roughly
   the symbol-to-bbox ratio. That should land under the 8.3 ms D39 budget without the
   overview texture, which could then be dropped from step 6.
4. **Motion coalescing: keep it.** Schemify already coalesces (211 frames for 600 inputs).
   Once step 0 is in, the coalesced frame shows the newest input, as Tk does. No extra
   work.

Not recommended: `PresentMode::Immediate` (no measured gain, and it tears), pan-by-blit (no
need after step 1), aliased lines (they break the GPU/CPU tolerance oracles), and CPU-side
full redraws (they fail D39).
