//! Schemify's canvas tables: the schematic as batched primitive rows in `i32` world milli-units
//! (D33), the pan/zoom [`Camera`], the per-layer [`Palette`], recolor [`Marks`], the spatial
//! index ([`PrimIndex`]) and the row diff ([`CanvasDiff`]) that let a cached canvas follow an
//! edit in O(change) (D39).
//!
//! No GPU, window or text dependency: the editor fills and edits these tables headless, and
//! `schemify-render` draws them. Their only owner is the producer that fills them (the
//! editor's scene); everyone else reads.
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
//! 5. **Lifetime.** Prims: document lifetime (rebuilt by their owner on edit, killed in place
//!    on removal, [`LAYER_DEAD`]). The index and the diff snapshot live with the cached canvas
//!    until the tables change in a way they cannot follow.
//! 6. **Parallel.** The per-kind tables are independent; the draw passes over them run only
//!    when the raster layer is re-recorded, so they stay serial.

#![forbid(unsafe_code)]

use kurbo::Rect;

mod damage;
mod index;

pub use damage::{CanvasDiff, DIFF_EDITS_MAX, TableChange};
pub use index::{PrimIndex, Rows};

/// Palette layers: every layer index is below it, and [`Palette::fill`] keeps one bit per layer
/// in a `u32`.
pub const LAYERS: usize = 32;

/// The layer of a dead row ([`CanvasPrims::kill`]): not a palette layer, so nothing draws,
/// lists or exports it. A producer that edits its table in place (the editor's scene) kills a
/// removed row instead of shifting or swapping rows, so every other row keeps its index and
/// [`CanvasDiff`] and [`PrimIndex`] follow the edit in O(change).
pub const LAYER_DEAD: u8 = u8::MAX;
const _: () = assert!(LAYER_DEAD as usize >= LAYERS, "dead is no palette layer");

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

/// A primitive's stroke weight, dash and fill (format spec §4 rank 2, `width`, `dash`,
/// `fill true`), packed in a byte: bits 0–1 hold `weight − 1`, bits 2–5 the dash, bit 6 the
/// fill. [`LineStyle::PLAIN`] (weight 1, solid, filled only if its layer fills) is the zero
/// value, so a table filled without styles draws exactly as before.
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

    /// This style, also filling a closed shape (rect, full circle, polygon) in its layer's
    /// colour whatever the layer's [`Palette::fill`] bit says; an open shape ignores it.
    pub fn filled(self) -> LineStyle {
        LineStyle(self.0 | Self::FILL)
    }

    /// True when [`LineStyle::filled`] was applied.
    pub fn fills(self) -> bool {
        self.0 & Self::FILL != 0
    }

    const FILL: u8 = 1 << 6;
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

    /// Marks row `row` of kind `kind` (0 lines, 1 rects, 2 arcs, 3 polylines, 4 polygons, 5
    /// texts: [`CanvasPrims`] field order) dead: its columns stay (a damage box still reads its
    /// old extent), its layer becomes [`LAYER_DEAD`]. Rows are never revived; the producer
    /// compacts by rebuilding its table.
    ///
    /// # Panics
    ///
    /// If `kind` > 5 or `row` is past the table.
    pub fn kill(&mut self, kind: u8, row: usize) {
        let layer = match kind {
            0 => &mut self.lines.layer,
            1 => &mut self.rects.layer,
            2 => &mut self.arcs.layer,
            3 => &mut self.polylines.layer,
            4 => &mut self.polygons.layer,
            5 => &mut self.texts.layer,
            _ => panic!("kind {kind} is not a primitive table"),
        };
        assert!(row < layer.len(), "row {row} of kind {kind} past the table");
        layer[row] = LAYER_DEAD;
    }

    /// Total primitive count over all kinds (dead rows included).
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
    /// are drawn over (`schemify_render::canvas::draw_marks` panics on a row past its table's end).
    pub fn push(&mut self, kind: PrimKind, index: u32, color: u32) {
        self.kind.push(kind);
        self.index.push(index);
        self.color.push(color);
    }
}

/// The points of chain `i` of a CSR table whose `end[i]` is one past its last point.
#[inline]
pub fn chain(end: &[u32], i: usize) -> std::ops::Range<usize> {
    let start = if i == 0 { 0 } else { end[i - 1] as usize };
    start..end[i] as usize
}
