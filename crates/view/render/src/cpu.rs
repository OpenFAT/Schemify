//! The CPU painter (D5): painter calls in, a `vello_cpu` pixmap out. The goldens' oracle, PNG
//! export, and the web's last tier (`docs/spec/web.md` §7). The canvas's instance rows are kept
//! across [`CpuPainter::reset`] and patched as the GPU's are ([`crate::gpu`]), then rasterized by
//! [`crate::canvas::draw_rows`]; the UI's rows are kept as recorded calls matched by the same
//! keys the GPU painter matches ([`crate::ui_rows::find`]), so a key that misses a paint input
//! shows in the goldens.
//!
//! Contents:
//! - `pub struct CpuPainter`, `CpuPainter::new`, `CpuPainter::reset`, `CpuPainter::finish`
//! - `impl Painter for CpuPainter`

use kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use peniko::{Color, Fill, FontData, ImageData, ImageSampler};
use vello_cpu::Glyph;

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};

use crate::canvas::{self, Paths};
use crate::gpu::{Change, Frame, InstanceRows, MARKS};
use crate::painter::{Painter, rgba};
use crate::ui_rows;
use crate::widgets::{RowKey, UiRow};

/// Paints with `vello_cpu` into a pixmap: golden images and the no-GPU fallback. Owns its
/// canvas rows and its UI rows; both survive [`CpuPainter::reset`].
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

/// One UI source's rows on the CPU: last frame's keys and calls (SoA, equal lengths), this
/// frame's so far, and the next of last frame's rows to match.
#[derive(Debug, Default)]
struct CpuRows {
    keys: Vec<RowKey>,
    calls: Vec<Vec<Call>>,
    new_keys: Vec<RowKey>,
    new_calls: Vec<Vec<Call>>,
    cursor: usize,
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
        assert!(w > 0, "empty target");
        assert!(h > 0, "empty target");
        assert!(scale > 0.0 && scale.is_finite(), "bad scale");
        CpuPainter {
            ctx: vello_cpu::RenderContext::new(w, h),
            resources: vello_cpu::Resources::new(),
            base: Affine::scale(scale),
            rows: InstanceRows::default(),
            paths: Paths::default(),
            canvas: None,
            ui: Default::default(),
            rec: None,
            recorded: 0,
        }
    }

    /// Starts a new frame on the same target, keeping the canvas and UI rows.
    pub fn reset(&mut self) {
        assert!(self.canvas.is_none(), "no canvas left open");
        self.ctx.reset();
    }

    /// Rasterizes everything painted so far onto `background` and returns the pixels.
    pub fn finish(&mut self, background: Color) -> vello_cpu::Pixmap {
        let (w, h) = (self.ctx.width(), self.ctx.height());
        let mut pixmap = vello_cpu::Pixmap::new(w, h);
        pixmap.data_mut().fill(background.premultiply().to_rgba8());
        self.ctx.flush();
        self.ctx.render(&mut pixmap, &mut self.resources);
        debug_assert_eq!((pixmap.width(), pixmap.height()), (w, h));
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

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        match &mut self.rec {
            Some(rec) => rec.push(Call::Image(image.clone(), r)),
            None => self.draw_image(image, r),
        }
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
        self.draw_source(src, |rows| rows.pack(src, table, palette, change));
    }

    fn canvas_marks(&mut self, scene: &CanvasPrims, marks: &Marks, changed: bool) {
        self.draw_source(MARKS, |rows| rows.pack_marks(scene, marks, changed));
    }

    fn canvas_end(&mut self) {
        assert!(self.canvas.take().is_some(), "inside a canvas");
        self.pop_clip();
        self.rows.clear_dirty(); // read in place: nothing to upload
    }

    /// Replays the row's calls while its key is the one recorded (matched as the GPU painter
    /// matches its rows), else records them anew and replays them: the goldens draw through
    /// the kept rows, so a key that misses a paint input shows in them.
    fn ui_row(&mut self, src: usize, row: &UiRow, mut paint: impl FnMut(&mut Self)) {
        assert!(self.rec.is_none(), "rows do not nest");
        assert!(src < ui_rows::SOURCES, "a UI source");
        if !row.visible {
            return;
        }
        let s = &mut self.ui[src];
        let matched = ui_rows::find(&s.keys, s.cursor, &row.key);
        if let Some(o) = matched {
            s.cursor = o + 1;
        }
        let kept = matched
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
        assert_eq!(s.keys.len(), s.calls.len(), "a call list per kept row");
    }
}

// ---- private ----

impl CpuPainter {
    /// Packs one row source with `pack` into the kept rows and rasterizes it in the open
    /// canvas.
    fn draw_source(&mut self, src: usize, pack: impl FnOnce(&mut InstanceRows)) {
        let (view, cam) = self.canvas.expect("inside a canvas");
        let mut rows = std::mem::take(&mut self.rows);
        let mut paths = std::mem::take(&mut self.paths);
        pack(&mut rows);
        canvas::draw_rows(self, view, &cam, &rows, src, &mut paths);
        (self.rows, self.paths) = (rows, paths);
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
}

/// Maps image pixels onto `r`: the paint transform of [`Painter::image`].
fn image_to(image: &ImageData, r: Rect) -> Affine {
    let (w, h) = (f64::from(image.width), f64::from(image.height));
    assert!(w > 0.0, "an image has pixels");
    assert!(h > 0.0, "an image has pixels");
    Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width() / w, r.height() / h)
}
