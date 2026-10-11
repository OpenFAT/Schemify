//! The canvas layer: batched schematic primitives in `i32` world milli-units (D33) drawn through
//! a pan/zoom [`Camera`].
//!
//! # Data design
//!
//! 1. **In → out.** [`CanvasPrims`] (filled by `elab`/`doc` later, synthetic in the demo) +
//!    [`Camera`] + [`Palette`] → one stroke path and one fill path per layer → painter calls.
//! 2. **How many.** 10^2–10^5 visible primitives per frame (D39 budgets 120 fps at 10^5).
//! 3. **How wide.** Coordinates `i32` milli-units (±2.1 M covers the corpus, D33); layer `u8`
//!    (xschem has 22); arc angles `i32` millidegrees; text size `i32` milli-units; screen math
//!    in `f64` (the camera) and `f32` only at the GPU boundary (vello's encoding).
//! 4. **Access.** Every frame reads all coordinates of one primitive kind in one linear pass;
//!    layer decides the batch. So each kind is SoA (`x0`, `y0`, … columns) and batching is by
//!    layer into reused paths: one draw call per (layer, stroke|fill), not per primitive.
//! 5. **Lifetime.** Prims: document lifetime (rebuilt by their owner on edit). Paths and glyph
//!    caches in [`CanvasScratch`]: reused across frames, never freed while the canvas lives.
//! 6. **Parallel.** The per-kind passes are independent (they could fill per-layer paths on
//!    separate threads), but at 10^5 lines the single-threaded build already fits the budget
//!    (see `tests/perf.rs`), so it stays serial.

use vello::Glyph;
use vello::kurbo::{Affine, BezPath, Rect};
use vello::peniko::FontData;

use schemify_ui::TextSystem;
use schemify_ui::parley::PositionedLayoutItem;

use crate::painter::{Chains, Painter, rgba};

/// Number of palette layers.
pub const LAYERS: usize = 32;

/// Pan/zoom: the world point at the view center and the zoom in pixels per milli-unit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// World x (milli-units) at the view center.
    pub cx: f64,
    /// World y (milli-units, y down) at the view center.
    pub cy: f64,
    /// Logical pixels per milli-unit.
    pub scale: f64,
}

impl Camera {
    /// Smallest and largest zoom: 1 px per 10^6 milli-units to 10 px per milli-unit.
    pub const SCALE_MIN: f64 = 1e-6;
    /// Largest zoom.
    pub const SCALE_MAX: f64 = 10.0;

    /// World → screen for a view rectangle `view`.
    #[inline]
    pub fn to_screen(&self, view: Rect, x: i32, y: i32) -> (f64, f64) {
        let (ox, oy) = self.origin(view);
        (
            f64::from(x) * self.scale + ox,
            f64::from(y) * self.scale + oy,
        )
    }

    /// Screen position of world (0, 0): `screen = world · scale + origin`.
    #[inline]
    pub fn origin(&self, view: Rect) -> (f64, f64) {
        (
            view.center().x - self.cx * self.scale,
            view.center().y - self.cy * self.scale,
        )
    }

    /// Screen → world (unrounded milli-units).
    pub fn to_world(&self, view: Rect, sx: f64, sy: f64) -> (f64, f64) {
        let (ox, oy) = self.origin(view);
        ((sx - ox) / self.scale, (sy - oy) / self.scale)
    }

    /// Moves the picture by `(dx, dy)` screen pixels.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.cx -= dx / self.scale;
        self.cy -= dy / self.scale;
    }

    /// Multiplies the zoom by `factor`, keeping the world point under `(sx, sy)` fixed.
    pub fn zoom_at(&mut self, view: Rect, sx: f64, sy: f64, factor: f64) {
        assert!(factor > 0.0 && factor.is_finite(), "bad zoom factor");
        let (wx, wy) = self.to_world(view, sx, sy);
        self.scale = (self.scale * factor).clamp(Self::SCALE_MIN, Self::SCALE_MAX);
        let (nx, ny) = self.to_world(view, sx, sy);
        self.cx += wx - nx;
        self.cy += wy - ny;
    }

    /// Fits world bounds `[x0, y0, x1, y1]` into `view` with a 5% margin.
    pub fn fit(&mut self, view: Rect, b: [i32; 4]) {
        let (w, h) = (
            f64::from(b[2]) - f64::from(b[0]),
            f64::from(b[3]) - f64::from(b[1]),
        );
        assert!(w >= 0.0 && h >= 0.0, "inverted bounds");
        self.cx = (f64::from(b[0]) + f64::from(b[2])) / 2.0;
        self.cy = (f64::from(b[1]) + f64::from(b[3])) / 2.0;
        let s = (view.width() / w.max(1.0)).min(view.height() / h.max(1.0)) * 0.95;
        self.scale = s.clamp(Self::SCALE_MIN, Self::SCALE_MAX);
    }
}

/// Line segments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lines {
    /// Start x.
    pub x0: Vec<i32>,
    /// Start y.
    pub y0: Vec<i32>,
    /// End x.
    pub x1: Vec<i32>,
    /// End y.
    pub y1: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Axis-aligned rectangles (outlined; also filled when the layer's palette entry fills).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rects {
    /// Corner x.
    pub x0: Vec<i32>,
    /// Corner y.
    pub y0: Vec<i32>,
    /// Opposite corner x.
    pub x1: Vec<i32>,
    /// Opposite corner y.
    pub y1: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Circular arcs; a full circle is `sweep = 360_000`. Angles are millidegrees, clockwise on
/// screen (y down), like the orientation transform in format spec §3.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arcs {
    /// Center x.
    pub cx: Vec<i32>,
    /// Center y.
    pub cy: Vec<i32>,
    /// Radius.
    pub r: Vec<i32>,
    /// Start angle.
    pub start: Vec<i32>,
    /// Sweep angle.
    pub sweep: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Point chains: polyline `i` uses points `end[i-1]..end[i]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polys {
    /// One past each chain's last point.
    pub end: Vec<u32>,
    /// Point x.
    pub x: Vec<i32>,
    /// Point y.
    pub y: Vec<i32>,
    /// Palette layer per chain.
    pub layer: Vec<u8>,
}

/// Text labels: top-left anchored, `size` = font size in milli-units.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Texts {
    /// Anchor x.
    pub x: Vec<i32>,
    /// Anchor y.
    pub y: Vec<i32>,
    /// Font size.
    pub size: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
    /// Quarter turns clockwise on screen about the anchor, `[0, 3]`.
    pub rot: Vec<u8>,
    /// One past each label's last byte in `text`.
    pub end: Vec<u32>,
    /// UTF-8 bytes of all labels.
    pub text: String,
    /// Bumped by the writer whenever any label changes; invalidates the shaped-glyph cache.
    pub epoch: u64,
}

/// One canvas's primitives: a table per kind (existence-based; no per-primitive objects).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasPrims {
    /// Segments (wires, symbol lines).
    pub lines: Lines,
    /// Rectangles.
    pub rects: Rects,
    /// Arcs and circles.
    pub arcs: Arcs,
    /// Open polylines.
    pub polylines: Polys,
    /// Closed polygons (filled when the layer fills).
    pub polygons: Polys,
    /// Labels.
    pub texts: Texts,
}

impl CanvasPrims {
    /// Maximum primitives per kind.
    pub const CAPACITY: usize = 1 << 22;

    /// Appends a segment.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        let t = &mut self.lines;
        assert!(t.x0.len() < Self::CAPACITY && usize::from(layer) < LAYERS);
        t.x0.push(x0);
        t.y0.push(y0);
        t.x1.push(x1);
        t.y1.push(y1);
        t.layer.push(layer);
    }

    /// Appends a rectangle.
    pub fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        let t = &mut self.rects;
        assert!(t.x0.len() < Self::CAPACITY && usize::from(layer) < LAYERS);
        t.x0.push(x0);
        t.y0.push(y0);
        t.x1.push(x1);
        t.y1.push(y1);
        t.layer.push(layer);
    }

    /// Appends an arc (`sweep = 360_000` for a circle).
    pub fn arc(&mut self, cx: i32, cy: i32, r: i32, start: i32, sweep: i32, layer: u8) {
        let t = &mut self.arcs;
        assert!(t.cx.len() < Self::CAPACITY && usize::from(layer) < LAYERS && r >= 0);
        t.cx.push(cx);
        t.cy.push(cy);
        t.r.push(r);
        t.start.push(start);
        t.sweep.push(sweep);
        t.layer.push(layer);
    }

    /// Appends a polyline (`closed = false`) or polygon (`closed = true`) of at least 2 points.
    pub fn poly(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8) {
        assert!(
            pts.len() >= 2 && usize::from(layer) < LAYERS,
            "poly needs 2 points"
        );
        let t = if closed {
            &mut self.polygons
        } else {
            &mut self.polylines
        };
        assert!(t.end.len() < Self::CAPACITY);
        t.x.extend(pts.iter().map(|p| p.0));
        t.y.extend(pts.iter().map(|p| p.1));
        t.end.push(t.x.len() as u32);
        t.layer.push(layer);
    }

    /// Appends a label and bumps the text epoch.
    pub fn text(&mut self, x: i32, y: i32, size: i32, layer: u8, s: &str) {
        self.text_rot(x, y, size, layer, s, 0);
    }

    /// Appends a label turned `rot` quarter turns clockwise about its anchor (taken mod 4).
    pub fn text_rot(&mut self, x: i32, y: i32, size: i32, layer: u8, s: &str, rot: u8) {
        let t = &mut self.texts;
        assert!(t.x.len() < Self::CAPACITY && usize::from(layer) < LAYERS && size > 0);
        assert_eq!(t.rot.len(), t.x.len(), "rotation column in step");
        t.x.push(x);
        t.y.push(y);
        t.size.push(size);
        t.layer.push(layer);
        t.rot.push(rot % 4);
        t.text.push_str(s);
        t.end.push(t.text.len() as u32);
        t.epoch += 1;
    }

    /// Total primitive count over all kinds.
    pub fn len(&self) -> usize {
        self.lines.x0.len()
            + self.rects.x0.len()
            + self.arcs.cx.len()
            + self.polylines.end.len()
            + self.polygons.end.len()
            + self.texts.x.len()
    }

    /// True when nothing is drawn.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Per-layer drawing style: variation by data, one row per layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Stroke/fill/text color, `0xRRGGBBAA`.
    pub color: [u32; LAYERS],
    /// Stroke width in logical pixels.
    pub width: [f32; LAYERS],
    /// Bit `l` set: closed shapes on layer `l` are also filled.
    pub fill: u32,
    /// Canvas background.
    pub background: u32,
    /// Grid dots.
    pub grid: u32,
}

impl Palette {
    /// The xschem dark scheme as it appears on screen (colors read off running xschem's
    /// screenshots, layer numbering from its Layers menu: 0 BG, 1 wire, 2 grid, 3 text, 4 symbol,
    /// 5 pin).
    // ponytail: hard-coded rows until the palette is a `ui_styles` selector set (`layer.N`).
    pub fn xschem_dark() -> Palette {
        let mut color = [0x8888_88ff; LAYERS];
        let named: [u32; 22] = [
            0x0000_00ff,
            0x00cc_ffff,
            0x5050_50ff,
            0xd0d0_d0ff,
            0x88dd_00ff,
            0xbb22_00ff,
            0x00a0_ffff,
            0xff70_ffff,
            0xffff_00ff,
            0x00ff_80ff,
            0xff80_00ff,
            0x8080_ffff,
            0xff40_40ff,
            0x40ff_ffff,
            0xc0c0_00ff,
            0xa040_ffff,
            0x40a0_40ff,
            0xffa0_a0ff,
            0xa0a0_ffff,
            0xe0e0_e0ff,
            0x80c0_ffff,
            0xffc0_40ff,
        ];
        color[..22].copy_from_slice(&named);
        Palette {
            color,
            width: [1.0; LAYERS],
            fill: 1 << 5,
            background: 0x0000_00ff,
            grid: 0x7070_70ff,
        }
    }
}

/// Primitive kind for [`Marks`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimKind {
    /// Row of [`CanvasPrims::lines`].
    Line,
    /// Row of [`CanvasPrims::rects`].
    Rect,
    /// Row of [`CanvasPrims::arcs`].
    Arc,
    /// Row of [`CanvasPrims::polylines`].
    Polyline,
    /// Row of [`CanvasPrims::polygons`].
    Polygon,
}

/// Recolored primitives drawn on top: selection highlight, visual-diff colors, annotation
/// markers. One table covers all three; group rows by color for fewer draw calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Which table.
    pub kind: Vec<PrimKind>,
    /// Row in that table.
    pub index: Vec<u32>,
    /// Color, `0xRRGGBBAA`.
    pub color: Vec<u32>,
}

impl Marks {
    /// Appends a mark.
    pub fn push(&mut self, kind: PrimKind, index: u32, color: u32) {
        self.kind.push(kind);
        self.index.push(index);
        self.color.push(color);
    }
}

/// Reused per-frame buffers and the shaped-label cache. Keep one per canvas.
#[derive(Default)]
pub struct CanvasScratch {
    open: Vec<Chains>,
    closed: Vec<Chains>,
    curves: Vec<BezPath>,
    fill: Vec<BezPath>,
    dots: Chains,
    path: BezPath,
    text_epoch: Option<u64>,
    text_face: Option<FontData>,
    text_glyph_end: Vec<u32>,
    text_glyphs: Vec<Glyph>,
}

impl std::fmt::Debug for CanvasScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasScratch")
            .field("glyphs", &self.text_glyphs.len())
            .finish()
    }
}

/// Labels are shaped once at this size and scaled by the camera.
const TEXT_REF_PX: f32 = 64.0;
/// Labels smaller than this on screen are skipped (unreadable, and the cheapest cull).
const TEXT_MIN_PX: f64 = 3.0;

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
    draw_grid(p, view, cam, grid_step, palette, scratch);
    draw_prims(p, view, cam, prims, palette, text, scratch);
    draw_prims(p, view, cam, overlay, palette, text, scratch);
    draw_marks(p, view, cam, prims, marks, scratch);
    p.pop_clip();
}

/// Grid dots every `step` milli-units, doubled until they are ≥ 10 px apart (xschem's rule).
pub fn draw_grid<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    step: i32,
    palette: &Palette,
    scratch: &mut CanvasScratch,
) {
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
        return; // degenerate zoom: no readable grid
    }
    let (ox, oy) = cam.origin(view);
    let dots = &mut scratch.dots;
    dots.clear();
    for j in j0..=j1 {
        let y = (j as f64 * s * cam.scale + oy).floor() as f32;
        for i in i0..=i1 {
            let x = (i as f64 * s * cam.scale + ox).floor() as f32;
            dots.push(&[[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]]);
        }
    }
    p.fill_chains(dots, rgba(palette.grid));
}

fn push_rect(path: &mut BezPath, x0: f64, y0: f64, x1: f64, y1: f64) {
    path.move_to((x0, y0));
    path.line_to((x1, y0));
    path.line_to((x1, y1));
    path.line_to((x0, y1));
    path.close_path();
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

/// Snaps a coordinate to the pixel center so 1 px strokes stay crisp.
#[inline]
fn snap(v: f64) -> f64 {
    v.floor() + 0.5
}

/// Builds one stroke path and one fill path per layer from all primitive tables, then issues
/// at most two draw calls per used layer. Off-screen primitives are culled.
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
    let l = &prims.lines;
    assert!(
        l.x0.len() == l.y0.len()
            && l.x0.len() == l.x1.len()
            && l.x0.len() == l.y1.len()
            && l.x0.len() == l.layer.len()
    );
    let sc = &mut *scratch;
    sc.open.resize_with(LAYERS, Chains::default);
    sc.closed.resize_with(LAYERS, Chains::default);
    sc.curves.resize_with(LAYERS, BezPath::new);
    sc.fill.resize_with(LAYERS, BezPath::new);
    for c in sc.open.iter_mut().chain(sc.closed.iter_mut()) {
        c.clear();
    }
    for path in sc.curves.iter_mut().chain(sc.fill.iter_mut()) {
        path.truncate(0);
    }
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let cull = view.inflate(2.0, 2.0);
    let outside = |ax: f64, ay: f64, bx: f64, by: f64| {
        ax.max(bx) < cull.x0 || ax.min(bx) > cull.x1 || ay.max(by) < cull.y0 || ay.min(by) > cull.y1
    };
    let pt = |x: f64, y: f64| [snap(x) as f32, snap(y) as f32];

    for i in 0..l.x0.len() {
        let (ax, ay) = (f64::from(l.x0[i]) * s + ox, f64::from(l.y0[i]) * s + oy);
        let (bx, by) = (f64::from(l.x1[i]) * s + ox, f64::from(l.y1[i]) * s + oy);
        if outside(ax, ay, bx, by) {
            continue;
        }
        sc.open[usize::from(l.layer[i])].push(&[pt(ax, ay), pt(bx, by)]);
    }

    let r = &prims.rects;
    for i in 0..r.x0.len() {
        let (ax, ay) = (f64::from(r.x0[i]) * s + ox, f64::from(r.y0[i]) * s + oy);
        let (bx, by) = (f64::from(r.x1[i]) * s + ox, f64::from(r.y1[i]) * s + oy);
        if outside(ax, ay, bx, by) {
            continue;
        }
        let layer = usize::from(r.layer[i]);
        let (x0, y0, x1, y1) = (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by));
        sc.closed[layer].push(&[pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)]);
        if palette.fill & (1 << layer) != 0 {
            push_rect(&mut sc.fill[layer], x0, y0, x1, y1);
        }
    }

    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        let (cx, cy, rad) = (
            f64::from(a.cx[i]) * s + ox,
            f64::from(a.cy[i]) * s + oy,
            f64::from(a.r[i]) * s,
        );
        if outside(cx - rad, cy - rad, cx + rad, cy + rad) {
            continue;
        }
        let layer = usize::from(a.layer[i]);
        let (start, sweep) = (
            f64::from(a.start[i]).to_radians() / 1000.0,
            f64::from(a.sweep[i]).to_radians() / 1000.0,
        );
        push_arc(&mut sc.curves[layer], cx, cy, rad, start, sweep);
        if a.sweep[i].abs() >= 360_000 && palette.fill & (1 << layer) != 0 {
            push_arc(&mut sc.fill[layer], cx, cy, rad, start, sweep);
        }
    }

    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            assert!(
                end >= start + 2 && end <= polys.x.len(),
                "malformed poly table"
            );
            let layer = usize::from(polys.layer[k]);
            let chains = if closed {
                &mut sc.closed[layer]
            } else {
                &mut sc.open[layer]
            };
            for j in start..end {
                chains.pts.push(pt(
                    f64::from(polys.x[j]) * s + ox,
                    f64::from(polys.y[j]) * s + oy,
                ));
            }
            chains.end.push(chains.pts.len() as u32);
            if closed && palette.fill & (1 << layer) != 0 {
                let f = &mut sc.fill[layer];
                f.move_to((
                    f64::from(polys.x[start]) * s + ox,
                    f64::from(polys.y[start]) * s + oy,
                ));
                for j in start + 1..end {
                    f.line_to((
                        f64::from(polys.x[j]) * s + ox,
                        f64::from(polys.y[j]) * s + oy,
                    ));
                }
                f.close_path();
            }
            start = end;
        }
    }

    for layer in 0..LAYERS {
        let (color, width) = (rgba(palette.color[layer]), f64::from(palette.width[layer]));
        if !sc.fill[layer].is_empty() {
            p.fill_path(&sc.fill[layer], color);
        }
        if !sc.closed[layer].is_empty() {
            p.stroke_chains(&sc.closed[layer], true, width, color);
        }
        if !sc.open[layer].is_empty() {
            p.stroke_chains(&sc.open[layer], false, width, color);
        }
        if !sc.curves[layer].is_empty() {
            p.stroke_path(&sc.curves[layer], width, color);
        }
    }
    draw_texts(p, view, cam, &prims.texts, palette, text, scratch);
}

fn draw_texts<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    t: &Texts,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    assert_eq!(t.x.len(), t.end.len());
    if scratch.text_epoch != Some(t.epoch) || scratch.text_glyph_end.len() != t.x.len() {
        reshape_texts(t, text, scratch);
    }
    let Some(face) = scratch.text_face.clone() else {
        return;
    };
    let (ox, oy) = cam.origin(view);
    let mut g0 = 0usize;
    for i in 0..t.x.len() {
        let g1 = scratch.text_glyph_end[i] as usize;
        let px = f64::from(t.size[i]) * cam.scale;
        let (x, y) = (
            f64::from(t.x[i]) * cam.scale + ox,
            f64::from(t.y[i]) * cam.scale + oy,
        );
        // A turned label extends left/up of its anchor too: only the size culls it.
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let visible = px >= TEXT_MIN_PX
            && (rot != 0 || (x < view.x1 && y < view.y1 && y + px * 2.0 > view.y0));
        if visible && g1 > g0 {
            let at = Affine::translate((x, y))
                * Affine::rotate(f64::from(rot) * std::f64::consts::FRAC_PI_2)
                * Affine::scale(px / f64::from(TEXT_REF_PX));
            let color = rgba(palette.color[usize::from(t.layer[i])]);
            p.glyphs(
                &face,
                TEXT_REF_PX,
                &[],
                color,
                at,
                &scratch.text_glyphs[g0..g1],
            );
        }
        g0 = g1;
    }
}

/// Shapes every label once at [`TEXT_REF_PX`]; glyph positions are relative to the label's
/// top-left. Called only when the text epoch changes.
fn reshape_texts(t: &Texts, text: &mut TextSystem, scratch: &mut CanvasScratch) {
    scratch.text_face = None;
    scratch.text_glyph_end.clear();
    scratch.text_glyphs.clear();
    let mut layout = schemify_ui::parley::Layout::new();
    let mut start = 0usize;
    for &end in &t.end {
        let s = &t.text[start..end as usize];
        start = end as usize;
        text.shape_into(s, TEXT_REF_PX, &mut layout);
        for line in layout.lines() {
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
        scratch
            .text_glyph_end
            .push(scratch.text_glyphs.len() as u32);
    }
    scratch.text_epoch = Some(t.epoch);
    debug_assert_eq!(scratch.text_glyph_end.len(), t.x.len());
}

/// Redraws marked primitives in their mark color, 1 px wider than the base stroke.
pub fn draw_marks<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    scratch: &mut CanvasScratch,
) {
    assert!(marks.kind.len() == marks.index.len() && marks.kind.len() == marks.color.len());
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let pt = |x: i32, y: i32| (snap(f64::from(x) * s + ox), snap(f64::from(y) * s + oy));
    let mut i = 0;
    // One path per run of equal colors.
    while i < marks.kind.len() {
        let color = marks.color[i];
        let path = &mut scratch.path;
        path.truncate(0);
        while i < marks.kind.len() && marks.color[i] == color {
            let k = marks.index[i] as usize;
            match marks.kind[i] {
                PrimKind::Line => {
                    let l = &prims.lines;
                    path.move_to(pt(l.x0[k], l.y0[k]));
                    path.line_to(pt(l.x1[k], l.y1[k]));
                }
                PrimKind::Rect => {
                    let r = &prims.rects;
                    let (a, b) = (pt(r.x0[k], r.y0[k]), pt(r.x1[k], r.y1[k]));
                    push_rect(path, a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
                }
                PrimKind::Arc => {
                    let a = &prims.arcs;
                    let (cx, cy) = (f64::from(a.cx[k]) * s + ox, f64::from(a.cy[k]) * s + oy);
                    let (start, sweep) = (
                        f64::from(a.start[k]).to_radians() / 1000.0,
                        f64::from(a.sweep[k]).to_radians() / 1000.0,
                    );
                    push_arc(path, cx, cy, f64::from(a.r[k]) * s, start, sweep);
                }
                PrimKind::Polyline | PrimKind::Polygon => {
                    let polys = if marks.kind[i] == PrimKind::Polygon {
                        &prims.polygons
                    } else {
                        &prims.polylines
                    };
                    let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
                    for j in start..polys.end[k] as usize {
                        let q = pt(polys.x[j], polys.y[j]);
                        if j == start {
                            path.move_to(q)
                        } else {
                            path.line_to(q)
                        }
                    }
                    if marks.kind[i] == PrimKind::Polygon {
                        path.close_path();
                    }
                }
            }
            i += 1;
        }
        p.stroke_path(path, 2.0, rgba(color));
    }
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
