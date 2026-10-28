//! The canvas layer on the CPU: schematic primitives in `i32` world milli-units (D33) in,
//! painter calls through a pan/zoom [`Camera`] out, from the same instance rows the GPU
//! pipelines draw ([`crate::gpu::InstanceRows`]). The rows are drawn in `rows`, the labels in
//! `text`; this file holds the diagonal route ([`draw_canvas`]) and the grid.
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
//!
//! Contents:
//! - `pub struct CanvasScratch`, `pub struct GridDots`
//! - `pub const LOD_PX`, `pub const TEXT_MIN_PX`
//! - `pub fn draw_canvas`, `draw_grid`, `grid_dots`, `draw_prims`, `draw_marks`
//! - `pub use rows::{Paths, draw_rows}`, `pub use text::draw_texts`

mod rows;
mod text;

use std::collections::HashMap;

use kurbo::Rect;
use peniko::FontData;
use vello_cpu::Glyph;

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
use schemify_ui::TextSystem;

use crate::gpu::{self, Change, InstanceRows};
use crate::painter::{self, Painter};

pub use rows::{Paths, draw_rows};
pub use text::draw_texts;

/// Reused per-frame buffers, the rows of the tables drawn through it, and the shaped-label
/// cache (glyphs per distinct string, shared by every primitive table drawn through it). Keep
/// one per canvas.
#[derive(Default)]
pub struct CanvasScratch {
    /// The paths [`draw_rows`] batches into.
    pub paths: Paths,
    rows: InstanceRows,
    pub(super) text_face: Option<FontData>,
    /// Glyph range in `text_glyphs` of each string shaped so far.
    pub(super) text_runs: HashMap<Box<str>, (u32, u32)>,
    pub(super) text_glyphs: Vec<Glyph>,
    pub(super) text_layout: schemify_ui::parley::Layout<()>,
}

/// The dots of a readable grid ([`grid_dots`]): dot `(i, j)` sits at world
/// `(i · pitch, j · pitch)`, for `i` in `i_first..=i_last` and `j` in `j_first..=j_last`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridDots {
    /// Milli-units between neighbouring dots.
    pub pitch: f64,
    /// The first dot column in view.
    pub i_first: i64,
    /// The last dot column in view (`≥ i_first`).
    pub i_last: i64,
    /// The first dot row in view.
    pub j_first: i64,
    /// The last dot row in view (`≥ j_first`).
    pub j_last: i64,
}

/// Primitives whose screen box is at most this many logical px each way are drawn as the whole
/// pixels that box touches (level of detail, D39): below two pixels a stroke and a block look
/// alike, and a block is cheap. A function of the camera and the row only (a chain on the CPU,
/// as one path), as `shaders/canvas/camera.wgsl` cuts it.
pub const LOD_PX: f64 = 2.0;
/// Labels smaller than this on screen are skipped (unreadable, and the cheapest cull).
pub const TEXT_MIN_PX: f64 = 3.0;

impl std::fmt::Debug for CanvasScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasScratch")
            .field("strings", &self.text_runs.len())
            .field("glyphs", &self.text_glyphs.len())
            .finish()
    }
}

/// Draws background, grid, primitives, overlay primitives and marks into `view`, clipped.
/// The diagonal route for the common case; each step is also public.
///
/// # Panics
///
/// Panics on `grid_step ≤ 0`, a camera that does not scale up, or a view that is not finite.
#[allow(
    clippy::too_many_arguments,
    reason = "the diagonal route takes every step's inputs; each step is public on its own"
)]
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
    assert!(grid_step > 0, "grid step must be positive");
    assert!(cam.scale > 0.0, "a drawable camera, scale {}", cam.scale);
    p.push_clip(view);
    p.fill_rect(view, painter::rgba(palette.background));
    draw_grid(p, view, cam, grid_step, palette.grid, &mut scratch.paths);
    draw_prims(p, view, cam, prims, palette, text, scratch);
    draw_prims(p, view, cam, overlay, palette, text, scratch);
    draw_marks(p, view, cam, prims, marks, scratch);
    p.pop_clip();
}

/// Grid dots every `step` milli-units in `color` (`0xRRGGBBAA`), doubled until they are ≥ 10
/// px apart (xschem's rule).
///
/// # Panics
///
/// Panics on `step ≤ 0`.
pub fn draw_grid<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    step: i32,
    color: u32,
    paths: &mut Paths,
) {
    let Some(GridDots {
        pitch: s,
        i_first: i0,
        i_last: i1,
        j_first: j0,
        j_last: j1,
    }) = grid_dots(view, cam, step)
    else {
        return;
    };
    let (ox, oy) = cam.origin(view);
    let dots = &mut paths.dots;
    dots.clear();
    for j in j0..=j1 {
        let y = rows::pixel(j as f64 * s * cam.scale + oy) as f32;
        for i in i0..=i1 {
            let x = rows::pixel(i as f64 * s * cam.scale + ox) as f32;
            dots.push(&[[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]]);
        }
    }
    let n = (i1 - i0 + 1) * (j1 - j0 + 1);
    debug_assert_eq!(dots.end.len() as i64, n, "a dot per grid point in view");
    p.fill_chains(dots, painter::rgba(color));
}

/// The grid `step` milli-units apart as `view` shows it through `cam`: the dot pitch in
/// milli-units (doubled until the dots are ≥ 10 px apart, xschem's rule) and the dot indices
/// in view. `None` when there is no readable grid: a degenerate zoom, or more than 200 000
/// dots.
///
/// # Panics
///
/// Panics on `step ≤ 0`.
#[must_use]
pub fn grid_dots(view: Rect, cam: &Camera, step: i32) -> Option<GridDots> {
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
    assert!(s >= f64::from(step), "the pitch only doubles");
    debug_assert!(i0 <= i1, "a dot column in view");
    debug_assert!(j0 <= j1, "a dot row in view");
    Some(GridDots {
        pitch: s,
        i_first: i0,
        i_last: i1,
        j_first: j0,
        j_last: j1,
    })
}

/// Draws the primitives of `prims` (packed into `scratch`'s rows, then [`draw_rows`]) and their
/// labels ([`draw_texts`]), culled to `view`: the immediate route (the gesture overlay, PNG
/// export, goldens) for a table no painter keeps rows of.
///
/// # Time complexity
///
/// O(primitives + glyphs of visible labels); allocation-free once `scratch` has grown.
///
/// # Panics
///
/// Panics on a table past [`CanvasPrims::CAPACITY`], a camera that does not scale up, or a
/// view that is not finite.
pub fn draw_prims<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    assert!(cam.scale > 0.0, "a drawable camera, scale {}", cam.scale);
    scratch.rows.pack(0, prims, palette, Change::All);
    debug_assert!(
        scratch.rows.count(0, gpu::LINE) <= prims.lines.x0.len(),
        "a slot per live line at most"
    );
    draw_rows(p, view, cam, &scratch.rows, 0, &mut scratch.paths);
    draw_texts(p, view, cam, &prims.texts, None, palette, text, scratch);
}

/// Redraws the marked rows of `prims` in their mark colour, 2 px wide, one stroke per run of
/// one colour (packed into `scratch`'s rows, then [`draw_rows`]).
///
/// # Panics
///
/// Panics on a mark past its table's end or mark columns of different lengths.
pub fn draw_marks<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    scratch: &mut CanvasScratch,
) {
    assert!(cam.scale > 0.0, "a drawable camera, scale {}", cam.scale);
    scratch.rows.pack_marks(prims, marks, /* changed */ true);
    debug_assert_eq!(
        scratch.rows.count(gpu::MARKS, gpu::TRI),
        0,
        "a mark never fills"
    );
    draw_rows(p, view, cam, &scratch.rows, gpu::MARKS, &mut scratch.paths);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Affine, BezPath, Stroke};
    use schemify_canvas::{LineStyle, PrimKind};

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
        let px = p.finish(painter::rgba(0x0000_00ff));
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
