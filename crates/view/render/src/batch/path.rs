//! Paths on the GPU: a fill or stroke painter call in, atlas mask or segment instances out. A
//! fill, or a stroke whose device box is small, is rasterized by `vello_cpu` into atlas masks
//! (one per [`MASK_MAX`] tile, keyed by the shape relative to its whole-pixel corner, so the
//! same shape anywhere on the pixel grid is one mask); a larger stroke is flattened into
//! round-capped segments the shader draws analytically.
//!
//! Contents:
//! - `GpuPainter::stroke`, `GpuPainter::fill`, `GpuPainter::segment` (`pub(super)`)

use std::collections::hash_map::DefaultHasher;
use std::f64::consts;
use std::hash::Hasher;

use kurbo::{Affine, BezPath, Cap, Join, PathEl, Point, Rect, Shape, Stroke};
use peniko::Color;

use super::{GpuPainter, MASK_MAX, SEG, What};
use crate::Painter;
use crate::atlas::PATH;

/// A stroke whose box is wider or taller than this is drawn as segments, not a mask (its mask
/// would be mostly empty texels).
const STROKE_MASK_MAX: f64 = 96.0;
/// Path keys quantize device coordinates to this many steps per pixel.
const PATH_STEPS: f64 = 64.0;

impl GpuPainter<'_> {
    /// Strokes `path` (logical px) in `stroke`: a mask when it is small, else segments. A
    /// transparent colour or a non-positive width draws nothing.
    pub(super) fn stroke(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        let c = super::color_word(color);
        if c >> 24 == 0 || stroke.width <= 0.0 {
            return;
        }
        let miter = if stroke.join == Join::Miter {
            stroke.miter_limit.max(1.0)
        } else {
            1.0
        };
        debug_assert!(miter >= 1.0, "a join reaches at least half the width");
        let caps = matches!(stroke.start_cap, Cap::Square) || matches!(stroke.end_cap, Cap::Square);
        let grow = stroke.width * 0.5 * miter * if caps { consts::SQRT_2 } else { 1.0 };
        debug_assert!(grow > 0.0, "a stroke covers its path");
        let dev = self
            .base
            .transform_rect_bbox(path.bounding_box().inflate(grow, grow));
        if self.clipped_out([dev.x0, dev.y0, dev.x1, dev.y1]) {
            return;
        }
        debug_assert!(dev.width() >= 0.0, "a device box");
        if dev.width() <= STROKE_MASK_MAX && dev.height() <= STROKE_MASK_MAX {
            self.path_masks(path, Some(stroke), c, dev);
        } else {
            self.path_segments(path, stroke, c);
        }
    }

    /// Fills `path` (logical px, non-zero): masks, tiled when large. A transparent colour or
    /// an empty path draws nothing.
    pub(super) fn fill(&mut self, path: &BezPath, color: Color) {
        let c = super::color_word(color);
        if c >> 24 == 0 || path.elements().is_empty() {
            return;
        }
        let dev = self.base.transform_rect_bbox(path.bounding_box());
        if self.clipped_out([dev.x0, dev.y0, dev.x1, dev.y1]) {
            return;
        }
        debug_assert!(dev.width() > 0.0, "a kept fill has width");
        self.path_masks(path, None, c, dev);
    }

    /// One round-capped segment `s` (device px, `[x0, y0, x1, y1]`) of half width `hw`;
    /// skipped when it misses the clip.
    pub(super) fn segment(&mut self, s: [f32; 4], hw: f32, color: u32) {
        debug_assert!(hw >= 0.0, "a half width");
        debug_assert!(s.iter().all(|v| v.is_finite()), "a segment in device px");
        let pad = f64::from(hw) + 1.0;
        let b = [
            f64::from(s[0].min(s[2])) - pad,
            f64::from(s[1].min(s[3])) - pad,
            f64::from(s[0].max(s[2])) + pad,
            f64::from(s[1].max(s[3])) + pad,
        ];
        if self.clipped_out(b) {
            return;
        }
        self.batch
            .push(SEG, s, [hw, 0.0, 0.0, 0.0], color, What::Ui);
    }
}

// ---- private ----

impl GpuPainter<'_> {
    /// Fills or strokes `path` (logical px) as atlas masks: one per [`MASK_MAX`] tile of its
    /// device box `dev`.
    fn path_masks(&mut self, path: &BezPath, stroke: Option<&Stroke>, color: u32, dev: Rect) {
        let (x0, y0) = (dev.x0.floor() as i32 - 1, dev.y0.floor() as i32 - 1);
        let (x1, y1) = (dev.x1.ceil() as i32 + 1, dev.y1.ceil() as i32 + 1);
        assert!(x0 < x1, "a box of pixels");
        assert!(y0 < y1, "a box of pixels");
        let shape = path_hash(path, self.base, (x0, y0), stroke);
        let base = self.base;
        let rows = (y0..y1).step_by(MASK_MAX as usize);
        for ty in rows {
            let columns = (x0..x1).step_by(MASK_MAX as usize);
            for tx in columns {
                let (w, h) = ((x1 - tx).min(MASK_MAX), (y1 - ty).min(MASK_MAX));
                debug_assert!(w > 0, "a tile has pixels");
                debug_assert!(h > 0, "a tile has pixels");
                let tile = [tx, ty, tx + w, ty + h].map(f64::from);
                if self.clipped_out(tile) {
                    continue;
                }
                let key = [PATH, shape, (((tx - x0) as u64) << 32) | (ty - y0) as u64];
                let placed = match self.batch.atlas.get(&key) {
                    Some(p) => p,
                    None => {
                        self.batch.rastered += 1;
                        let batch = &mut *self.batch;
                        let cov = batch.raster.run(w as u16, h as u16, |ctx, _| {
                            let at = Affine::translate((-f64::from(tx), -f64::from(ty))) * base;
                            ctx.set_transform(at);
                            match stroke {
                                Some(s) => {
                                    ctx.set_stroke(s.clone());
                                    ctx.stroke_path(path);
                                }
                                None => {
                                    ctx.set_fill_rule(peniko::Fill::NonZero);
                                    ctx.fill_path(path);
                                }
                            }
                        });
                        let empty = cov.iter().all(|&v| v == 0);
                        let (w, h, cov) = if empty {
                            (0, 0, &[][..])
                        } else {
                            (w as u16, h as u16, cov)
                        };
                        match batch.atlas.insert(key, w, h, [0, 0], cov) {
                            Some(p) => p,
                            None => continue,
                        }
                    }
                };
                self.mask(placed, tx, ty, color);
            }
        }
    }

    /// Strokes `path` (logical px) as round-capped segments, its dashes applied: a stroke
    /// too large for a mask. Joins are the segments' round caps (`Stroke::new`'s round joins).
    fn path_segments(&mut self, path: &BezPath, stroke: &Stroke, color: u32) {
        assert!(stroke.width > 0.0, "a visible stroke");
        let hw = (stroke.width * self.device_scale() * 0.5) as f32;
        let base = self.base;
        let mut pts: Vec<Point> = Vec::new();
        let mut segs: Vec<[f32; 4]> = Vec::new();
        let mut flush = |pts: &mut Vec<Point>, closed: bool| {
            if closed && pts.len() > 2 {
                pts.push(pts[0]);
            }
            for w in pts.windows(2) {
                segs.push([w[0].x, w[0].y, w[1].x, w[1].y].map(|v| v as f32));
            }
            pts.clear();
        };
        let mut each = |el: PathEl| match el {
            PathEl::MoveTo(p) => {
                flush(&mut pts, false);
                pts.push(base * p);
            }
            PathEl::LineTo(p) => pts.push(base * p),
            PathEl::ClosePath => flush(&mut pts, true),
            _ => unreachable!("flattened"),
        };
        let tol = 0.1 / self.device_scale();
        if stroke.dash_pattern.is_empty() {
            kurbo::flatten(path, tol, &mut each);
        } else {
            let dashed = kurbo::dash(path.iter(), stroke.dash_offset, &stroke.dash_pattern);
            kurbo::flatten(dashed, tol, &mut each);
        }
        flush(&mut pts, false);
        debug_assert!(pts.is_empty(), "every point flushed into a segment");
        for s in segs {
            self.segment(s, hw, color);
        }
    }
}

/// A hash of `path` through `base` relative to device pixel `origin` (quantized to
/// [`PATH_STEPS`] per px) and of `stroke` (`None`: a non-zero fill): what decides a path
/// mask, whatever whole pixel it is drawn at.
fn path_hash(path: &BezPath, base: Affine, origin: (i32, i32), stroke: Option<&Stroke>) -> u64 {
    debug_assert!(base.is_finite(), "a drawable base transform");
    let mut h = DefaultHasher::new();
    let o = (f64::from(origin.0), f64::from(origin.1));
    let pt = |h: &mut DefaultHasher, p: Point| {
        let p = base * p;
        h.write_i64(((p.x - o.0) * PATH_STEPS).round() as i64);
        h.write_i64(((p.y - o.1) * PATH_STEPS).round() as i64);
    };
    for el in path.elements() {
        let tag = match *el {
            PathEl::MoveTo(p) => {
                pt(&mut h, p);
                0
            }
            PathEl::LineTo(p) => {
                pt(&mut h, p);
                1
            }
            PathEl::QuadTo(a, b) => {
                pt(&mut h, a);
                pt(&mut h, b);
                2
            }
            PathEl::CurveTo(a, b, c) => {
                pt(&mut h, a);
                pt(&mut h, b);
                pt(&mut h, c);
                3
            }
            PathEl::ClosePath => 4,
        };
        h.write_u8(tag);
    }
    for v in &base.as_coeffs()[..4] {
        h.write_u64(v.to_bits());
    }
    match stroke {
        None => h.write_u8(0xf),
        Some(s) => {
            h.write_u64(s.width.to_bits());
            h.write_u64(s.miter_limit.to_bits());
            h.write_u8(s.join as u8);
            h.write_u8(s.start_cap as u8);
            h.write_u8(s.end_cap as u8);
            h.write_u64(s.dash_offset.to_bits());
            for d in &s.dash_pattern {
                h.write_u64(d.to_bits());
            }
        }
    }
    h.finish()
}
