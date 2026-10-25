//! One drawing interface over two backends: the GPU painter's frame batch
//! ([`crate::batch::GpuPainter`]) and the `vello_cpu` rasterizer (golden images, GPU-less
//! fallback, D5). Monomorphized, so it costs nothing at runtime.
//!
//! Both draw the canvas from the instance rows [`crate::gpu`] packs ([`Painter::canvas`]): the
//! GPU painter leaves them to the window's instanced pass, the CPU painter rasterizes them.

use kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use peniko::{Color, Fill, FontData, ImageData, ImageSampler};
use vello_cpu::Glyph;

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
use schemify_ui::TextSystem;

use crate::canvas::{self, CanvasScratch, Paths};
use crate::gpu::{Change, Frame, InstanceRows, MARKS};

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
        debug_assert!(image.width > 0 && image.height > 0);
        self.fill_rect(r, Color::from_rgba8(128, 128, 128, 48));
    }

    /// Draws the canvas from the instance rows this painter keeps ([`crate::gpu`]):
    /// `frame`'s background and grid, `tables` in drawing order (each packed after how it
    /// changed since this painter last drew it), each table's texts through `texts(self, k)`
    /// right after its rows, then the `marks` over `marks.0` (repacked when `marks.2`), all
    /// clipped to `frame.view`. The GPU painter leaves the rows to the window's canvas pass,
    /// drawn at their place among its batch's instances; the CPU painter rasterizes them in
    /// place ([`crate::canvas::draw_rows`]). The default draws nothing: a painter that never
    /// shows a canvas (a test probe, the dock's shifted painter).
    fn canvas(
        &mut self,
        frame: &Frame,
        tables: [(&CanvasPrims, Change<'_>); 4],
        marks: (&CanvasPrims, &Marks, bool),
        palette: &Palette,
        texts: impl FnMut(&mut Self, usize),
    ) where
        Self: Sized,
    {
        let _ = (frame, tables, marks, palette, texts);
    }

    /// Draws the gesture overlay `prims` (rows and labels, repacked whole) over this frame's
    /// canvas, clipped to `view`. The GPU painter packs it into its instance rows' overlay
    /// source, drawn by the canvas pass; the default draws it immediately
    /// ([`crate::canvas::draw_prims`]), as the CPU painter does.
    fn overlay(
        &mut self,
        view: Rect,
        cam: &Camera,
        prims: &CanvasPrims,
        palette: &Palette,
        text: &mut TextSystem,
        scratch: &mut CanvasScratch,
    ) where
        Self: Sized,
    {
        self.push_clip(view);
        canvas::draw_prims(self, view, cam, prims, palette, text, scratch);
        self.pop_clip();
    }
}

/// Maps image pixels onto `r`: the paint transform of [`Painter::image`].
fn image_to(image: &ImageData, r: Rect) -> Affine {
    let (w, h) = (f64::from(image.width), f64::from(image.height));
    assert!(w > 0.0 && h > 0.0, "an image has pixels");
    Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width() / w, r.height() / h)
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
    debug_assert_eq!(d.data.data().as_ptr(), key);
    d
}

/// Polylines as flat `f32` point runs: chain `i` is `pts[end[i-1]..end[i]]`, at least 2 points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chains {
    /// Points in logical pixels.
    pub pts: Vec<[f32; 2]>,
    /// One past each chain's last point.
    pub end: Vec<u32>,
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
            path.move_to(pt(self.pts[start]));
            for &q in &self.pts[start + 1..end] {
                path.line_to(pt(q));
            }
            if closed {
                path.close_path();
            }
            start = end;
        }
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

/// Converts packed `0xRRGGBBAA` (the `ui_styles` encoding) to a color.
pub fn rgba(v: u32) -> Color {
    let [r, g, b, a] = v.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

/// Paints with `vello_cpu` into a pixmap: golden images and the no-GPU fallback. The canvas's
/// instance rows are kept across [`CpuPainter::reset`] and patched as the GPU's are.
pub struct CpuPainter {
    ctx: vello_cpu::RenderContext,
    resources: vello_cpu::Resources,
    base: Affine,
    /// The canvas's instance rows (kept across frames, patched as the GPU's are) and the
    /// paths they are batched into.
    rows: InstanceRows,
    paths: Paths,
}

impl std::fmt::Debug for CpuPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPainter")
            .field("w", &self.ctx.width())
            .field("h", &self.ctx.height())
            .finish()
    }
}

impl CpuPainter {
    /// A `w × h` physical-pixel target with `scale` logical → physical pixels.
    ///
    /// # Panics
    ///
    /// Panics on a zero-sized target or a non-positive scale.
    pub fn new(w: u16, h: u16, scale: f64) -> Self {
        assert!(w > 0 && h > 0, "empty target");
        assert!(scale > 0.0 && scale.is_finite(), "bad scale");
        CpuPainter {
            ctx: vello_cpu::RenderContext::new(w, h),
            resources: vello_cpu::Resources::new(),
            base: Affine::scale(scale),
            rows: InstanceRows::default(),
            paths: Paths::default(),
        }
    }

    /// Starts a new frame on the same target, keeping the canvas rows.
    pub fn reset(&mut self) {
        self.ctx.reset();
    }

    /// Rasterizes everything painted so far onto `background` and returns the pixels.
    pub fn finish(&mut self, background: Color) -> vello_cpu::Pixmap {
        let (w, h) = (self.ctx.width(), self.ctx.height());
        let mut pixmap = vello_cpu::Pixmap::new(w, h);
        let bg = background.premultiply().to_rgba8();
        pixmap.data_mut().fill(bg);
        self.ctx.flush();
        self.ctx.render(&mut pixmap, &mut self.resources);
        pixmap
    }
}

impl Painter for CpuPainter {
    fn fill_rect(&mut self, r: Rect, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.fill_rect(&r);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx.fill_path(path);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.set_stroke(Stroke::new(width));
        self.ctx.stroke_path(path);
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.set_stroke(stroke.clone());
        self.ctx.stroke_path(path);
    }

    fn glyphs(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        color: Color,
        at: Affine,
        glyphs: &[Glyph],
    ) {
        self.ctx.set_transform(self.base * at);
        self.ctx.set_paint(color);
        let run = self
            .ctx
            .glyph_run(&mut self.resources, font)
            .font_size(px)
            .normalized_coords(coords);
        // A glyph the face cannot outline is skipped by vello_cpu; the rest of the run still draws.
        let _ = run.fill_glyphs(glyphs.iter().copied());
    }

    fn push_clip(&mut self, r: Rect) {
        self.ctx.set_transform(self.base);
        self.ctx.push_clip_layer(&r.to_path(0.1));
    }

    fn pop_clip(&mut self) {
        self.ctx.pop_layer();
    }

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn canvas(
        &mut self,
        frame: &Frame,
        tables: [(&CanvasPrims, Change<'_>); 4],
        marks: (&CanvasPrims, &Marks, bool),
        palette: &Palette,
        mut texts: impl FnMut(&mut Self, usize),
    ) {
        let mut rows = std::mem::take(&mut self.rows);
        let mut paths = std::mem::take(&mut self.paths);
        let (view, cam) = (frame.view, &frame.cam);
        self.push_clip(view);
        self.fill_rect(view, rgba(frame.background));
        canvas::draw_grid(self, view, cam, frame.grid, frame.grid_color, &mut paths);
        for (src, (t, change)) in tables.into_iter().enumerate() {
            rows.pack(src, t, palette, change);
            canvas::draw_rows(self, view, cam, &rows, src, &mut paths);
            texts(self, src);
        }
        rows.pack_marks(marks.0, marks.1, marks.2);
        canvas::draw_rows(self, view, cam, &rows, MARKS, &mut paths);
        self.pop_clip();
        rows.clear_dirty(); // read in place: nothing to upload
        (self.rows, self.paths) = (rows, paths);
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        // ponytail: converts (copies) the pixels on every draw; the CPU path draws goldens
        // and the GPU-less fallback only. Cache the pixmap beside the blob if that changes.
        let brush = vello_cpu::Image {
            image: vello_cpu::ImageSource::from_peniko_image_data(image),
            sampler: ImageSampler::default(),
        };
        self.ctx.set_transform(self.base);
        self.ctx.set_paint_transform(image_to(image, r));
        self.ctx.set_paint(brush);
        self.ctx.fill_rect(&r);
        self.ctx.reset_paint_transform();
    }
}
