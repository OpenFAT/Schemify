//! One drawing interface over two backends: the GPU painter's frame batch
//! ([`crate::batch::GpuPainter`]) and the `vello_cpu` rasterizer (golden images, GPU-less
//! fallback, D5). Monomorphized, so it costs nothing at runtime.
//!
//! Both draw the canvas from the instance rows [`crate::gpu`] packs ([`Painter::canvas_rows`]): the
//! GPU painter leaves them to the window's instanced pass, the CPU painter rasterizes them.

use kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use peniko::{Color, Fill, FontData, ImageData, ImageSampler};
use vello_cpu::Glyph;

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};

use crate::canvas::{self, Paths};
use crate::gpu::{Change, Frame, InstanceRows, MARKS};
use crate::ui_rows;
use crate::widgets::{RowKey, UiRow};

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
    /// canvas tables (below [`MARKS`]) or the gesture overlay ([`crate::gpu::OVERLAY`]), `table` packed
    /// into the rows this painter keeps after `change`, how it changed since this painter last
    /// drew `src`.
    ///
    /// # Panics
    ///
    /// Panics outside a canvas, or on `src` = [`MARKS`] ([`Painter::canvas_marks`]).
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

/// One painter call of a UI row, as the CPU painter keeps it: replayed while the row's key
/// holds (the GPU painter keeps the row's instances instead; an instance is not the CPU's
/// pixels, so the goldens replay the calls).
#[derive(Clone, Debug)]
enum Call {
    Rect(Rect, Color),
    Fill(BezPath, Color),
    Stroke(BezPath, Stroke, Color),
    Glyphs {
        font: FontData,
        px: f32,
        coords: Vec<i16>,
        color: Color,
        at: Affine,
        glyphs: Vec<Glyph>,
    },
    Clip(Rect),
    Pop,
    Image(ImageData, Rect),
}

/// One UI source's rows on the CPU: last frame's keys and calls, this frame's so far, and the
/// next of last frame's rows to match.
#[derive(Debug, Default)]
struct CpuRows {
    keys: Vec<RowKey>,
    calls: Vec<Vec<Call>>,
    new_keys: Vec<RowKey>,
    new_calls: Vec<Vec<Call>>,
    cursor: usize,
}

/// Paints with `vello_cpu` into a pixmap: golden images and the no-GPU fallback. The canvas's
/// instance rows are kept across [`CpuPainter::reset`] and patched as the GPU's are; so are the
/// UI's rows, as recorded calls matched by the same keys ([`Painter::ui_row`]).
pub struct CpuPainter {
    ctx: vello_cpu::RenderContext,
    resources: vello_cpu::Resources,
    base: Affine,
    /// The canvas's instance rows (kept across frames, patched as the GPU's are) and the
    /// paths they are batched into.
    rows: InstanceRows,
    paths: Paths,
    /// The canvas being drawn (between `canvas_begin` and `canvas_end`): its view and camera.
    canvas: Option<(Rect, Camera)>,
    /// The UI's rows per source, and the calls of the row being recorded.
    ui: [CpuRows; ui_rows::SOURCES],
    rec: Option<Vec<Call>>,
    /// Rows recorded so far (the rest were replayed): tests count them.
    pub recorded: u64,
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
            canvas: None,
            paths: Paths::default(),
            ui: Default::default(),
            rec: None,
            recorded: 0,
        }
    }

    /// Replays one recorded call (the same `vello_cpu` calls the direct path makes).
    fn exec(&mut self, c: &Call) {
        match c {
            Call::Rect(r, color) => self.draw_rect(*r, *color),
            Call::Fill(path, color) => self.draw_fill(path, *color),
            Call::Stroke(path, stroke, color) => self.draw_stroke(path, stroke.clone(), *color),
            Call::Glyphs {
                font,
                px,
                coords,
                color,
                at,
                glyphs,
            } => self.draw_glyphs(font, *px, coords, *color, *at, glyphs),
            Call::Clip(r) => self.clip(*r),
            Call::Pop => self.ctx.pop_layer(),
            Call::Image(image, r) => self.draw_image(image, *r),
        }
    }

    fn draw_rect(&mut self, r: Rect, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.fill_rect(&r);
    }

    fn draw_fill(&mut self, path: &BezPath, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx.fill_path(path);
    }

    fn draw_stroke(&mut self, path: &BezPath, stroke: Stroke, color: Color) {
        self.ctx.set_transform(self.base);
        self.ctx.set_paint(color);
        self.ctx.set_stroke(stroke);
        self.ctx.stroke_path(path);
    }

    fn draw_glyphs(
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

    fn clip(&mut self, r: Rect) {
        self.ctx.set_transform(self.base);
        self.ctx.push_clip_layer(&r.to_path(0.1));
    }

    fn draw_image(&mut self, image: &ImageData, r: Rect) {
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
        match &mut self.rec {
            Some(rec) => rec.push(Call::Rect(r, color)),
            None => self.draw_rect(r, color),
        }
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Fill(path.clone(), color)),
            None => self.draw_fill(path, color),
        }
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        self.stroke_styled(path, &Stroke::new(width), color);
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Stroke(path.clone(), stroke.clone(), color)),
            None => self.draw_stroke(path, stroke.clone(), color),
        }
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
        match &mut self.rec {
            Some(rec) => rec.push(Call::Glyphs {
                font: font.clone(),
                px,
                coords: coords.to_vec(),
                color,
                at,
                glyphs: glyphs.to_vec(),
            }),
            None => self.draw_glyphs(font, px, coords, color, at, glyphs),
        }
    }

    fn push_clip(&mut self, r: Rect) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Clip(r)),
            None => self.clip(r),
        }
    }

    fn pop_clip(&mut self) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Pop),
            None => self.ctx.pop_layer(),
        }
    }

    /// Replays the row's calls while its key is the one recorded (matched as the GPU painter
    /// matches its rows), else records them anew and replays them: the goldens draw through
    /// the kept rows, so a key that misses a paint input shows in them.
    fn ui_row(&mut self, src: usize, row: &UiRow, mut paint: impl FnMut(&mut Self)) {
        assert!(self.rec.is_none(), "rows do not nest");
        if !row.visible {
            return;
        }
        let s = &mut self.ui[src];
        let m = ui_rows::find(&s.keys, s.cursor, &row.key);
        if let Some(o) = m {
            s.cursor = o + 1;
        }
        let kept = m
            .filter(|&o| s.keys[o] == row.key)
            .map(|o| std::mem::take(&mut s.calls[o]));
        let calls = match kept {
            Some(c) => c,
            None => {
                self.rec = Some(Vec::new());
                paint(self);
                self.recorded += 1;
                self.rec.take().expect("recording")
            }
        };
        for c in &calls {
            self.exec(c);
        }
        let s = &mut self.ui[src];
        s.new_keys.push(row.key);
        s.new_calls.push(calls);
    }

    fn ui_end(&mut self, src: usize) {
        let s = &mut self.ui[src];
        std::mem::swap(&mut s.keys, &mut s.new_keys);
        std::mem::swap(&mut s.calls, &mut s.new_calls);
        s.new_keys.clear();
        s.new_calls.clear();
        s.cursor = 0;
        assert_eq!(s.keys.len(), s.calls.len());
    }

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn canvas_begin(&mut self, frame: &Frame) -> bool {
        assert!(self.canvas.is_none(), "one canvas at a time");
        let (view, cam) = (frame.view, frame.cam);
        self.push_clip(view);
        self.fill_rect(view, rgba(frame.background));
        let mut paths = std::mem::take(&mut self.paths);
        canvas::draw_grid(self, view, &cam, frame.grid, frame.grid_color, &mut paths);
        self.paths = paths;
        self.canvas = Some((view, cam));
        true
    }

    fn canvas_rows(
        &mut self,
        src: usize,
        table: &CanvasPrims,
        palette: &Palette,
        change: Change<'_>,
    ) {
        let (view, cam) = self.canvas.expect("inside a canvas");
        let (mut rows, mut paths) = (
            std::mem::take(&mut self.rows),
            std::mem::take(&mut self.paths),
        );
        rows.pack(src, table, palette, change);
        canvas::draw_rows(self, view, &cam, &rows, src, &mut paths);
        (self.rows, self.paths) = (rows, paths);
    }

    fn canvas_marks(&mut self, scene: &CanvasPrims, marks: &Marks, changed: bool) {
        let (view, cam) = self.canvas.expect("inside a canvas");
        let (mut rows, mut paths) = (
            std::mem::take(&mut self.rows),
            std::mem::take(&mut self.paths),
        );
        rows.pack_marks(scene, marks, changed);
        canvas::draw_rows(self, view, &cam, &rows, MARKS, &mut paths);
        (self.rows, self.paths) = (rows, paths);
    }

    fn canvas_end(&mut self) {
        assert!(self.canvas.take().is_some(), "inside a canvas");
        self.pop_clip();
        self.rows.clear_dirty(); // read in place: nothing to upload
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Image(image.clone(), r)),
            None => self.draw_image(image, r),
        }
    }
}
