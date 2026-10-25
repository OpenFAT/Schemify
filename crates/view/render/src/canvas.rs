//! The canvas layer: schematic primitives in `i32` world milli-units (D33) drawn through a
//! pan/zoom [`Camera`] on the CPU, from the same instance rows the GPU pipelines draw
//! ([`crate::gpu::InstanceRows`]).
//!
//! # Data design
//!
//! 1. **In → out.** [`InstanceRows`] (packed from [`CanvasPrims`] by `gpu`: colour, width,
//!    fill and drawing order already decided) + [`Camera`] → per layer a few batched paths →
//!    painter calls.
//! 2. **How many.** 10^2–10^5 visible primitives per frame (D39 budgets 120 fps at 10^5).
//! 3. **How wide.** Rows in `i32` milli-units (±2.1 M covers the corpus, D33), arc angles `i32`
//!    millidegrees, text size `i32` milli-units; screen math in `f64` (the camera), path points
//!    `f32` where the painter takes them.
//! 4. **Access.** Rows come grouped by palette layer, so one pass per layer reads that layer's
//!    slot range of each kind and batches it into reused paths: one draw call per (layer,
//!    path kind), not per primitive.
//! 5. **Lifetime.** Rows: the CPU painter's canvas keeps them across frames (patched like the
//!    GPU's); [`draw_prims`] packs into [`CanvasScratch`] per call. Paths and glyph caches:
//!    reused across frames, never freed while the canvas lives.
//! 6. **Parallel.** Layers are drawn in order (antialiased edges blend over the layers below),
//!    so serial.

use std::collections::HashMap;

use kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use peniko::FontData;
use vello_cpu::Glyph;

use schemify_ui::TextSystem;
use schemify_ui::parley::PositionedLayoutItem;

use crate::gpu::{self, ARC, CLOSED, Change, EDGE, FILL, FIRST, InstanceRows, LINE, PARITY, RECT};
use crate::painter::{Chains, Painter, push_rect, rgba};
use schemify_canvas::*;

/// Reused per-frame buffers, the rows of the tables drawn through it, and the shaped-label
/// cache (glyphs per distinct string, shared by every primitive table drawn through it). Keep
/// one per canvas.
#[derive(Default)]
pub struct CanvasScratch {
    /// The paths [`draw_rows`] batches into.
    pub paths: Paths,
    rows: InstanceRows,
    text_face: Option<FontData>,
    /// Glyph range in `text_glyphs` of each string shaped so far.
    text_runs: HashMap<Box<str>, (u32, u32)>,
    text_glyphs: Vec<Glyph>,
    text_layout: schemify_ui::parley::Layout<()>,
}

impl std::fmt::Debug for CanvasScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasScratch")
            .field("strings", &self.text_runs.len())
            .field("glyphs", &self.text_glyphs.len())
            .finish()
    }
}

/// The paths [`draw_rows`] batches one layer into (reused, so a frame allocates nothing once
/// they have grown), and the grid's dots.
#[derive(Debug, Default)]
pub struct Paths {
    /// Plain closed strokes (rects, polygons), plain open ones (lines, polylines), plain arcs.
    closed: Chains,
    open: Chains,
    curves: BezPath,
    /// Fills: rects, circles, polygons, one closed shape after another, and where each ends
    /// (its element count): each is filled on its own, so shapes wound opposite ways that
    /// overlap do not cancel as one non-zero path would, and `one`, the shape being filled.
    fill: BezPath,
    fill_ends: Vec<usize>,
    one: BezPath,
    /// Styled strokes, one path per (width in 1/16 px, dash) seen so far.
    styled: Vec<(u32, u32, BezPath)>,
    /// Far-zoom rows as the whole pixels their box touches.
    lod: Chains,
    /// The marks of one colour run.
    marks: BezPath,
    dots: Chains,
}

impl Paths {
    /// Empties the per-layer paths, keeping their capacity (and the styles seen).
    fn clear(&mut self) {
        self.closed.clear();
        self.open.clear();
        self.lod.clear();
        self.fill_ends.clear();
        for path in [&mut self.curves, &mut self.fill, &mut self.marks] {
            path.truncate(0);
        }
        for b in &mut self.styled {
            b.2.truncate(0);
        }
    }

    /// The path that collects the rows of style word `style` (its width and dash; made on
    /// first use), and the pixel offset its points snap to: a pixel centre for an odd rounded
    /// width, the pixel edge for an even one, so both stay crisp.
    fn styled(&mut self, style: u32) -> (&mut BezPath, f64) {
        let (q16, dash) = (style & 0xffff, (style >> 16) & 0xf);
        let w = f64::from(q16) / 16.0;
        let o = if (w.round() as i64) % 2 == 0 {
            0.0
        } else {
            0.5
        };
        let at = match self.styled.iter().position(|b| (b.0, b.1) == (q16, dash)) {
            Some(at) => at,
            None => {
                assert!(
                    self.styled.len() < LAYERS * 64,
                    "styles are a byte per layer"
                );
                self.styled.push((q16, dash, BezPath::new()));
                self.styled.len() - 1
            }
        };
        (&mut self.styled[at].2, o)
    }

    /// Notes a row whose screen box `b` is [`LOD_PX`]-small as the whole pixels it touches;
    /// false (nothing noted) when it is larger.
    #[inline]
    fn lod(&mut self, b: [f64; 4]) -> bool {
        debug_assert!(b[0] <= b[2] && b[1] <= b[3]);
        if b[2] - b[0] > LOD_PX || b[3] - b[1] > LOD_PX {
            return false;
        }
        let (x0, y0) = (pixel(b[0]) as f32, pixel(b[1]) as f32);
        let (x1, y1) = (pixel(b[2]) as f32 + 1.0, pixel(b[3]) as f32 + 1.0);
        // ponytail: one rect per far-zoom row (overlaps fill once: whole pixels, non-zero);
        // a per-layer pixel mask drawn as runs if the GPU-less far-zoom frame measures slow.
        self.lod.push(&[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
        true
    }
}

/// Primitives whose screen box is at most this many logical px each way are drawn as the whole
/// pixels that box touches (level of detail, D39): below two pixels a stroke and a block look
/// alike, and a block is cheap. A function of the camera and the row only (a chain on the CPU,
/// as one path), as `shaders/canvas/camera.wgsl` cuts it.
pub const LOD_PX: f64 = 2.0;

/// Labels are shaped once at this size and scaled by the camera.
const TEXT_REF_PX: f32 = 64.0;
/// Labels smaller than this on screen are skipped (unreadable, and the cheapest cull).
pub const TEXT_MIN_PX: f64 = 3.0;
/// Distinct strings the label cache keeps before it starts over (a phase boundary, like the UI
/// text cache).
const TEXT_RUNS_MAX: usize = 1 << 16;

/// Draws background, grid, primitives, overlay primitives and marks into `view`, clipped.
/// The diagonal route for the common case; each step is also public.
#[allow(clippy::too_many_arguments)]
pub fn draw_canvas<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    grid_step: i32,
    prims: &CanvasPrims,
    palette: &Palette,
    overlay: &CanvasPrims,
    marks: &Marks,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    p.push_clip(view);
    p.fill_rect(view, rgba(palette.background));
    draw_grid(p, view, cam, grid_step, palette.grid, &mut scratch.paths);
    draw_prims(p, view, cam, prims, palette, text, scratch);
    draw_prims(p, view, cam, overlay, palette, text, scratch);
    draw_marks(p, view, cam, prims, marks, scratch);
    p.pop_clip();
}

/// Grid dots every `step` milli-units in `color` (`0xRRGGBBAA`), doubled until they are ≥ 10
/// px apart (xschem's rule).
pub fn draw_grid<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    step: i32,
    color: u32,
    paths: &mut Paths,
) {
    let Some((s, i0, i1, j0, j1)) = grid_dots(view, cam, step) else {
        return;
    };
    let (ox, oy) = cam.origin(view);
    let dots = &mut paths.dots;
    dots.clear();
    for j in j0..=j1 {
        let y = pixel(j as f64 * s * cam.scale + oy) as f32;
        for i in i0..=i1 {
            let x = pixel(i as f64 * s * cam.scale + ox) as f32;
            dots.push(&[[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]]);
        }
    }
    p.fill_chains(dots, rgba(color));
}

/// The grid `step` milli-units apart as `view` shows it through `cam`: the dot pitch in
/// milli-units (doubled until the dots are ≥ 10 px apart, xschem's rule) and the dot indices
/// `[i0, i1] × [j0, j1]` in view, dot `i` at world `i · pitch`. `None` when there is no
/// readable grid: a degenerate zoom, or more than 200 000 dots.
///
/// # Panics
///
/// Panics on `step ≤ 0`.
pub fn grid_dots(view: Rect, cam: &Camera, step: i32) -> Option<(f64, i64, i64, i64, i64)> {
    assert!(step > 0, "grid step must be positive");
    let mut s = f64::from(step);
    // Bounded: 40 doublings exceed any i32 range.
    for _ in 0..40 {
        if s * cam.scale >= 10.0 {
            break;
        }
        s *= 2.0;
    }
    let (wx0, wy0) = cam.to_world(view, view.x0, view.y0);
    let (wx1, wy1) = cam.to_world(view, view.x1, view.y1);
    let (i0, i1) = ((wx0 / s).ceil() as i64, (wx1 / s).floor() as i64);
    let (j0, j1) = ((wy0 / s).ceil() as i64, (wy1 / s).floor() as i64);
    if (i1 - i0 + 1).saturating_mul(j1 - j0 + 1) > 200_000 || i1 < i0 || j1 < j0 {
        return None; // degenerate zoom: no readable grid
    }
    assert!(s >= f64::from(step));
    Some((s, i0, i1, j0, j1))
}

/// Millidegrees in radians.
#[inline]
fn radians(m: u32) -> f64 {
    f64::from(m as i32).to_radians() / 1000.0
}

/// Appends a circular arc as ≤ 4 cubics (one per started quarter turn); a sweep of a full turn
/// or more closes into a circle.
///
/// One `sin_cos` per arc plus a rotation per segment: kurbo's adaptive `Arc` costs ~8× more,
/// which dominated the 100k-primitive frame. Radial error ≤ 2.7e-4 · r (sub-pixel below
/// r = 3000 px).
fn push_arc(path: &mut BezPath, cx: f64, cy: f64, r: f64, start: f64, sweep: f64) {
    let full = sweep.abs() >= std::f64::consts::TAU;
    let sweep = sweep.clamp(-std::f64::consts::TAU, std::f64::consts::TAU);
    let n = ((sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as u32).clamp(1, 4);
    let step = sweep / f64::from(n);
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let (mut sn, mut cs) = start.sin_cos();
    let (ss, cc) = step.sin_cos();
    path.move_to((cx + r * cs, cy + r * sn));
    for _ in 0..n {
        let (s2, c2) = (sn * cc + cs * ss, cs * cc - sn * ss);
        path.curve_to(
            (cx + r * (cs - k * sn), cy + r * (sn + k * cs)),
            (cx + r * (c2 + k * s2), cy + r * (s2 - k * c2)),
            (cx + r * c2, cy + r * s2),
        );
        (sn, cs) = (s2, c2);
    }
    if full {
        path.close_path();
    }
}

/// Snaps a coordinate to the pixel center so 1 px strokes stay crisp. The 1e-6 px bias keeps a
/// coordinate that lands on a pixel edge on the same side however its camera origin was
/// rounded (two derivations differ by ~1e-9 px).
#[inline]
fn snap(v: f64) -> f64 {
    pixel(v) + 0.5
}

/// The pixel `v` falls in, with [`snap`]'s bias.
#[inline]
fn pixel(v: f64) -> f64 {
    pixel_i(v) as f64
}

/// [`pixel`] as an integer. A truncating cast and a compare: `f64::floor` is a libm call on
/// baseline x86-64 (no SSE4.1), which cost ~10% of a far-zoom frame. Exact for |v| < 2^63
/// (screen coordinates are far inside).
#[inline]
fn pixel_i(v: f64) -> i64 {
    let v = v + 1e-6;
    let t = v as i64;
    t - i64::from((t as f64) > v)
}

/// A screen point snapped to a pixel centre, as path points.
#[inline]
fn pt((x, y): (f64, f64)) -> [f32; 2] {
    [snap(x) as f32, snap(y) as f32]
}

/// A screen point snapped to whole pixels plus `o` (see [`Paths::styled`]).
#[inline]
fn pt_at((x, y): (f64, f64), o: f64) -> (f64, f64) {
    (pixel(x) + o, pixel(y) + o)
}

/// The ordered box `[x0, y0, x1, y1]` of two screen points.
#[inline]
fn boxed(a: (f64, f64), b: (f64, f64)) -> [f64; 4] {
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
}

/// The stroke of a styled row's style word: its width (1/16 px), round caps and joins (a thick
/// zigzag mitres into spikes), dashes and gaps of `2 · dash` px.
fn stroke_of(q16: u32, dash: u32) -> Stroke {
    assert!(q16 > 0, "zero-width stroke");
    let w = f64::from(q16) / 16.0;
    let stroke = Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round);
    let d = 2.0 * f64::from(dash);
    if d > 0.0 {
        stroke.with_dashes(0.0, [d, d])
    } else {
        stroke
    }
}

/// Draws the primitives of `prims` (packed into `scratch`'s rows, then [`draw_rows`]) and their
/// labels ([`draw_texts`]), culled to `view`: the immediate route (the gesture overlay, PNG
/// export, goldens) for a table no painter keeps rows of.
///
/// # Time complexity
///
/// O(primitives + glyphs of visible labels); allocation-free once `scratch` has grown.
pub fn draw_prims<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    scratch.rows.pack(0, prims, palette, Change::All);
    draw_rows(p, view, cam, &scratch.rows, 0, &mut scratch.paths);
    draw_texts(p, view, cam, &prims.texts, None, palette, text, scratch);
}

/// Draws source `src` of `rows` (a canvas table, or [`gpu::MARKS`]) into `view` through `cam`
/// as the CPU paints the canvas: layer by layer, each layer batched into a few paths drawn in
/// turn (fills; plain closed, open and curved strokes; each stroke style; the far-zoom pixel
/// boxes), the marks as one stroke per colour run. Rows off the view (2 px of slack) are
/// culled; a row, or a whole polyline or polygon, whose screen box is at most [`LOD_PX`] each
/// way is the whole pixels it touches (marks excepted). Texts are not rows ([`draw_texts`]).
///
/// Cost: O(rows of `src`); allocation-free once `paths` has grown.
///
/// # Panics
///
/// Panics on `src ≥ gpu::SOURCES`.
pub fn draw_rows<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    rows: &InstanceRows,
    src: usize,
    paths: &mut Paths,
) {
    assert!(src < gpu::SOURCES, "a row source");
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let cull = view.inflate(2.0, 2.0);
    let outside =
        |b: [f64; 4]| b[2] < cull.x0 || b[0] > cull.x1 || b[3] < cull.y0 || b[1] > cull.y1;
    let at = |x: u32, y: u32| (f64::from(x as i32) * s + ox, f64::from(y as i32) * s + oy);
    let flags = |style: u32| (style >> 20) & 0xf;
    let marks = src == gpu::MARKS;
    for layer in 0..LAYERS {
        paths.clear();
        // The layer's colour and plain stroke width, from its rows (one of each per layer, the
        // pack's decision); marks: the run's colour and its 2 px.
        let (mut color, mut width) = (0u32, 0u32);
        for slot in rows.layer_slots(src, LINE, layer) {
            let w = rows.row(src, LINE, slot);
            if w[4] == 0 {
                continue; // an emptied slot
            }
            let (a, b) = (at(w[0], w[1]), at(w[2], w[3]));
            let bx = boxed(a, b);
            if outside(bx) {
                continue;
            }
            color = w[4];
            if marks {
                width = w[5] & 0xffff;
                paths.marks.move_to((snap(a.0), snap(a.1)));
                paths.marks.line_to((snap(b.0), snap(b.1)));
                continue;
            }
            if paths.lod(bx) {
                continue;
            }
            if flags(w[5]) & PARITY != 0 {
                let (path, o) = paths.styled(w[5]);
                path.move_to(pt_at(a, o));
                path.line_to(pt_at(b, o));
                continue;
            }
            width = w[5] & 0xffff;
            paths.open.push(&[pt(a), pt(b)]);
        }
        for slot in rows.layer_slots(src, RECT, layer) {
            let w = rows.row(src, RECT, slot);
            if w[4] == 0 {
                continue;
            }
            let [x0, y0, x1, y1] = boxed(at(w[0], w[1]), at(w[2], w[3]));
            if outside([x0, y0, x1, y1]) {
                continue;
            }
            color = w[4];
            if marks {
                width = w[5] & 0xffff;
                let r = Rect::new(snap(x0), snap(y0), snap(x1), snap(y1));
                push_rect(&mut paths.marks, r);
                continue;
            }
            if paths.lod([x0, y0, x1, y1]) {
                continue;
            }
            if flags(w[5]) & FILL != 0 {
                push_rect(&mut paths.fill, Rect::new(x0, y0, x1, y1));
                paths.fill_ends.push(paths.fill.elements().len());
            }
            if flags(w[5]) & PARITY != 0 {
                let (path, o) = paths.styled(w[5]);
                let (a, b) = (pt_at((x0, y0), o), pt_at((x1, y1), o));
                push_rect(path, Rect::new(a.0, a.1, b.0, b.1));
                continue;
            }
            width = w[5] & 0xffff;
            let (a, b) = (pt((x0, y0)), pt((x1, y1)));
            paths.closed.push(&[a, [b[0], a[1]], b, [a[0], b[1]]]);
        }
        for slot in rows.layer_slots(src, ARC, layer) {
            let w = rows.row(src, ARC, slot);
            if w[5] == 0 {
                continue;
            }
            let ((cx, cy), r) = (at(w[0], w[1]), f64::from(w[2] as i32) * s);
            let bx = boxed((cx - r, cy - r), (cx + r, cy + r));
            if outside(bx) {
                continue;
            }
            color = w[5];
            let (start, sweep) = (radians(w[3]), radians(w[4]));
            if marks {
                width = w[6] & 0xffff;
                push_arc(&mut paths.marks, cx, cy, r, start, sweep);
                continue;
            }
            if paths.lod(bx) {
                continue;
            }
            let curves = if flags(w[6]) & PARITY != 0 {
                paths.styled(w[6]).0
            } else {
                width = w[6] & 0xffff;
                &mut paths.curves
            };
            push_arc(curves, cx, cy, r, start, sweep);
            if flags(w[6]) & FILL != 0 {
                push_arc(&mut paths.fill, cx, cy, r, start, sweep);
                paths.fill_ends.push(paths.fill.elements().len());
            }
        }
        // Edges: each chain (a run of edges from one flagged FIRST) as the one path it is.
        let edges = rows.layer_slots(src, EDGE, layer);
        let mut k = edges.start;
        while k < edges.end {
            let first = rows.row(src, EDGE, k);
            let mut end = k + 1;
            while end < edges.end && flags(rows.row(src, EDGE, end)[5]) & FIRST == 0 {
                end += 1;
            }
            let chain = k..end;
            k = end;
            let (style, closed) = (first[5], flags(first[5]) & CLOSED != 0);
            if first[4] == 0 {
                continue;
            }
            assert!(
                flags(style) & FIRST != 0,
                "a chain starts at its first edge"
            );
            // The chain's points: the first edge's start, then every edge's end (a polygon's
            // last edge ends where it started: that point is the close).
            let n = chain.len() + usize::from(!closed);
            let point = |j: usize| {
                let w = rows.row(src, EDGE, chain.start + j.saturating_sub(1) as u32);
                if j == 0 {
                    at(w[0], w[1])
                } else {
                    at(w[2], w[3])
                }
            };
            let mut bx = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for j in 0..n {
                let q = point(j);
                bx = [
                    bx[0].min(q.0),
                    bx[1].min(q.1),
                    bx[2].max(q.0),
                    bx[3].max(q.1),
                ];
            }
            if outside(bx) {
                continue;
            }
            color = first[4];
            if marks {
                width = style & 0xffff;
                for j in 0..n {
                    let q = point(j);
                    let q = (snap(q.0), snap(q.1));
                    if j == 0 {
                        paths.marks.move_to(q);
                    } else {
                        paths.marks.line_to(q);
                    }
                }
                if closed {
                    paths.marks.close_path();
                }
                continue;
            }
            if paths.lod(bx) {
                continue;
            }
            if flags(style) & PARITY != 0 {
                let (path, o) = paths.styled(style);
                for j in 0..n {
                    let q = pt_at(point(j), o);
                    if j == 0 {
                        path.move_to(q);
                    } else {
                        path.line_to(q);
                    }
                }
                if closed {
                    path.close_path();
                }
            } else {
                width = style & 0xffff;
                let c = if closed {
                    &mut paths.closed
                } else {
                    &mut paths.open
                };
                c.pts.extend((0..n).map(|j| pt(point(j))));
                c.end.push(c.pts.len() as u32);
            }
            if closed && flags(style) & FILL != 0 {
                let f = &mut paths.fill;
                f.move_to(point(0));
                for j in 1..n {
                    f.line_to(point(j));
                }
                f.close_path();
                paths.fill_ends.push(paths.fill.elements().len());
            }
        }
        if color == 0 {
            continue; // nothing of this layer in view
        }
        let (c, w) = (rgba(color.swap_bytes()), f64::from(width) / 16.0);
        if marks {
            if !paths.marks.is_empty() {
                p.stroke_path(&paths.marks, w, c);
            }
            continue;
        }
        // Each closed shape on its own (the GPU fills each instance on its own too).
        let mut at = 0;
        for &end in &paths.fill_ends {
            paths.one.truncate(0);
            paths
                .one
                .extend(paths.fill.elements()[at..end].iter().copied());
            p.fill_path(&paths.one, c);
            at = end;
        }
        debug_assert_eq!(at, paths.fill.elements().len(), "every fill ends a shape");
        if !paths.closed.is_empty() {
            p.stroke_chains(&paths.closed, true, w, c);
        }
        if !paths.open.is_empty() {
            p.stroke_chains(&paths.open, false, w, c);
        }
        if !paths.curves.is_empty() {
            p.stroke_path(&paths.curves, w, c);
        }
        for (q16, dash, path) in &paths.styled {
            if !path.is_empty() {
                p.stroke_styled(path, &stroke_of(*q16, *dash), c);
            }
        }
        if !paths.lod.is_empty() {
            p.fill_chains(&paths.lod, c);
        }
    }
}

/// Every row `0..n`, or the listed ones.
fn each(n: usize, rows: Option<&[u32]>) -> impl Iterator<Item = usize> + '_ {
    let all = rows.is_none().then_some(0..n).into_iter().flatten();
    all.chain(rows.into_iter().flatten().map(|&i| i as usize))
}

/// Draws the labels of `t` (the rows `rows` lists, or all), culled to `view` and skipping any
/// under [`TEXT_MIN_PX`] on screen; returns the labels drawn. Text is not an instance row: the
/// painter draws it after each table's rows until the glyph atlas lands.
///
/// Cost: O(listed rows + glyphs of visible labels).
#[allow(clippy::too_many_arguments)]
pub fn draw_texts<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    t: &Texts,
    rows: Option<&[u32]>,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) -> usize {
    assert_eq!(t.x.len(), t.end.len());
    let (ox, oy) = cam.origin(view);
    let mut drawn = 0;
    for i in each(t.x.len(), rows) {
        if usize::from(t.layer[i]) >= LAYERS {
            continue; // dead
        }
        let bytes = chain(&t.end, i);
        let px = f64::from(t.size[i]) * cam.scale;
        let (x, y) = (
            f64::from(t.x[i]) * cam.scale + ox,
            f64::from(t.y[i]) * cam.scale + oy,
        );
        // A turned label extends left/up of its anchor too: only the size culls it.
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let visible = px >= TEXT_MIN_PX
            && (rot != 0 || (x < view.x1 && y < view.y1 && y + px * 2.0 > view.y0));
        if visible {
            let (g0, g1) = shaped(&t.text[bytes], text, scratch);
            if let (Some(face), true) = (scratch.text_face.clone(), g1 > g0) {
                let at = Affine::translate((x, y))
                    * Affine::rotate(f64::from(rot) * std::f64::consts::FRAC_PI_2)
                    * Affine::scale(px / f64::from(TEXT_REF_PX));
                let color = rgba(palette.color[usize::from(t.layer[i])]);
                let glyphs = &scratch.text_glyphs[g0 as usize..g1 as usize];
                p.glyphs(&face, TEXT_REF_PX, &[], color, at, glyphs);
                drawn += 1;
            }
        }
    }
    drawn
}

/// The glyph range of `s` shaped at [`TEXT_REF_PX`] (positions relative to its top-left),
/// shaping it on first use: a string is shaped once per cache lifetime, however many labels or
/// frames show it.
fn shaped(s: &str, text: &mut TextSystem, scratch: &mut CanvasScratch) -> (u32, u32) {
    if let Some(&r) = scratch.text_runs.get(s) {
        return r;
    }
    if scratch.text_runs.len() >= TEXT_RUNS_MAX {
        scratch.text_runs.clear();
        scratch.text_glyphs.clear();
    }
    let g0 = scratch.text_glyphs.len() as u32;
    text.shape_into(s, TEXT_REF_PX, &mut scratch.text_layout);
    for line in scratch.text_layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                // ponytail: one face for all labels (the only registered UI font). Font
                // fallback would need a face per glyph range.
                scratch
                    .text_face
                    .get_or_insert_with(|| run.run().font().clone());
                scratch
                    .text_glyphs
                    .extend(run.positioned_glyphs().map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }));
            }
        }
    }
    let r = (g0, scratch.text_glyphs.len() as u32);
    scratch.text_runs.insert(s.into(), r);
    assert!(r.0 <= r.1);
    r
}

/// Redraws the marked rows of `prims` in their mark colour, 2 px wide, one stroke per run of
/// one colour (packed into `scratch`'s rows, then [`draw_rows`]).
pub fn draw_marks<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    scratch: &mut CanvasScratch,
) {
    scratch.rows.pack_marks(prims, marks, true);
    draw_rows(p, view, cam, &scratch.rows, gpu::MARKS, &mut scratch.paths);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_round_trips_and_zoom_keeps_anchor() {
        let view = Rect::new(10.0, 20.0, 810.0, 620.0);
        let mut cam = Camera {
            cx: 5_000.0,
            cy: -3_000.0,
            scale: 0.02,
        };
        let (sx, sy) = cam.to_screen(view, 12_345, -6_789);
        let (wx, wy) = cam.to_world(view, sx, sy);
        assert!((wx - 12_345.0).abs() < 1e-6 && (wy + 6_789.0).abs() < 1e-6);
        cam.zoom_at(view, sx, sy, 3.0);
        let (sx2, sy2) = cam.to_screen(view, 12_345, -6_789);
        assert!(
            (sx2 - sx).abs() < 1e-6 && (sy2 - sy).abs() < 1e-6,
            "point under cursor stays put"
        );
        cam.pan(10.0, -5.0);
        let (sx3, sy3) = cam.to_screen(view, 12_345, -6_789);
        assert!((sx3 - sx - 10.0).abs() < 1e-6 && (sy3 - sy + 5.0).abs() < 1e-6);
        cam.zoom_at(view, 0.0, 0.0, 1e30);
        assert_eq!(cam.scale, Camera::SCALE_MAX, "zoom is clamped");
    }

    /// Renders `prims` on a 100 × 100 black view at `scale` px per milli-unit centred on the
    /// origin and returns the green channel (layer 4, xschem's symbol green).
    fn lit(prims: &CanvasPrims, scale: f64) -> Vec<u8> {
        let view = Rect::new(0.0, 0.0, 100.0, 100.0);
        let cam = Camera {
            cx: 0.0,
            cy: 0.0,
            scale,
        };
        let mut p = crate::CpuPainter::new(100, 100, 1.0);
        let mut text = schemify_ui::TextSystem::new();
        let mut sc = CanvasScratch::default();
        let palette = Palette::xschem_dark();
        draw_prims(&mut p, view, &cam, prims, &palette, &mut text, &mut sc);
        let px = p.finish(rgba(0x0000_00ff));
        let out: Vec<u8> = px.data().iter().map(|c| c.g).collect();
        assert_eq!(out.len(), 100 * 100);
        out
    }

    /// Path elements a painter was asked to stroke.
    #[derive(Default)]
    struct Strokes(usize);

    impl Painter for Strokes {
        fn fill_rect(&mut self, _: Rect, _: peniko::Color) {}
        fn fill_path(&mut self, _: &BezPath, _: peniko::Color) {}
        fn stroke_path(&mut self, path: &BezPath, _: f64, _: peniko::Color) {
            self.0 += path.elements().len();
        }
        fn stroke_styled(&mut self, _: &BezPath, _: &Stroke, _: peniko::Color) {}
        fn glyphs(
            &mut self,
            _: &FontData,
            _: f32,
            _: &[i16],
            _: peniko::Color,
            _: Affine,
            _: &[Glyph],
        ) {
        }
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
    }

    /// A frame strokes only the marks that reach into its view: marked rows far outside it
    /// add nothing to the path (a select-all frame once stroked every mark per canvas tile).
    #[test]
    fn marks_outside_the_view_are_not_drawn() {
        let view = Rect::new(0.0, 0.0, 100.0, 100.0);
        let cam = Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 1e-3,
        };
        let stroked = |far: i32| {
            let (mut prims, mut marks) = (CanvasPrims::default(), Marks::default());
            prims.line(-10_000, 0, 10_000, 0, 4);
            marks.push(PrimKind::Line, 0, 0xffff_00ff);
            for k in 0..far {
                prims.line(1_000_000 + k, 0, 1_000_000 + k, 1_000, 4);
                marks.push(PrimKind::Line, k as u32 + 1, 0xffff_00ff);
            }
            let mut p = Strokes::default();
            draw_marks(
                &mut p,
                view,
                &cam,
                &prims,
                &marks,
                &mut CanvasScratch::default(),
            );
            p.0
        };
        assert_eq!(stroked(0), 2, "the mark in view: one move and one line");
        assert_eq!(stroked(1_000), stroked(0), "marks far away draw nothing");
    }

    fn ink(px: &[u8]) -> u32 {
        px.iter().map(|&g| u32::from(g)).sum()
    }

    #[test]
    fn style_packs_and_clamps() {
        assert_eq!(LineStyle::new(1, 0), LineStyle::PLAIN);
        let s = LineStyle::new(3, 7);
        assert_eq!((s.weight(), s.dash()), (3, 7));
        let c = LineStyle::new(9, 200);
        assert_eq!((c.weight(), c.dash()), (4, 15), "out of range clamps");
        assert_eq!(LineStyle::new(0, 0).weight(), 1, "weight 0 is weight 1");
    }

    #[test]
    fn weight_two_draws_twice_the_ink_of_weight_one() {
        let at = |w: u8| {
            let mut p = CanvasPrims::default();
            p.line_styled(-40_000, 0, 40_000, 0, 4, LineStyle::new(w, 0));
            ink(&lit(&p, 1e-3))
        };
        let (one, two, four) = (at(1), at(2), at(4));
        assert!(one > 0, "weight 1 draws");
        let r2 = f64::from(two) / f64::from(one);
        let r4 = f64::from(four) / f64::from(one);
        assert!((1.8..2.3).contains(&r2), "weight 2 / weight 1 ink = {r2}");
        assert!((3.6..4.5).contains(&r4), "weight 4 / weight 1 ink = {r4}");
    }

    #[test]
    fn dash_leaves_gaps_and_solid_does_not() {
        // A vertical segment down column 50; count unlit rows along it.
        let gaps = |dash: u8| {
            let mut p = CanvasPrims::default();
            p.line_styled(0, -40_000, 0, 40_000, 4, LineStyle::new(1, dash));
            let px = lit(&p, 1e-3);
            (15..85)
                .filter(|&y| (48..53).all(|x| px[y * 100 + x] < 40))
                .count()
        };
        assert_eq!(gaps(0), 0, "solid has no gap");
        let g = gaps(3);
        assert!(
            (20..50).contains(&g),
            "dash 3 (6 px on, 6 off) leaves gaps: {g}"
        );
        // Dashes apply to every kind, not only lines.
        let mut p = CanvasPrims::default();
        p.arc_styled(0, 0, 40_000, 0, 360_000, 4, LineStyle::new(1, 3));
        let mut solid = CanvasPrims::default();
        solid.arc(0, 0, 40_000, 0, 360_000, 4);
        assert!(ink(&lit(&p, 1e-3)) < ink(&lit(&solid, 1e-3)) * 3 / 4);
    }

    /// A filled rect and a filled polygon that overlap fill their overlap whichever way the
    /// polygon winds (each shape is filled on its own; one non-zero path of both cancelled
    /// there when they wound opposite ways), and a filled circle does too.
    #[test]
    fn overlapping_fills_wound_either_way_both_fill() {
        let at = |px: &[u8], x: usize, y: usize| px[y * 100 + x];
        let square = [
            (-10_000, -10_000),
            (30_000, -10_000),
            (30_000, 30_000),
            (-10_000, 30_000),
        ];
        for reversed in [false, true] {
            let mut pts = square.to_vec();
            if reversed {
                pts.reverse();
            }
            let mut p = CanvasPrims::default();
            p.rect_styled(
                -30_000,
                -30_000,
                10_000,
                10_000,
                4,
                LineStyle::PLAIN.filled(),
            );
            p.poly_styled(&pts, true, 4, LineStyle::PLAIN.filled());
            p.arc_styled(0, 0, 4_000, 0, -360_000, 4, LineStyle::PLAIN.filled());
            let px = lit(&p, 1e-3);
            // (50, 50) is world (0, 0): inside all three.
            assert!(
                at(&px, 50, 50) > 0xc0,
                "overlap filled (reversed {reversed})"
            );
            assert!(
                at(&px, 44, 44) > 0xc0,
                "rect and polygon overlap (reversed {reversed})"
            );
        }
    }

    #[test]
    fn fill_bit_fills_closed_shapes_only() {
        let tri = [(-30_000, -30_000), (30_000, 0), (-30_000, 30_000)];
        let draw = |closed: bool, st: LineStyle| {
            let mut p = CanvasPrims::default();
            p.poly_styled(&tri, closed, 4, st);
            ink(&lit(&p, 1e-3))
        };
        let (outline, filled) = (
            draw(true, LineStyle::PLAIN),
            draw(true, LineStyle::PLAIN.filled()),
        );
        assert!(
            filled > outline * 5,
            "layer 4 does not fill; the bit does: {outline} {filled}"
        );
        assert!(LineStyle::new(2, 3).filled().fills() && !LineStyle::new(2, 3).fills());
        assert!(
            draw(false, LineStyle::PLAIN.filled()) < filled / 3,
            "an open chain never fills"
        );
    }

    #[test]
    fn strokes_keep_a_pixel_when_zoomed_far_out() {
        // 8·10^6 milli-units at 10^-5 px each = 80 px long; the stroke stays ≥ 1 px across.
        for w in [1u8, 2] {
            let mut p = CanvasPrims::default();
            p.line_styled(-4_000_000, 0, 4_000_000, 0, 4, LineStyle::new(w, 0));
            let px = lit(&p, 1e-5);
            for x in 15..85 {
                let col: u32 = (0..100).map(|y| u32::from(px[y * 100 + x])).sum();
                let want = 0xdd * u32::from(w) * 9 / 10;
                assert!(col >= want, "column {x} weight {w}: {col} < {want}");
            }
        }
    }

    #[test]
    fn plain_style_draws_exactly_like_unstyled() {
        let mut a = CanvasPrims::default();
        let mut b = CanvasPrims::default();
        let tri = [(0, 0), (10_000, 30_000), (-25_000, 30_000)];
        a.line(-30_000, -10_000, 30_000, 20_000, 4);
        a.rect(-20_000, -20_000, 10_000, 5_000, 4);
        a.arc(5_000, 5_000, 12_000, 0, 270_000, 4);
        a.poly(&tri, true, 4);
        let plain = LineStyle::PLAIN;
        b.line_styled(-30_000, -10_000, 30_000, 20_000, 4, plain);
        b.rect_styled(-20_000, -20_000, 10_000, 5_000, 4, plain);
        b.arc_styled(5_000, 5_000, 12_000, 0, 270_000, 4, plain);
        b.poly_styled(&tri, true, 4, plain);
        assert_eq!(a, b);
        assert_eq!(lit(&a, 1e-3), lit(&b, 1e-3));
        // ...and a styled row does not.
        b.line_styled(-30_000, 30_000, 30_000, 30_000, 4, LineStyle::new(2, 0));
        assert_ne!(lit(&a, 1e-3), lit(&b, 1e-3));
    }

    #[test]
    fn fit_centers_bounds() {
        let view = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let mut cam = Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 1.0,
        };
        cam.fit(view, [-100_000, -20_000, 100_000, 20_000]);
        let (x0, _) = cam.to_screen(view, -100_000, 0);
        let (x1, _) = cam.to_screen(view, 100_000, 0);
        assert!((x0 - 25.0).abs() < 1e-6 && (x1 - 975.0).abs() < 1e-6);
    }
}
