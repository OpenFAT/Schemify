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
//!    The picture itself: one raster layer per canvas ([`draw_cached`]), re-recorded only when
//!    the content key or the zoom changes or a pan leaves its margin, so a steady pan or zoom
//!    frame encodes nothing (D39).
//! 6. **Parallel.** The per-kind passes are independent (they could fill per-layer paths on
//!    separate threads), but they run only when the layer is re-recorded, so they stay serial.

use std::collections::HashMap;

use vello::Glyph;
use vello::kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use vello::peniko::FontData;

use schemify_ui::TextSystem;
use schemify_ui::parley::PositionedLayoutItem;

pub use crate::damage::{CanvasDiff, DIFF_EDITS_MAX};
pub use crate::index::{PrimIndex, Rows};
use crate::painter::{Chains, Painter, push_rect, rgba};
pub use crate::tiles::{CanvasCache, Drawn, PATCH_PAD_PX, TILE_PX, TILES_PER_FRAME, draw_cached};

/// Palette layers: every layer index is below it, and [`Palette::fill`] keeps one bit per layer
/// in a `u32`.
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

/// Two points per row: a line segment, or (as [`Rects`]) the opposite corners of a rectangle.
/// Columns stay equally long.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lines {
    /// First point x.
    pub x0: Vec<i32>,
    /// First point y.
    pub y0: Vec<i32>,
    /// Second point x.
    pub x1: Vec<i32>,
    /// Second point y.
    pub y1: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
    /// Stroke weight and dash, in step with `layer`.
    pub style: Vec<LineStyle>,
}

impl Lines {
    fn push(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        assert!(self.x0.len() < CanvasPrims::CAPACITY && usize::from(layer) < LAYERS);
        assert_eq!(self.style.len(), self.x0.len(), "style column in step");
        self.x0.push(x0);
        self.y0.push(y0);
        self.x1.push(x1);
        self.y1.push(y1);
        self.layer.push(layer);
        self.style.push(style);
    }
}

/// A primitive's stroke weight and dash (format spec §4 rank 2, `width` and `dash`), packed
/// in a byte: bits 0–1 hold `weight − 1`, bits 2–5 the dash. [`LineStyle::PLAIN`] (weight 1,
/// solid) is the zero value, so a table filled without styles draws exactly as before.
///
/// Both are in screen terms, not world units: a stroke is `weight` × its layer's
/// [`Palette::width`] logical pixels and a dash `n` draws dashes and gaps of `2n` logical
/// pixels each, at every zoom. So a stroke never thins below the layer width when zoomed out,
/// and a body drawn at weight 2 stays twice its wire at every zoom.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash, PartialOrd, Ord)]
pub struct LineStyle(pub u8);

impl LineStyle {
    /// Weight 1, solid: the layer's own stroke.
    pub const PLAIN: LineStyle = LineStyle(0);
    /// Heaviest weight.
    pub const WEIGHT_MAX: u8 = 4;
    /// Longest dash.
    pub const DASH_MAX: u8 = 15;

    /// The style of weight `weight` (clamped to `[1, WEIGHT_MAX]`) and dash `dash` (0 = solid,
    /// clamped to `DASH_MAX`). Clamping, not failing: the file stores any number (format spec
    /// §4) and the renderer draws the nearest it can.
    pub fn new(weight: u8, dash: u8) -> LineStyle {
        let w = weight.clamp(1, Self::WEIGHT_MAX) - 1;
        let d = dash.min(Self::DASH_MAX);
        let s = LineStyle(w | (d << 2));
        assert!(s.weight() == w + 1 && s.dash() == d);
        s
    }

    /// Stroke weight, `[1, WEIGHT_MAX]`, in multiples of the layer width.
    pub fn weight(self) -> u8 {
        (self.0 & 3) + 1
    }

    /// Dash, `[0, DASH_MAX]`; 0 is solid.
    pub fn dash(self) -> u8 {
        (self.0 >> 2) & 15
    }
}

/// Axis-aligned rectangles between two opposite corners (outlined; also filled when the
/// layer's palette entry fills).
pub type Rects = Lines;

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
    /// Stroke weight and dash.
    pub style: Vec<LineStyle>,
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
    /// Stroke weight and dash per chain.
    pub style: Vec<LineStyle>,
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

    /// Appends a segment in the layer's plain stroke.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        self.lines.push(x0, y0, x1, y1, layer, LineStyle::PLAIN);
    }

    /// Appends a segment drawn in `style`.
    pub fn line_styled(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        self.lines.push(x0, y0, x1, y1, layer, style);
    }

    /// Appends a rectangle in the layer's plain stroke.
    pub fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        self.rects.push(x0, y0, x1, y1, layer, LineStyle::PLAIN);
    }

    /// Appends a rectangle outlined in `style`.
    pub fn rect_styled(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        self.rects.push(x0, y0, x1, y1, layer, style);
    }

    /// Appends an arc (`sweep = 360_000` for a circle) in the layer's plain stroke.
    pub fn arc(&mut self, cx: i32, cy: i32, r: i32, start: i32, sweep: i32, layer: u8) {
        self.arc_styled(cx, cy, r, start, sweep, layer, LineStyle::PLAIN);
    }

    /// Appends an arc stroked in `style`.
    #[allow(clippy::too_many_arguments)]
    pub fn arc_styled(
        &mut self,
        cx: i32,
        cy: i32,
        r: i32,
        start: i32,
        sweep: i32,
        layer: u8,
        style: LineStyle,
    ) {
        let t = &mut self.arcs;
        assert!(t.cx.len() < Self::CAPACITY && usize::from(layer) < LAYERS && r >= 0);
        assert_eq!(t.style.len(), t.cx.len(), "style column in step");
        t.cx.push(cx);
        t.cy.push(cy);
        t.r.push(r);
        t.start.push(start);
        t.sweep.push(sweep);
        t.layer.push(layer);
        t.style.push(style);
    }

    /// Appends a polyline (`closed = false`) or polygon (`closed = true`) of at least 2 points
    /// in the layer's plain stroke.
    pub fn poly(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8) {
        self.poly_styled(pts, closed, layer, LineStyle::PLAIN);
    }

    /// Appends a polyline or polygon stroked in `style`.
    pub fn poly_styled(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8, style: LineStyle) {
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
        assert_eq!(t.style.len(), t.end.len(), "style column in step");
        t.x.extend(pts.iter().map(|p| p.0));
        t.y.extend(pts.iter().map(|p| p.1));
        t.end.push(t.x.len() as u32);
        t.layer.push(layer);
        t.style.push(style);
    }

    /// Appends a label.
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
    /// Appends a mark. `index` must be a row of `kind`'s table in the [`CanvasPrims`] the marks
    /// are drawn over ([`draw_marks`] panics on a row past its table's end).
    pub fn push(&mut self, kind: PrimKind, index: u32, color: u32) {
        self.kind.push(kind);
        self.index.push(index);
        self.color.push(color);
    }
}

/// Reused per-frame buffers and the shaped-label cache (glyphs per distinct string, shared by
/// every primitive table drawn through it). Keep one per canvas.
#[derive(Default)]
pub struct CanvasScratch {
    open: Vec<Chains>,
    closed: Vec<Chains>,
    curves: Vec<BezPath>,
    fill: Vec<BezPath>,
    /// Strokes of rows not in [`LineStyle::PLAIN`], one path per (layer, style) seen so far.
    styled: Vec<(u8, LineStyle, BezPath)>,
    dots: Chains,
    path: BezPath,
    lod: Lod,
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

/// Primitives whose screen box is at most this many logical px each way are drawn as the whole
/// pixels that box touches (level of detail, D39): at a far zoom a row costs a few bit sets
/// instead of path segments, and below two pixels a stroke and a block look alike. A function
/// of the zoom and the row only, so a tile, a patch and the whole view draw a row alike.
pub const LOD_PX: f64 = 2.0;
/// Cost units a row drawn as a path or label adds over one culled or drawn as pixels (it is
/// about that many times slower: path encoding against a few bit sets).
pub const PATH_COST: usize = 6;
/// Most pixels a level-of-detail mask covers (2 MiB per layer); larger views draw every row
/// as a path.
const LOD_AREA_MAX: i64 = 1 << 24;

/// Per-layer pixel masks of the [`LOD_PX`]-small rows of one view, drawn as horizontal runs.
#[derive(Default)]
struct Lod {
    /// The mask's pixel (0, 0) in view px, its size in px (0 = off) and words per mask row.
    x0: i64,
    y0: i64,
    w: i64,
    h: i64,
    wpr: usize,
    /// Per layer, `wpr · h` words once used; all zero between draws.
    bits: Vec<Vec<u64>>,
    /// Layers with a set bit.
    used: u32,
    runs: Chains,
}

impl Lod {
    /// Starts the masks of `view` (empty: the previous draw flushed every layer).
    fn begin(&mut self, view: Rect) {
        assert_eq!(self.used, 0, "every layer flushed");
        let (x0, y0) = (view.x0.floor() as i64, view.y0.floor() as i64);
        let (w, h) = (view.x1.ceil() as i64 - x0, view.y1.ceil() as i64 - y0);
        let on = w > 0 && h > 0 && w.saturating_mul(h) <= LOD_AREA_MAX;
        (self.x0, self.y0) = (x0, y0);
        (self.w, self.h) = if on { (w, h) } else { (0, 0) };
        self.wpr = (self.w as usize).div_ceil(64);
        self.bits.resize_with(LAYERS, Vec::new);
    }

    /// Marks the pixels of screen box `[x0, x1] × [y0, y1]` in `layer` when it is
    /// [`LOD_PX`]-small; false (nothing marked) when it is larger or the masks are off.
    #[inline(always)]
    fn mark(&mut self, layer: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
        debug_assert!(x0 <= x1 && y0 <= y1);
        if self.w == 0 || x1 - x0 > LOD_PX || y1 - y0 > LOD_PX {
            return false;
        }
        let (a, b) = (pixel_i(x0) - self.x0, pixel_i(x1) - self.x0);
        let (c, d) = (pixel_i(y0) - self.y0, pixel_i(y1) - self.y0);
        if b < 0 || d < 0 || a >= self.w || c >= self.h {
            return true; // wholly outside: nothing to show
        }
        let (a, b) = (a.max(0) as usize, b.min(self.w - 1) as usize);
        let (c, d) = (c.max(0) as usize, d.min(self.h - 1) as usize);
        if self.used & (1 << layer) == 0 {
            self.used |= 1 << layer;
            self.bits[layer].resize(self.wpr * self.h as usize, 0);
        }
        // At most 3 px across: the bits of a row sit in one word or two.
        let (wa, wb) = (a / 64, b / 64);
        let (ma, mb) = (!0u64 << (a % 64), !0u64 >> (63 - b % 64));
        let bits = &mut self.bits[layer];
        for y in c..=d {
            let row = y * self.wpr;
            if wa == wb {
                bits[row + wa] |= ma & mb;
            } else {
                bits[row + wa] |= ma;
                bits[row + wb] |= mb;
            }
        }
        true
    }

    /// Fills `layer`'s marked pixels in `color` (one rect per horizontal run) and clears them.
    ///
    /// Cost: O(mask words + runs).
    fn flush<P: Painter>(&mut self, p: &mut P, layer: usize, color: vello::peniko::Color) {
        if self.used & (1 << layer) == 0 {
            return;
        }
        self.used &= !(1 << layer);
        self.runs.clear();
        let wpr = self.wpr;
        let bits = &mut self.bits[layer];
        for y in 0..self.h as usize {
            let row = &mut bits[y * wpr..(y + 1) * wpr];
            let (fy, ly) = ((self.y0 + y as i64) as f32, (self.y0 + y as i64 + 1) as f32);
            let run = |runs: &mut Chains, a: usize, b: usize| {
                let (fa, fb) = ((self.x0 + a as i64) as f32, (self.x0 + b as i64) as f32);
                runs.push(&[[fa, fy], [fb, fy], [fb, ly], [fa, ly]]);
            };
            let mut start: Option<usize> = None;
            for (i, word) in row.iter_mut().enumerate() {
                let (m, base) = (std::mem::take(word), i * 64);
                let mut at = 0u32;
                // Bits below `at` are done; a run open at a word's end continues into the next.
                while at < 64 {
                    let rest = m >> at;
                    match start {
                        None if rest == 0 => break,
                        None => {
                            at += rest.trailing_zeros();
                            start = Some(base + at as usize);
                        }
                        Some(a) => {
                            at += (!rest).trailing_zeros();
                            if at < 64 {
                                run(&mut self.runs, a, base + at as usize);
                                start = None;
                            }
                        }
                    }
                }
            }
            if let Some(a) = start {
                run(&mut self.runs, a, self.w as usize);
            }
        }
        assert!(self.runs.end.len() <= (self.w * self.h) as usize);
        p.fill_chains(&self.runs, color);
    }
}

/// Labels are shaped once at this size and scaled by the camera.
const TEXT_REF_PX: f32 = 64.0;
/// Labels smaller than this on screen are skipped (unreadable, and the cheapest cull).
const TEXT_MIN_PX: f64 = 3.0;
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
        let y = pixel(j as f64 * s * cam.scale + oy) as f32;
        for i in i0..=i1 {
            let x = pixel(i as f64 * s * cam.scale + ox) as f32;
            dots.push(&[[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]]);
        }
    }
    p.fill_chains(dots, rgba(palette.grid));
}

/// The points of chain `i` of a CSR table whose `end[i]` is one past its last point.
#[inline]
pub(crate) fn chain(end: &[u32], i: usize) -> std::ops::Range<usize> {
    let start = if i == 0 { 0 } else { end[i - 1] as usize };
    start..end[i] as usize
}

/// Start and sweep of arc `i` in radians.
#[inline]
fn arc_angles(a: &Arcs, i: usize) -> (f64, f64) {
    let rad = |m: i32| f64::from(m).to_radians() / 1000.0;
    (rad(a.start[i]), rad(a.sweep[i]))
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
/// coordinate that lands on a pixel edge on the same side whether its camera origin was
/// computed directly or rebuilt for a cached layer (they differ by rounding, ~1e-9 px).
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
/// baseline x86-64 (no SSE4.1), which cost ~10% of a far-zoom tile. Exact for |v| < 2^63
/// (screen coordinates are far inside).
#[inline]
fn pixel_i(v: f64) -> i64 {
    let v = v + 1e-6;
    let t = v as i64;
    t - i64::from((t as f64) > v)
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
) -> usize {
    draw_prims_in(p, view, cam, prims, None, palette, text, scratch)
}

/// [`draw_prims`] over the rows `rows` lists (ascending per kind, e.g. a
/// [`PrimIndex::query`] of the view's world box), or every row for `None`: what it draws
/// inside `view` is the same when `rows` holds every row that reaches into it.
///
/// # Time complexity
///
/// O(listed rows + glyphs of visible labels). Returns the cost (for [`draw_cached`]'s
/// per-frame budget): 1 per row looked at, [`PATH_COST`] more per row drawn as a path or
/// label (~15 ns per unit on a 2024 laptop).
#[allow(clippy::too_many_arguments)]
pub fn draw_prims_in<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    rows: Option<&Rows>,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) -> usize {
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
    for b in &mut sc.styled {
        b.2.truncate(0);
    }
    assert!(
        l.style.len() == l.x0.len()
            && prims.rects.style.len() == prims.rects.x0.len()
            && prims.arcs.style.len() == prims.arcs.cx.len()
            && prims.polylines.style.len() == prims.polylines.end.len()
            && prims.polygons.style.len() == prims.polygons.end.len(),
        "style columns in step"
    );
    sc.lod.begin(view);
    let mut paths = 0; // rows drawn as paths or glyph runs (the costly ones)
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let cull = view.inflate(2.0, 2.0);
    let outside = |ax: f64, ay: f64, bx: f64, by: f64| {
        ax.max(bx) < cull.x0 || ax.min(bx) > cull.x1 || ay.max(by) < cull.y0 || ay.min(by) > cull.y1
    };
    let pt = |x: f64, y: f64| [snap(x) as f32, snap(y) as f32];

    for i in each(l.x0.len(), rows.map(|r| &r.lines[..])) {
        let (ax, ay) = (f64::from(l.x0[i]) * s + ox, f64::from(l.y0[i]) * s + oy);
        let (bx, by) = (f64::from(l.x1[i]) * s + ox, f64::from(l.y1[i]) * s + oy);
        let layer = usize::from(l.layer[i]);
        if outside(ax, ay, bx, by)
            || sc
                .lod
                .mark(layer, ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
        {
            continue;
        }
        paths += 1;
        let st = l.style[i];
        if st != LineStyle::PLAIN {
            let (path, o) = styled(&mut sc.styled, layer, st, palette);
            path.move_to(pt_at(ax, ay, o));
            path.line_to(pt_at(bx, by, o));
            continue;
        }
        sc.open[layer].push(&[pt(ax, ay), pt(bx, by)]);
    }

    let r = &prims.rects;
    for i in each(r.x0.len(), rows.map(|r| &r.rects[..])) {
        let (ax, ay) = (f64::from(r.x0[i]) * s + ox, f64::from(r.y0[i]) * s + oy);
        let (bx, by) = (f64::from(r.x1[i]) * s + ox, f64::from(r.y1[i]) * s + oy);
        let layer = usize::from(r.layer[i]);
        let (x0, y0, x1, y1) = (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by));
        if outside(ax, ay, bx, by) || sc.lod.mark(layer, x0, y0, x1, y1) {
            continue;
        }
        paths += 1;
        if palette.fill & (1 << layer) != 0 {
            push_rect(&mut sc.fill[layer], Rect::new(x0, y0, x1, y1));
        }
        let st = r.style[i];
        if st != LineStyle::PLAIN {
            let (path, o) = styled(&mut sc.styled, layer, st, palette);
            let (a, b) = (pt_at(x0, y0, o), pt_at(x1, y1, o));
            push_rect(path, Rect::new(a.0, a.1, b.0, b.1));
            continue;
        }
        sc.closed[layer].push(&[pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)]);
    }

    let a = &prims.arcs;
    for i in each(a.cx.len(), rows.map(|r| &r.arcs[..])) {
        let (cx, cy, rad) = (
            f64::from(a.cx[i]) * s + ox,
            f64::from(a.cy[i]) * s + oy,
            f64::from(a.r[i]) * s,
        );
        let layer = usize::from(a.layer[i]);
        if outside(cx - rad, cy - rad, cx + rad, cy + rad)
            || sc.lod.mark(layer, cx - rad, cy - rad, cx + rad, cy + rad)
        {
            continue;
        }
        let (start, sweep) = arc_angles(a, i);
        let st = a.style[i];
        let curves = if st == LineStyle::PLAIN {
            &mut sc.curves[layer]
        } else {
            styled(&mut sc.styled, layer, st, palette).0
        };
        push_arc(curves, cx, cy, rad, start, sweep);
        paths += 1;
        if a.sweep[i].abs() >= 360_000 && palette.fill & (1 << layer) != 0 {
            push_arc(&mut sc.fill[layer], cx, cy, rad, start, sweep);
        }
    }

    for (polys, closed, listed) in [
        (&prims.polylines, false, rows.map(|r| &r.polylines[..])),
        (&prims.polygons, true, rows.map(|r| &r.polygons[..])),
    ] {
        for k in each(polys.end.len(), listed) {
            let std::ops::Range { start, end } = chain(&polys.end, k);
            assert!(
                end >= start + 2 && end <= polys.x.len(),
                "malformed poly table"
            );
            let layer = usize::from(polys.layer[k]);
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for j in start..end {
                let (x, y) = (
                    f64::from(polys.x[j]) * s + ox,
                    f64::from(polys.y[j]) * s + oy,
                );
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            if outside(x0, y0, x1, y1) || sc.lod.mark(layer, x0, y0, x1, y1) {
                continue;
            }
            let st = polys.style[k];
            if st == LineStyle::PLAIN {
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
            } else {
                let (path, o) = styled(&mut sc.styled, layer, st, palette);
                for j in start..end {
                    let q = pt_at(
                        f64::from(polys.x[j]) * s + ox,
                        f64::from(polys.y[j]) * s + oy,
                        o,
                    );
                    if j == start {
                        path.move_to(q);
                    } else {
                        path.line_to(q);
                    }
                }
                if closed {
                    path.close_path();
                }
            }
            paths += 1;
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
        for (_, st, path) in sc.styled.iter().filter(|b| usize::from(b.0) == layer) {
            if !path.is_empty() {
                p.stroke_styled(path, &stroke_of(*st, width), color);
            }
        }
        sc.lod.flush(p, layer, color);
    }
    assert_eq!(sc.lod.used, 0, "every mask drawn");
    let listed = rows.map(|r| &r.texts[..]);
    paths += draw_texts(p, view, cam, &prims.texts, listed, palette, text, scratch);
    rows.map_or(prims.len(), Rows::len) + PATH_COST * paths
}

/// The stroke of `style` over a layer `width` logical px wide: `weight × width`, round caps
/// and joins (a thick zigzag mitres into spikes), dashes and gaps of `2 · dash` px.
pub fn stroke_of(style: LineStyle, width: f64) -> Stroke {
    assert!(width > 0.0, "zero-width layer");
    let w = width * f64::from(style.weight());
    let stroke = Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round);
    let d = 2.0 * f64::from(style.dash());
    let out = if d > 0.0 {
        stroke.with_dashes(0.0, [d, d])
    } else {
        stroke
    };
    assert!(out.width >= width);
    out
}

/// The path that collects `layer`'s rows in `style` (made on first use; at most one per pair,
/// so the list stays as short as the distinct styles drawn) and the pixel offset its points
/// snap to: a pixel center for an odd stroke width, the pixel edge for an even one, so both
/// stay crisp.
fn styled<'a>(
    buckets: &'a mut Vec<(u8, LineStyle, BezPath)>,
    layer: usize,
    style: LineStyle,
    palette: &Palette,
) -> (&'a mut BezPath, f64) {
    assert!(layer < LAYERS && style != LineStyle::PLAIN);
    let w = f64::from(palette.width[layer]) * f64::from(style.weight());
    let o = if (w.round() as i64) % 2 == 0 {
        0.0
    } else {
        0.5
    };
    let l = layer as u8;
    let at = match buckets.iter().position(|b| b.0 == l && b.1 == style) {
        Some(at) => at,
        None => {
            assert!(buckets.len() < LAYERS * 64, "styles are a byte per layer");
            buckets.push((l, style, BezPath::new()));
            buckets.len() - 1
        }
    };
    (&mut buckets[at].2, o)
}

/// A screen point snapped to whole pixels plus `o` (see [`styled`]).
#[inline]
fn pt_at(x: f64, y: f64, o: f64) -> (f64, f64) {
    (pixel(x) + o, pixel(y) + o)
}

/// Every row `0..n`, or the listed ones.
fn each(n: usize, rows: Option<&[u32]>) -> impl Iterator<Item = usize> + '_ {
    let all = rows.is_none().then_some(0..n).into_iter().flatten();
    all.chain(rows.into_iter().flatten().map(|&i| i as usize))
}

#[allow(clippy::too_many_arguments)]
fn draw_texts<P: Painter>(
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
                    push_rect(path, Rect::new(a.0, a.1, b.0, b.1).abs());
                }
                PrimKind::Arc => {
                    let a = &prims.arcs;
                    let (cx, cy) = (f64::from(a.cx[k]) * s + ox, f64::from(a.cy[k]) * s + oy);
                    let (start, sweep) = arc_angles(a, k);
                    push_arc(path, cx, cy, f64::from(a.r[k]) * s, start, sweep);
                }
                PrimKind::Polyline | PrimKind::Polygon => {
                    let polys = if marks.kind[i] == PrimKind::Polygon {
                        &prims.polygons
                    } else {
                        &prims.polylines
                    };
                    let pts = chain(&polys.end, k);
                    let start = pts.start;
                    for j in pts {
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
