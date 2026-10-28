//! The primitive tables one canvas draws: a SoA table per kind in `i32` world milli-units
//! (D33), appended by their producer (the editor's scene), killed in place on removal
//! ([`LAYER_DEAD`]). Stroke style follows format spec §4 rank 2 (`width`, `dash`,
//! `fill true`); arc angles follow the orientation transform of format spec §3.
//!
//! Contents:
//! - `pub struct Lines { x0, y0, x1, y1, layer, style }`, `pub type Rects = Lines`
//! - `pub struct Arcs { cx, cy, r, start, sweep, layer, style }`
//! - `pub struct Polys { end, x, y, layer, style }`
//! - `pub struct Texts { x, y, size, layer, rot, end, text }`
//! - `pub struct CanvasPrims { lines, rects, arcs, polylines, polygons, texts }`,
//!   `CanvasPrims::CAPACITY`, `line`, `line_styled`, `rect`, `rect_styled`, `arc`,
//!   `arc_styled`, `poly`, `poly_styled`, `text`, `text_rot`, `kill`, `len`, `is_empty`
//! - `pub struct LineStyle(pub u8)`, `LineStyle::PLAIN`, `WEIGHT_MAX`, `DASH_MAX`, `new`,
//!   `weight`, `dash`, `filled`, `fills`

use crate::row::{TABLES, layers_mut};
use crate::{LAYER_DEAD, LAYERS};

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
    /// Radius, non-negative.
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

/// Point chains (CSR): chain `i` uses points [`chain(end, i)`](crate::chain), at least 2.
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
    /// Font size, positive.
    pub size: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
    /// Quarter turns clockwise on screen about the anchor, `[0, 3]`.
    pub rot: Vec<u8>,
    /// One past each label's last byte in `text` (CSR, [`chain`](crate::chain)).
    pub end: Vec<u32>,
    /// UTF-8 bytes of all labels.
    pub text: String,
}

/// One canvas's primitives: a table per kind (existence-based; no per-primitive objects).
/// The kind order here is the `kind` of [`CanvasPrims::kill`] and of every per-kind array in
/// this crate.
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

/// A primitive's stroke weight, dash and fill (format spec §4 rank 2, `width`, `dash`,
/// `fill true`), packed in a byte: bits 0–1 hold `weight − 1`, bits 2–5 the dash, bit 6 the
/// fill. [`LineStyle::PLAIN`] (weight 1, solid, filled only if its layer fills) is the zero
/// value, so a table filled without styles draws exactly as before.
///
/// Both are in screen terms, not world units: a stroke is `weight` × its layer's
/// [`Palette::width`](crate::Palette::width) logical pixels and a dash `n` draws dashes and
/// gaps of `2n` logical pixels each, at every zoom. So a stroke never thins below the layer
/// width when zoomed out, and a body drawn at weight 2 stays twice its wire at every zoom.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash, PartialOrd, Ord)]
pub struct LineStyle(pub u8);

const FILL: u8 = 1 << 6;

const _: () = assert!(LineStyle::WEIGHT_MAX - 1 <= 0b11, "weight fits bits 0–1");
const _: () = assert!(LineStyle::DASH_MAX <= 0b1111, "dash fits bits 2–5");

impl CanvasPrims {
    /// Maximum rows per kind; row indices and CSR offsets fit `u32`.
    pub const CAPACITY: usize = 1 << 22;

    /// Appends a segment in the layer's plain stroke.
    ///
    /// # Panics
    ///
    /// If the table is full or `layer` is not below [`LAYERS`].
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        self.lines.push(x0, y0, x1, y1, layer, LineStyle::PLAIN);
    }

    /// Appends a segment drawn in `style`; panics as [`CanvasPrims::line`].
    pub fn line_styled(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        self.lines.push(x0, y0, x1, y1, layer, style);
    }

    /// Appends a rectangle in the layer's plain stroke; panics as [`CanvasPrims::line`].
    pub fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        self.rects.push(x0, y0, x1, y1, layer, LineStyle::PLAIN);
    }

    /// Appends a rectangle outlined in `style`; panics as [`CanvasPrims::line`].
    pub fn rect_styled(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        self.rects.push(x0, y0, x1, y1, layer, style);
    }

    /// Appends an arc (`sweep = 360_000` for a circle) in the layer's plain stroke.
    ///
    /// # Panics
    ///
    /// As [`CanvasPrims::line`], and if `r` is negative.
    pub fn arc(&mut self, cx: i32, cy: i32, r: i32, start: i32, sweep: i32, layer: u8) {
        self.arc_styled(cx, cy, r, start, sweep, layer, LineStyle::PLAIN);
    }

    /// Appends an arc stroked in `style`; panics as [`CanvasPrims::arc`].
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
        assert!(t.cx.len() < Self::CAPACITY, "arc table full");
        assert!(
            usize::from(layer) < LAYERS,
            "layer {layer} is no palette layer"
        );
        assert!(r >= 0, "negative radius");
        assert_eq!(t.style.len(), t.cx.len(), "style column in step");
        t.cx.push(cx);
        t.cy.push(cy);
        t.r.push(r);
        t.start.push(start);
        t.sweep.push(sweep);
        t.layer.push(layer);
        t.style.push(style);
    }

    /// Appends a polyline (`closed = false`) or polygon (`closed = true`) in the layer's plain
    /// stroke.
    ///
    /// # Panics
    ///
    /// As [`CanvasPrims::line`], and if `pts` has fewer than 2 points.
    pub fn poly(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8) {
        self.poly_styled(pts, closed, layer, LineStyle::PLAIN);
    }

    /// Appends a polyline or polygon stroked in `style`; panics as [`CanvasPrims::poly`].
    pub fn poly_styled(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8, style: LineStyle) {
        assert!(pts.len() >= 2, "poly needs 2 points");
        assert!(
            usize::from(layer) < LAYERS,
            "layer {layer} is no palette layer"
        );
        let t = if closed {
            &mut self.polygons
        } else {
            &mut self.polylines
        };
        assert!(t.end.len() < Self::CAPACITY, "poly table full");
        assert_eq!(t.style.len(), t.end.len(), "style column in step");
        t.x.extend(pts.iter().map(|p| p.0));
        t.y.extend(pts.iter().map(|p| p.1));
        t.end.push(offset(t.x.len()));
        t.layer.push(layer);
        t.style.push(style);
    }

    /// Appends a label.
    ///
    /// # Panics
    ///
    /// As [`CanvasPrims::line`], and if `size` is not positive.
    pub fn text(&mut self, x: i32, y: i32, size: i32, layer: u8, s: &str) {
        self.text_rot(x, y, size, layer, s, 0);
    }

    /// Appends a label turned `rot` quarter turns clockwise about its anchor (taken mod 4);
    /// panics as [`CanvasPrims::text`].
    pub fn text_rot(&mut self, x: i32, y: i32, size: i32, layer: u8, s: &str, rot: u8) {
        let t = &mut self.texts;
        assert!(t.x.len() < Self::CAPACITY, "text table full");
        assert!(
            usize::from(layer) < LAYERS,
            "layer {layer} is no palette layer"
        );
        assert!(size > 0, "font size is positive");
        assert_eq!(t.rot.len(), t.x.len(), "rotation column in step");
        t.x.push(x);
        t.y.push(y);
        t.size.push(size);
        t.layer.push(layer);
        t.rot.push(rot % 4);
        t.text.push_str(s);
        t.end.push(offset(t.text.len()));
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
        let Some(&table) = TABLES.get(usize::from(kind)) else {
            panic!("kind {kind} is not a primitive table");
        };
        let layer = layers_mut(self, table);
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

    /// True when the tables hold no row, dead or alive.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

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
        assert_eq!(s.weight(), w + 1, "weight round-trips");
        assert_eq!(s.dash(), d, "dash round-trips");
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
    /// colour whatever the layer's [`Palette::fill`](crate::Palette::fill) bit says; an open
    /// shape ignores it.
    pub fn filled(self) -> LineStyle {
        LineStyle(self.0 | FILL)
    }

    /// True when [`LineStyle::filled`] was applied.
    pub fn fills(self) -> bool {
        self.0 & FILL != 0
    }
}

// ---- private ----

/// A CSR end offset: below [`CanvasPrims::CAPACITY`] rows a column could still outgrow `u32`
/// (labels of 1 KiB each), so the narrowing is checked.
fn offset(len: usize) -> u32 {
    u32::try_from(len).expect("CSR offset fits u32")
}

impl Lines {
    fn push(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8, style: LineStyle) {
        assert!(self.x0.len() < CanvasPrims::CAPACITY, "table full");
        assert!(
            usize::from(layer) < LAYERS,
            "layer {layer} is no palette layer"
        );
        assert_eq!(self.style.len(), self.x0.len(), "style column in step");
        self.x0.push(x0);
        self.y0.push(y0);
        self.x1.push(x1);
        self.y1.push(y1);
        self.layer.push(layer);
        self.style.push(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the spec's ranges (format spec §4): weight and dash clamp, fill is a separate
    /// bit, and the zero byte is the plain stroke.
    #[test]
    fn line_style_packs_and_clamps() {
        for w in 0..=8 {
            for d in 0..=20 {
                let s = LineStyle::new(w, d);
                assert_eq!(s.weight(), w.clamp(1, 4));
                assert_eq!(s.dash(), d.min(15));
                assert!(!s.fills());
                let f = s.filled();
                assert!(f.fills());
                assert_eq!((f.weight(), f.dash()), (s.weight(), s.dash()));
            }
        }
        assert_eq!(LineStyle::new(1, 0), LineStyle::PLAIN);
    }

    /// Every appender keeps its table's columns in step; a kill changes only the layer.
    #[test]
    fn appends_and_kills_keep_columns_in_step() {
        let mut t = CanvasPrims::default();
        t.line(0, 0, 10, 10, 1);
        t.rect_styled(0, 0, 5, 5, 4, LineStyle::new(2, 3));
        t.arc(0, 0, 7, 0, 360_000, 4);
        t.poly(&[(0, 0), (1, 1), (2, 0)], false, 4);
        t.poly(&[(0, 0), (1, 1), (2, 0)], true, 4);
        t.text_rot(3, 4, 100, 3, "ab", 5);
        assert_eq!(t.len(), 6);
        assert_eq!(t.texts.rot, [1], "rotation taken mod 4");
        assert_eq!((t.polylines.end[0], t.texts.end[0]), (3, 2));
        let before = t.clone();
        t.kill(2, 0);
        assert_eq!(t.arcs.layer, [LAYER_DEAD]);
        assert_eq!(t.arcs.r, before.arcs.r, "columns stay");
        assert_eq!(t.len(), 6, "a dead row is still a row");
    }

    #[test]
    #[should_panic(expected = "not a primitive table")]
    fn kill_refuses_an_unknown_kind() {
        CanvasPrims::default().kill(6, 0);
    }

    #[test]
    #[should_panic(expected = "past the table")]
    fn kill_refuses_a_row_past_the_table() {
        let mut t = CanvasPrims::default();
        t.line(0, 0, 1, 1, 1);
        t.kill(0, 1);
    }
}
