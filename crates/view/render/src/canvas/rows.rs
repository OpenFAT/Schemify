//! Rows on the CPU: one row source of [`InstanceRows`] and a camera in, painter calls out.
//! Layer by layer, each layer's rows are culled to the view and batched into a few reused
//! paths ([`Paths`]), each drawn with one painter call: fills, plain closed, open and curved
//! strokes, each stroke style, and the far-zoom pixel boxes ([`super::LOD_PX`]); the marks are
//! one stroke per colour run.
//!
//! Contents:
//! - `pub struct Paths`
//! - `pub fn draw_rows<P: Painter>(p: &mut P, view: Rect, cam: &Camera, rows: &InstanceRows, src: usize, paths: &mut Paths)`
//! - `pub(super) fn pixel(v: f64) -> f64`

use kurbo::{BezPath, Cap, Join, Rect, Stroke};

use schemify_canvas::{Camera, LAYERS};

use super::LOD_PX;
use crate::gpu::{self, ARC, CLOSED, EDGE, FILL, FIRST, InstanceRows, LINE, PARITY, RECT};
use crate::painter::{Chains, Painter, push_rect, rgba};

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
    pub(super) dots: Chains,
}

/// Draws source `src` of `rows` (a canvas table, or [`gpu::MARKS`]) into `view` through `cam`
/// as the CPU paints the canvas: layer by layer, each layer batched into a few paths drawn in
/// turn (fills; plain closed, open and curved strokes; each stroke style; the far-zoom pixel
/// boxes), the marks as one stroke per colour run. Rows off the view (2 px of slack) are
/// culled; a row, or a whole polyline or polygon, whose screen box is at most [`LOD_PX`] each
/// way is the whole pixels it touches (marks excepted). Texts are not rows ([`super::draw_texts`]).
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

/// The pixel `v` falls in, with [`snap`]'s bias.
#[inline]
pub(super) fn pixel(v: f64) -> f64 {
    pixel_i(v) as f64
}

// ---- private ----

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
