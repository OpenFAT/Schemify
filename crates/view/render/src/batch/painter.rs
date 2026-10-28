//! The GPU painter: [`Painter`] calls in, instances of the frame's [`Batch`] out. Rects and
//! images become one instance each; glyphs ([`super::glyph`]) and paths ([`super::path`])
//! become atlas masks or stroke segments; a UI layout row is kept across frames in the batch's
//! [`crate::ui_rows::UiRows`]; the canvas leaves its stages among the instances and its rows
//! in [`InstanceRows`].
//!
//! Contents:
//! - `pub struct GpuPainter`
//! - `impl Painter for GpuPainter`
//! - `GpuPainter::mask`, `GpuPainter::clipped_out` (`pub(super)`)

use kurbo::{Affine, BezPath, Point, Rect, Stroke};
use peniko::{Color, FontData, ImageData};
use vello_cpu::Glyph;

use schemify_canvas::{CanvasPrims, Marks, Palette};

use super::{Batch, IMAGE, MASK, RECT, STRIDE, Stage, What, color_word};
use crate::atlas::Placed;
use crate::gpu::{Change, Frame, InstanceRows, MARKS};
use crate::painter::{Chains, Painter};
use crate::ui_rows;
use crate::widgets::UiRow;

/// Paints into a [`Batch`] (the GPU path): the window's painter. Built per frame by its
/// owner, which calls [`Batch::begin_frame`] before painting.
#[allow(missing_debug_implementations)]
pub struct GpuPainter<'a> {
    /// The frame's batch.
    pub batch: &'a mut Batch,
    /// Logical → device px (the window's scale factor).
    pub base: Affine,
    /// The canvas's instance rows ([`Painter::canvas_rows`] packs into them and leaves the
    /// frame to draw in [`InstanceRows::frame`]); `None` draws no canvas (a benchmark of the UI
    /// alone).
    pub rows: Option<&'a mut InstanceRows>,
}

impl Painter for GpuPainter<'_> {
    fn fill_rect(&mut self, r: Rect, color: Color) {
        let c = color_word(color);
        let d = self.base.transform_rect_bbox(r);
        if c >> 24 == 0 || self.clipped_out([d.x0, d.y0, d.x1, d.y1]) {
            return;
        }
        let pos = [d.x0, d.y0, d.x1, d.y1].map(|v| v as f32);
        self.batch.push(RECT, pos, [0.0; 4], c, What::Ui);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        self.fill(path, color);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        self.stroke(path, &Stroke::new(width), color);
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        self.stroke(path, stroke, color);
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
        self.draw_glyphs(font, px, coords, color_word(color), at, glyphs);
    }

    fn push_clip(&mut self, r: Rect) {
        let d = self.base.transform_rect_bbox(r);
        let c = self.batch.clip();
        let d = [d.x0, d.y0, d.x1, d.y1].map(|v| v as f32);
        self.batch.clips.push([
            d[0].max(c[0]),
            d[1].max(c[1]),
            d[2].min(c[2]),
            d[3].min(c[3]),
        ]);
    }

    fn pop_clip(&mut self) {
        assert!(self.batch.clips.pop().is_some(), "a clip to pop");
    }

    fn stroke_chains(&mut self, chains: &Chains, closed: bool, width: f64, color: Color) {
        let c = color_word(color);
        if c >> 24 == 0 {
            return;
        }
        assert!(width > 0.0, "zero-width stroke");
        let hw = (width * self.device_scale() * 0.5) as f32;
        let base = self.base;
        let dev = |q: [f32; 2]| {
            let p = base * Point::new(f64::from(q[0]), f64::from(q[1]));
            [p.x as f32, p.y as f32]
        };
        let mut start = 0usize;
        for &end in &chains.end {
            let end = end as usize;
            let pts = &chains.pts[start..end];
            for k in 0..pts.len() - usize::from(!closed) {
                let (a, b) = (dev(pts[k]), dev(pts[(k + 1) % pts.len()]));
                self.segment([a[0], a[1], b[0], b[1]], hw, c);
            }
            start = end;
        }
        debug_assert_eq!(start, chains.pts.len(), "every point in a chain");
    }

    fn fill_chains(&mut self, chains: &Chains, color: Color) {
        if !chains.is_empty() {
            self.fill(&chains.to_path(true), color);
        }
    }

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        let d = self.base.transform_rect_bbox(r);
        if self.clipped_out([d.x0, d.y0, d.x1, d.y1]) {
            return;
        }
        let k = self.batch.image(image);
        let pos = [d.x0, d.y0, d.x1, d.y1].map(|v| v as f32);
        self.batch
            .push(IMAGE, pos, [0.0, 0.0, 1.0, 1.0], u32::MAX, What::Image(k));
    }

    fn canvas_begin(&mut self, frame: &Frame) -> bool {
        let Some(rows) = self.rows.as_deref_mut() else {
            return false; // a batch that draws no canvas (a benchmark of the UI alone)
        };
        assert!(rows.frame.is_none(), "one canvas per frame");
        rows.frame = Some(*frame);
        // The canvas pass's stages go among the batch's instances in call order: each source's
        // labels over its rows and under the next source's, as the CPU painter draws them.
        self.batch.stage(Stage::Background);
        self.push_clip(frame.view);
        true
    }

    fn canvas_rows(
        &mut self,
        src: usize,
        table: &CanvasPrims,
        palette: &Palette,
        change: Change<'_>,
    ) {
        let rows = self.rows.as_deref_mut().expect("inside a canvas");
        assert!(rows.frame.is_some(), "inside a canvas");
        rows.pack(src, table, palette, change);
        self.batch.stage(Stage::Rows(src));
    }

    fn canvas_marks(&mut self, scene: &CanvasPrims, marks: &Marks, changed: bool) {
        let rows = self.rows.as_deref_mut().expect("inside a canvas");
        assert!(rows.frame.is_some(), "inside a canvas");
        rows.pack_marks(scene, marks, changed);
        self.batch.stage(Stage::Rows(MARKS));
    }

    fn canvas_end(&mut self) {
        self.pop_clip();
    }

    /// Keeps the row's slots when its key is unchanged; else paints it into them (or, from
    /// here on, lays its source out again). A row in a scroll pane is kept in the pane's
    /// content coordinates (whole device px; the fraction is in its key), unclipped, and drawn
    /// through its pane's offset and clip in the uniform, so it is kept while it scrolls, in or
    /// out of view. A row that drew a stand-in or a mask the atlas could not place is painted
    /// again next frame; one that draws an image is drawn from the frame's instances (its
    /// texture is the frame's).
    fn ui_row(&mut self, src: usize, row: &UiRow, mut paint: impl FnMut(&mut Self)) {
        assert!(src < ui_rows::SOURCES, "a UI source");
        let dev = self.device_scale();
        self.batch.ui.rescale(dev);
        let pane = if row.pane.is_none() {
            None
        } else {
            self.batch.ui.pane(row.pane)
        };
        if !row.visible && pane.is_none() {
            return; // scrolled out of view, kept in screen coordinates: nothing to draw
        }
        let mut key = row.key;
        let mut shift = 0.0;
        if let Some(k) = pane {
            let off = f64::from(key.scroll) * dev;
            shift = off.round();
            let c = self.base.transform_rect_bbox(row.clip);
            let entry = [0.0, shift, 0.0, 0.0, c.x0, c.y0, c.x1, c.y1].map(|v| v as f32);
            self.batch.ui.panes[k] = entry;
            key.rect[1] += key.scroll;
            key.scroll = (off - shift) as f32;
            key.clip = [0.0; 4];
            key.pane = k as u8;
        }
        let Batch { ui, atlas, .. } = &mut *self.batch;
        if let Some(range) = ui.keep(src, &key, atlas) {
            self.batch.retained(src, range);
            return;
        }
        // Paint the row into the capture, in its pane's coordinates.
        let base = self.base;
        let clips = pane.map(|_| std::mem::take(&mut self.batch.clips));
        self.base = Affine::translate((0.0, shift)) * base;
        self.batch.capture = Some(pane.unwrap_or(0) as u32);
        let unsettled = std::mem::take(&mut self.batch.unsettled);
        let overflows = self.batch.atlas.overflows;
        paint(self);
        self.batch.capture = None;
        self.base = base;
        if let Some(c) = clips {
            self.batch.clips = c;
        }
        key.stale = self.batch.unsettled || self.batch.atlas.overflows != overflows;
        self.batch.unsettled |= unsettled;
        // Keep it, or draw it from the frame's instances when it holds an image.
        let mut bytes = std::mem::take(&mut self.batch.captured);
        if std::mem::take(&mut self.batch.captured_image) {
            paint(self); // from the frame's instances, in screen coordinates
        } else {
            let Batch { ui, atlas, .. } = &mut *self.batch;
            match ui.put(src, key, &bytes, atlas) {
                Some(range) => self.batch.retained(src, range),
                None => self.batch.dropped += (bytes.len() / STRIDE) as u64,
            }
        }
        bytes.clear();
        self.batch.captured = bytes;
    }

    fn ui_end(&mut self, src: usize) {
        let Batch { ui, atlas, .. } = &mut *self.batch;
        ui.end(src, atlas);
    }
}

impl GpuPainter<'_> {
    /// Draws placed mask `p` with its cell at device `(x, y)` in `color`; an empty mask draws
    /// nothing.
    pub(super) fn mask(&mut self, p: Placed, x: i32, y: i32, color: u32) {
        let s = p.slot;
        if s.w == 0 {
            return;
        }
        let (x, y) = (
            (x + i32::from(p.off[0])) as f32,
            (y + i32::from(p.off[1])) as f32,
        );
        let pos = [x, y, x + f32::from(s.w), y + f32::from(s.h)];
        let aux = [s.x, s.y, s.x + s.w, s.y + s.h].map(f32::from);
        self.batch.push(MASK, pos, aux, color, What::Ui);
    }

    /// Whether device box `b` misses the current clip (an empty box always does).
    pub(super) fn clipped_out(&self, b: [f64; 4]) -> bool {
        let c = self.batch.clip().map(f64::from);
        b[2] <= c[0] || b[0] >= c[2] || b[3] <= c[1] || b[1] >= c[3] || b[2] <= b[0] || b[3] <= b[1]
    }
}
