//! One drawing interface over two backends (D5): painter calls in, pixels or instances out.
//! [`crate::batch::GpuPainter`] turns calls into the frame batch, [`crate::CpuPainter`]
//! rasterizes them with `vello_cpu` (golden images, the GPU-less fallback). Monomorphized, so
//! the interface costs nothing at run time. Also the flat polyline table every bulk stroke
//! goes through ([`Chains`]) and the small path helpers both backends' callers share.
//!
//! Contents:
//! - `pub trait Painter`
//! - `pub struct Chains`, `Chains::push`, `Chains::clear`, `Chains::is_empty`, `Chains::to_path`
//! - `pub fn rgba(v: u32) -> Color`
//! - `pub(crate) fn push_rect(path: &mut BezPath, r: Rect)`
//! - `pub(crate) fn outline<P: Painter>(p: &mut P, path: &mut BezPath, r: Rect, w: f64, color: Color)`
//! - `pub(crate) fn image_data(cache: &mut Vec<ImageData>, image: &schemify_ui::Image) -> ImageData`

use kurbo::{Affine, BezPath, Rect, Stroke};
use peniko::{Color, FontData, ImageData};
use vello_cpu::Glyph;

use schemify_canvas::{CanvasPrims, Marks, Palette};

use crate::gpu::{Change, Frame};
use crate::widgets::UiRow;

/// The drawing calls every frame builder uses. Coordinates are logical pixels; each painter
/// applies its own base transform (the window's scale factor).
pub trait Painter {
    /// Fills an axis-aligned rectangle.
    fn fill_rect(&mut self, r: Rect, color: Color);
    /// Fills a path (non-zero winding).
    fn fill_path(&mut self, path: &BezPath, color: Color);
    /// Strokes a path with round caps and joins (kurbo's `Stroke::new`); `width` in logical
    /// pixels.
    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color);
    /// Strokes a path in a full style (width in logical pixels, caps, joins, dashes): the
    /// canvas's weighted and dashed primitives.
    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color);
    /// Draws glyphs of one font at `px` pixels, positioned in the space of `at`.
    fn glyphs(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        color: Color,
        at: Affine,
        glyphs: &[Glyph],
    );
    /// Clips subsequent drawing to `r` until the matching [`Painter::pop_clip`].
    fn push_clip(&mut self, r: Rect);
    /// Ends the innermost clip.
    fn pop_clip(&mut self);

    /// Strokes every chain as one path (`closed` closes each chain). The canvas's bulk route:
    /// a backend may encode straight from the `f32` points instead of building a [`BezPath`].
    fn stroke_chains(&mut self, chains: &Chains, closed: bool, width: f64, color: Color) {
        self.stroke_path(&chains.to_path(closed), width, color);
    }

    /// Fills every chain as a closed polygon (non-zero winding), all in one path.
    fn fill_chains(&mut self, chains: &Chains, color: Color) {
        self.fill_path(&chains.to_path(true), color);
    }

    /// Device pixels per logical pixel (the base transform's scale).
    fn device_scale(&self) -> f64 {
        1.0
    }

    /// Draws `image` (premultiplied RGBA8, as prose images are) scaled into `r`. A backend
    /// without images fills `r` with a faint grey instead.
    fn image(&mut self, image: &ImageData, r: Rect) {
        debug_assert!(image.width > 0, "an image has pixels");
        debug_assert!(image.height > 0, "an image has pixels");
        self.fill_rect(r, Color::from_rgba8(128, 128, 128, 48));
    }

    /// Starts this frame's canvas: `frame`'s background and grid, and every call after it
    /// clipped to `frame.view` until [`Painter::canvas_end`]. Then, in drawing order, each row
    /// source through [`Painter::canvas_rows`] with its labels drawn right after it, then
    /// [`Painter::canvas_marks`]. False for a painter that shows no canvas (a test probe, the
    /// dock's shifted painter, a GPU painter without rows): the caller skips the canvas.
    ///
    /// The GPU painter leaves the rows to the window's canvas pass, drawn at their place among
    /// its batch's instances; the CPU painter rasterizes them in place
    /// ([`crate::canvas::draw_rows`]). Both keep the rows across frames ([`crate::gpu`]).
    fn canvas_begin(&mut self, frame: &Frame) -> bool {
        let _ = frame;
        false
    }

    /// Draws row source `src` of the canvas begun by [`Painter::canvas_begin`]: one of the
    /// canvas tables (below [`crate::gpu::MARKS`]) or the gesture overlay
    /// ([`crate::gpu::OVERLAY`]), `table` packed into the rows this painter keeps after
    /// `change`, how it changed since this painter last drew `src`.
    ///
    /// # Panics
    ///
    /// Panics outside a canvas, or on `src` = [`crate::gpu::MARKS`]
    /// ([`Painter::canvas_marks`]).
    fn canvas_rows(
        &mut self,
        src: usize,
        table: &CanvasPrims,
        palette: &Palette,
        change: Change<'_>,
    ) {
        let _ = (src, table, palette, change);
        unreachable!("canvas_rows without a canvas_begin that returned true");
    }

    /// Draws the selection marks over `scene`'s rows (repacked when `changed`: the marks or
    /// `scene` since the last call).
    ///
    /// # Panics
    ///
    /// Panics outside a canvas.
    fn canvas_marks(&mut self, scene: &CanvasPrims, marks: &Marks, changed: bool) {
        let _ = (scene, marks, changed);
        unreachable!("canvas_marks without a canvas_begin that returned true");
    }

    /// Ends the canvas begun by [`Painter::canvas_begin`] (its clip).
    fn canvas_end(&mut self) {
        unreachable!("canvas_end without a canvas_begin that returned true");
    }

    /// Draws UI layout row `row` of UI source `src` (0: the main layer, 1: the overlay) with
    /// `paint`. A painter that keeps rows across frames ([`crate::ui_rows`]) calls `paint` only
    /// when the row's key differs from the one it kept, and draws what it kept otherwise. The
    /// default draws a visible row and skips a hidden one.
    fn ui_row(&mut self, src: usize, row: &UiRow, mut paint: impl FnMut(&mut Self))
    where
        Self: Sized,
    {
        let _ = src;
        if row.visible {
            paint(self);
        }
    }

    /// Ends UI source `src`'s rows for this frame: a painter that keeps rows drops those this
    /// frame did not draw.
    fn ui_end(&mut self, src: usize) {
        let _ = src;
    }
}

/// Polylines as flat `f32` point runs: chain `i` is `pts[end[i-1]..end[i]]`, at least 2 points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chains {
    /// Points in logical pixels.
    pub pts: Vec<[f32; 2]>,
    /// One past each chain's last point.
    pub end: Vec<u32>,
}

/// Converts packed `0xRRGGBBAA` (the `ui_styles` encoding) to a color.
pub fn rgba(v: u32) -> Color {
    let [r, g, b, a] = v.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

impl Chains {
    /// Appends one chain.
    ///
    /// # Panics
    ///
    /// Panics on fewer than 2 points.
    pub fn push(&mut self, pts: &[[f32; 2]]) {
        assert!(pts.len() >= 2, "a chain needs 2 points");
        self.pts.extend_from_slice(pts);
        self.end.push(self.pts.len() as u32);
        debug_assert_eq!(self.end.last().copied(), Some(self.pts.len() as u32));
    }

    /// Empties both columns, keeping capacity.
    pub fn clear(&mut self) {
        self.pts.clear();
        self.end.clear();
    }

    /// True when no chain is stored.
    pub fn is_empty(&self) -> bool {
        self.end.is_empty()
    }

    /// The chains as a kurbo path (allocates; the slow route for backends without a fast path).
    pub fn to_path(&self, closed: bool) -> BezPath {
        let mut path = BezPath::new();
        let pt = |q: [f32; 2]| (f64::from(q[0]), f64::from(q[1]));
        let mut start = 0usize;
        for &end in &self.end {
            let end = end as usize;
            assert!(end >= start + 2, "a chain of 2 points or more");
            path.move_to(pt(self.pts[start]));
            for &q in &self.pts[start + 1..end] {
                path.line_to(pt(q));
            }
            if closed {
                path.close_path();
            }
            start = end;
        }
        debug_assert_eq!(start, self.pts.len(), "every point in a chain");
        path
    }
}

/// Appends `r` as one closed subpath (corners clockwise on screen from the top-left).
pub(crate) fn push_rect(path: &mut BezPath, r: Rect) {
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
}

/// Strokes the outline of `r`, `w` px wide, building it in `path` (cleared first).
pub(crate) fn outline<P: Painter>(p: &mut P, path: &mut BezPath, r: Rect, w: f64, color: Color) {
    path.truncate(0);
    push_rect(path, r);
    p.stroke_path(path, w, color);
}

/// The [`ImageData`] of a prose image, cached in `cache` so its blob (and the GPU's upload of
/// it) is reused across frames. Holding the blob keeps the pixels alive, so a cached pointer is
/// never reused by another image.
// ponytail: dropped wholesale past twice the image cap; an LRU when pages show more images.
pub(crate) fn image_data(cache: &mut Vec<ImageData>, image: &schemify_ui::Image) -> ImageData {
    let key = image.rgba.as_ptr();
    if let Some(d) = cache.iter().find(|d| d.data.data().as_ptr() == key) {
        return d.clone();
    }
    if cache.len() >= 2 * schemify_ui::TextSystem::IMAGES_MAX {
        cache.clear();
    }
    let d = ImageData {
        data: peniko::Blob::new(image.rgba.clone()),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::AlphaPremultiplied,
        width: image.width,
        height: image.height,
    };
    cache.push(d.clone());
    debug_assert_eq!(
        d.data.data().as_ptr(),
        key,
        "the blob shares the image's pixels"
    );
    d
}
