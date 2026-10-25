//! The GPU painter (`docs/dev/research/render-backend.md` § 5, step 3): every [`Painter`] call
//! of a frame becomes instances of one batch ([`Batch`]) that `ui.wgsl` draws in a single
//! pass with the canvas pipelines ([`crate::gpu::Pipelines::draw`]): rects, atlas masks,
//! stroke segments, fill triangles and images, each with its clip. Glyphs and small paths are
//! coverage masks rasterized once by `vello_cpu` (the golden painter's rasterizer, so a mask
//! is the CPU painter's own pixels) into the [`Atlas`]; long strokes are segments drawn
//! analytically; the canvas is the retained instance rows, drawn at its stage in the batch.
//!
//! Six questions:
//! 1. In: one frame's painter calls. Out: instance bytes ([`STRIDE`] each), the draws (a
//!    range of instances with its texture, or a canvas stage), the atlas's pending masks, the
//!    frame's images.
//! 2. 10^3–10^4 instances a frame (the UI's rects and glyphs, a wave pane's segments);
//!    [`INSTANCES_MAX`] at most.
//! 3. Device px as `f32` (a frame is under 2^24 px across); colours RGBA8; a kind and a draw
//!    index are small integers.
//! 4. Appended in call order (the order is the picture), read once by the upload.
//! 5. Instances, draws and images live one frame; the atlas and the rasterizer persist.
//! 6. Serial (call order is drawing order).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use kurbo::{Affine, BezPath, Cap, Join, PathEl, Point, Rect, Shape, Stroke};
use peniko::{Color, FontData, ImageData};
use vello_cpu::Glyph;

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
use schemify_ui::TextSystem;
use schemify_ui::skrifa::{self, MetadataProvider};

use crate::atlas::{Atlas, Key, PATH, Placed};
use crate::canvas::{self, CanvasScratch};
use crate::gpu::{Change, Frame, InstanceRows, MARKS, OVERLAY};
use crate::painter::{Chains, Painter};

/// Bytes per instance: box (4 × f32), aux (4 × f32), clip (4 × f32), colour, kind.
pub const STRIDE: usize = 56;
/// Instance kinds (`ui.wgsl`): a filled rect.
pub const RECT: u32 = 0;
/// An atlas mask drawn one texel per pixel.
pub const MASK: u32 = 1;
/// A round-capped stroke segment.
pub const SEG: u32 = 2;
/// A plain fill triangle.
pub const TRI: u32 = 3;
/// An image (premultiplied RGBA), sampled.
pub const IMAGE: u32 = 4;
/// An atlas mask drawn scaled (a glyph too large to rasterize at its size), sampled.
pub const MASK_SCALED: u32 = 5;
/// Instances one frame holds; past it calls draw nothing (counted in [`Batch::dropped`]).
pub const INSTANCES_MAX: usize = 1 << 18;
/// Largest side of one mask, device px: a larger fill is split into tiles of this side.
const MASK_MAX: i32 = 512;
/// A stroke whose box is wider or taller than this is drawn as segments, not a mask (its mask
/// would be mostly empty texels).
const STROKE_MASK_MAX: f64 = 96.0;
/// Glyphs larger than this em (device px) are rasterized at it and drawn scaled.
const EM_MAX: f64 = 96.0;
/// Subpixel positions per pixel a glyph is rasterized at (horizontally; vertically too when
/// it is not hinted): 1/8 px. At 1/4 px (glifo's choice) dense UI text measured twice as far
/// from the CPU painter's continuous positions (mean |Δ| 0.72 against 0.34).
pub const PHASES: f64 = 8.0;
/// Glyph masks a frame rasterizes at most while a stand-in can take a glyph's place: a
/// cached mask of the same glyph in the same direction at another size, drawn scaled
/// ([`Batch::settled`] is false until the exact ones are in). A zoom with 200 labels in view
/// rasterized 100–180 masks a frame, 1.0–1.6 ms (`canvas_label_zoom_frames`).
const EXACT_PER_FRAME: u32 = 32;
/// Path keys quantize device coordinates to this many steps per pixel.
const PATH_STEPS: f64 = 64.0;
/// No clip: far outside any target.
const UNCLIPPED: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

/// What a draw of the batch draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// Its instances, sampling the atlas.
    Ui,
    /// Its instances, sampling image `k` of [`Batch::images`].
    Image(u32),
    /// A canvas stage, from the canvas pipelines and the instance rows.
    Canvas(Stage),
}

/// A stage of the canvas pass, in drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Background and grid.
    Background,
    /// The rows of one source (a canvas table, the marks, the gesture overlay).
    Rows(usize),
}

/// One draw: a range of instances and what draws them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draw {
    /// Instance indices (empty for a canvas stage).
    pub range: Range<u32>,
    /// The texture or canvas stage.
    pub what: What,
}

/// `vello_cpu` state reused for every mask: the context, its resources, the target.
struct Raster {
    ctx: vello_cpu::RenderContext,
    res: vello_cpu::Resources,
    pix: vello_cpu::Pixmap,
    coverage: Vec<u8>,
}

impl Default for Raster {
    fn default() -> Self {
        Raster {
            ctx: vello_cpu::RenderContext::new(1, 1),
            res: vello_cpu::Resources::new(),
            pix: vello_cpu::Pixmap::new(1, 1),
            coverage: Vec::new(),
        }
    }
}

impl Raster {
    /// Rasterizes `draw` (white, into a `w × h` target at the origin) and returns its
    /// coverage, row-major.
    fn run(
        &mut self,
        w: u16,
        h: u16,
        draw: impl FnOnce(&mut vello_cpu::RenderContext, &mut vello_cpu::Resources),
    ) -> &[u8] {
        assert!(w > 0 && h > 0, "a mask has pixels");
        self.ctx.reset_and_resize(w, h);
        self.ctx.set_paint(Color::WHITE);
        draw(&mut self.ctx, &mut self.res);
        self.ctx.flush();
        self.pix.resize(w, h);
        self.pix
            .data_mut()
            .fill(vello_cpu::color::PremulRgba8::from_u32(0));
        self.ctx.render(&mut self.pix, &mut self.res);
        self.coverage.clear();
        self.coverage.extend(self.pix.data().iter().map(|p| p.a));
        assert_eq!(self.coverage.len(), usize::from(w) * usize::from(h));
        &self.coverage
    }
}

/// One frame's instances and draws, the coverage atlas behind them, and the rasterizer that
/// fills it. Owner: the window (or a benchmark); the [`GpuPainter`] fills it, the GPU side
/// ([`crate::gpu::Pipelines`]) uploads and draws it.
#[derive(Default)]
pub struct Batch {
    bytes: Vec<u8>,
    draws: Vec<Draw>,
    clips: Vec<[f32; 4]>,
    images: Vec<ImageData>,
    /// The masks every frame's glyphs and paths are drawn from.
    pub atlas: Atlas,
    raster: Raster,
    /// Instances past [`INSTANCES_MAX`], not drawn.
    pub dropped: u64,
    /// Masks rasterized so far (the atlas's misses).
    pub rastered: u64,
    /// Per glyph and direction (`[font blob, face and glyph id, direction hash]`), the key,
    /// em (device px) and subpixel phase of its last exact mask: the stand-in for its other
    /// sizes.
    latest: HashMap<Key, (Key, f32, [u8; 2])>,
    /// Exact glyph masks this frame may still rasterize where a stand-in exists.
    exact_left: u32,
    /// A stand-in was drawn this frame: the next frame rasterizes more exact masks.
    unsettled: bool,
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Batch")
            .field("instances", &self.len())
            .field("draws", &self.draws.len())
            .field("atlas", &self.atlas.len())
            .finish()
    }
}

impl Batch {
    /// Starts a frame: no instance, draw, clip or image; the atlas's masks drawn from now on
    /// are this frame's.
    pub fn begin_frame(&mut self) {
        self.bytes.clear();
        self.draws.clear();
        self.clips.clear();
        self.images.clear();
        self.atlas.begin_frame();
        self.exact_left = EXACT_PER_FRAME;
        self.unsettled = false;
        // ponytail: the stand-in index starts over past the atlas's capacity in glyphs.
        if self.latest.len() > 1 << 16 {
            self.latest.clear();
        }
    }

    /// False when the frame drew a glyph from a stand-in (another size, scaled): its owner
    /// asks for another frame, with no input, to rasterize the exact masks (a few dozen a
    /// frame), so a zoom costs little per frame and the picture settles exact.
    pub fn settled(&self) -> bool {
        !self.unsettled
    }

    /// Instances this frame, [`STRIDE`] bytes each.
    pub fn instances(&self) -> &[u8] {
        &self.bytes
    }

    /// Instances this frame.
    pub fn len(&self) -> usize {
        self.bytes.len() / STRIDE
    }

    /// True when the frame drew nothing.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// The draws, in order.
    pub fn draws(&self) -> &[Draw] {
        &self.draws
    }

    /// The images [`What::Image`] names.
    pub fn images(&self) -> &[ImageData] {
        &self.images
    }

    /// The current clip, device px.
    fn clip(&self) -> [f32; 4] {
        self.clips.last().copied().unwrap_or(UNCLIPPED)
    }

    /// Appends one instance drawn by `what`, under the current clip.
    fn push(&mut self, kind: u32, pos: [f32; 4], aux: [f32; 4], color: u32, what: What) {
        let i = self.len() as u32;
        if self.len() >= INSTANCES_MAX {
            self.dropped += 1;
            return;
        }
        let clip = self.clip();
        for v in pos.iter().chain(&aux).chain(&clip) {
            self.bytes.extend_from_slice(&v.to_ne_bytes());
        }
        self.bytes.extend_from_slice(&color.to_ne_bytes());
        self.bytes.extend_from_slice(&kind.to_ne_bytes());
        match self.draws.last_mut() {
            Some(d) if d.what == what && d.range.end == i => d.range.end = i + 1,
            _ => self.draws.push(Draw {
                range: i..i + 1,
                what,
            }),
        }
        debug_assert_eq!(self.bytes.len(), self.len() * STRIDE);
    }

    /// Marks where canvas stage `stage` is drawn among the instances.
    fn stage(&mut self, stage: Stage) {
        let i = self.len() as u32;
        self.draws.push(Draw {
            range: i..i,
            what: What::Canvas(stage),
        });
    }

    /// The draw index of `image` this frame (added on first use).
    fn image(&mut self, image: &ImageData) -> u32 {
        let id = image.data.id();
        let k = match self.images.iter().position(|d| d.data.id() == id) {
            Some(k) => k,
            None => {
                self.images.push(image.clone());
                self.images.len() - 1
            }
        };
        k as u32
    }
}

/// RGBA8 bytes in memory order (R first), straight alpha, as `ui.wgsl` reads a colour.
fn color_word(c: Color) -> u32 {
    let c = c.to_rgba8();
    u32::from_ne_bytes([c.r, c.g, c.b, c.a])
}

/// Whether `t` is a positive uniform scale with no vertical skew: `vello_cpu` then hints the
/// glyphs it draws through it (vertically) and rounds their baseline to whole pixels.
fn hinted(t: &Affine) -> bool {
    const EPS: f64 = 1.0 / 4096.0; // glifo's SCALAR_NEARLY_ZERO
    let [a, b, _, d, _, _] = t.as_coeffs();
    (a - d).abs() <= EPS && a > 0.0 && d > 0.0 && b.abs() <= EPS
}

/// `v` as a whole pixel and the nearest of [`PHASES`] subpixel positions in it (the last
/// rounds up to the next pixel's first).
pub fn phase(v: f64) -> (i32, u8) {
    let i = v.floor();
    let q = ((v - i) * PHASES).round();
    let (i, q) = if q >= PHASES { (i + 1.0, 0.0) } else { (i, q) };
    assert!((0.0..PHASES).contains(&q), "a phase in range");
    (i as i32, q as u8)
}

/// The atlas key of glyph `id` of `font` (face `index`) drawn through the em-to-device
/// matrix `m` (the run's linear transform times its size), at subpixel phases `q`, with
/// variation `coords`: what decides its mask, and nothing else (not where it is drawn).
pub fn glyph_key(font: &FontData, id: u32, m: [f64; 4], q: [u8; 2], coords: &[i16]) -> Key {
    key_in_run(font, id, run_hash(m, coords), q)
}

/// The hash of what every glyph of a run shares in its key: the em-to-device matrix `m` (to
/// 1/256 px of em: two runs that round alike rasterize alike) and the variation `coords`.
fn run_hash(m: [f64; 4], coords: &[i16]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.map(|v| (v * 256.0).round() as i64).hash(&mut h);
    coords.hash(&mut h);
    h.finish()
}

/// [`glyph_key`] from its run's [`run_hash`]: the phases mixed in by an odd multiplier, so
/// each phase pair of a run is its own key, without hashing the run again per glyph.
fn key_in_run(font: &FontData, id: u32, run: u64, q: [u8; 2]) -> Key {
    let phase = u64::from(q[0]) | (u64::from(q[1]) << 8);
    [
        font.data.id(),
        (u64::from(font.index) << 32) | u64::from(id),
        run ^ (phase + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
    ]
}

/// Paints into a [`Batch`] (the GPU path): the window's painter.
#[allow(missing_debug_implementations)]
pub struct GpuPainter<'a> {
    /// The frame's batch; the owner calls [`Batch::begin_frame`] before painting.
    pub batch: &'a mut Batch,
    /// Logical → device px (the window's scale factor).
    pub base: Affine,
    /// The canvas's instance rows ([`Painter::canvas`] packs into them and leaves the frame
    /// to draw in [`InstanceRows::frame`]); `None` draws no canvas (a benchmark of the UI
    /// alone).
    pub rows: Option<&'a mut InstanceRows>,
}

impl GpuPainter<'_> {
    /// Draws placed mask `p` with its cell at device `(x, y)` in `color`.
    fn mask(&mut self, p: Placed, x: i32, y: i32, color: u32) {
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

    /// Whether device box `b` misses the current clip.
    fn clipped_out(&self, b: [f64; 4]) -> bool {
        let c = self.batch.clip().map(f64::from);
        b[2] <= c[0] || b[0] >= c[2] || b[3] <= c[1] || b[1] >= c[3] || b[2] <= b[0] || b[3] <= b[1]
    }

    /// One glyph of a run through `t` (run space → device px) at `px`, its em `em` device px,
    /// `hint` whether `vello_cpu` hints it, `run` the run's [`run_hash`].
    #[allow(clippy::too_many_arguments)]
    fn glyph(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        t: Affine,
        (em, hint, run, dir): (f64, bool, u64, u64),
        g: &Glyph,
        c: u32,
    ) {
        let o = t * Point::new(f64::from(g.x), f64::from(g.y));
        let (ix, qx) = phase(o.x);
        let (iy, qy) = if hint {
            (o.y.round() as i32, 0)
        } else {
            phase(o.y)
        };
        // Off the clip by more than a few ems: skip (cheaper than the lookup).
        let r = 4.0 * em;
        if self.clipped_out([o.x - r, o.y - r, o.x + r, o.y + r]) {
            return;
        }
        let key = key_in_run(font, g.id, run, [qx, qy]);
        let same = [key[0], key[1], dir];
        let placed = match self.batch.atlas.get(&key) {
            Some(p) => p,
            None => {
                if self.batch.exact_left == 0 && self.stand_in(&same, em, o, c) {
                    self.batch.unsettled = true;
                    return;
                }
                self.batch.exact_left = self.batch.exact_left.saturating_sub(1);
                // The glyph's origin at its phase inside a cell around its outline's box.
                let at = (f64::from(qx) / PHASES, f64::from(qy) / PHASES);
                let Some(cell) = glyph_cell(font, px, coords, t, g, o, at) else {
                    return;
                };
                let local = (at.0 - f64::from(cell[0]), at.1 - f64::from(cell[1]));
                let shift = Affine::translate((local.0 - o.x, local.1 - o.y)) * t;
                let (w, h) = ((cell[2] - cell[0]) as u16, (cell[3] - cell[1]) as u16);
                let off = [cell[0] as i16, cell[1] as i16];
                self.batch.rastered += 1;
                let batch = &mut *self.batch;
                let cov = if w == 0 || h == 0 {
                    &[][..]
                } else {
                    batch.raster.run(w, h, |ctx, res| {
                        ctx.set_transform(shift);
                        let run = ctx
                            .glyph_run(res, font)
                            .font_size(px)
                            .normalized_coords(coords);
                        let _ = run.fill_glyphs(std::iter::once(*g));
                    })
                };
                let (w, h) = if cov.is_empty() { (0, 0) } else { (w, h) };
                let p = batch.atlas.insert(key, w, h, off, cov);
                if p.is_some() {
                    batch.latest.insert(same, (key, em as f32, [qx, qy]));
                }
                match p {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        self.mask(placed, ix, iy, c);
    }

    /// Draws glyph `same` (its font, id and direction) of em `em` with its origin at device
    /// `o` from its last exact mask at another size, scaled about the origin; false when no
    /// such mask is held.
    fn stand_in(&mut self, same: &Key, em: f64, o: Point, c: u32) -> bool {
        let Some(&(key, em0, q)) = self.batch.latest.get(same) else {
            return false;
        };
        let Some(p) = self.batch.atlas.get(&key) else {
            return false;
        };
        let s = p.slot;
        if s.w == 0 {
            return true; // no outline at any size
        }
        // The old cell relative to its glyph's origin, scaled to this size.
        let k = em / f64::from(em0);
        let at = (f64::from(q[0]) / PHASES, f64::from(q[1]) / PHASES);
        let (x0, y0) = (f64::from(p.off[0]) - at.0, f64::from(p.off[1]) - at.1);
        let (x1, y1) = (x0 + f64::from(s.w), y0 + f64::from(s.h));
        let pos = [o.x + k * x0, o.y + k * y0, o.x + k * x1, o.y + k * y1].map(|v| v as f32);
        let aux = [s.x, s.y, s.x + s.w, s.y + s.h].map(f32::from);
        self.batch.push(MASK_SCALED, pos, aux, c, What::Ui);
        true
    }

    /// A glyph whose em is over [`EM_MAX`]: rasterized at `k` times its size (an em of
    /// `EM_MAX`, so every size past it shares one mask per direction) and drawn scaled up.
    #[allow(clippy::too_many_arguments)]
    fn glyph_scaled(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        t: Affine,
        g: &Glyph,
        c: u32,
        k: f64,
    ) {
        assert!(k > 0.0 && k < 1.0, "a glyph scaled down to rasterize");
        let o = t * Point::new(f64::from(g.x), f64::from(g.y));
        let [a, b, cc, d, _, _] = t.as_coeffs();
        // Through `ts`, the glyph's origin lands where it does through `t`, its outline `k`
        // times as large.
        let lin = Affine::new([a * k, b * k, cc * k, d * k, 0.0, 0.0]);
        let go = lin * Point::new(f64::from(g.x), f64::from(g.y));
        let ts = Affine::translate((o.x - go.x, o.y - go.y)) * lin;
        let m = [a, b, cc, d].map(|v| v * k * f64::from(px));
        let key = glyph_key(font, g.id, m, [PHASES as u8, 0], coords); // a phase no unscaled key has
        let placed = match self.batch.atlas.get(&key) {
            Some(p) => p,
            None => {
                let Some(cell) = glyph_cell(font, px, coords, ts, g, o, (0.0, 0.0)) else {
                    return;
                };
                let shift =
                    Affine::translate((-f64::from(cell[0]) - o.x, -f64::from(cell[1]) - o.y)) * ts;
                let (w, h) = ((cell[2] - cell[0]) as u16, (cell[3] - cell[1]) as u16);
                let off = [cell[0] as i16, cell[1] as i16];
                self.batch.rastered += 1;
                let batch = &mut *self.batch;
                let cov = if w == 0 || h == 0 {
                    &[][..]
                } else {
                    batch.raster.run(w, h, |ctx, res| {
                        ctx.set_transform(shift);
                        let run = ctx
                            .glyph_run(res, font)
                            .font_size(px)
                            .normalized_coords(coords);
                        let _ = run.fill_glyphs(std::iter::once(*g));
                    })
                };
                let (w, h) = if cov.is_empty() { (0, 0) } else { (w, h) };
                match batch.atlas.insert(key, w, h, off, cov) {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let s = placed.slot;
        if s.w == 0 {
            return;
        }
        // The cell, `1/k` times as large about the glyph's origin.
        let (x0, y0) = (f64::from(placed.off[0]) / k, f64::from(placed.off[1]) / k);
        let (x1, y1) = (x0 + f64::from(s.w) / k, y0 + f64::from(s.h) / k);
        let pos = [o.x + x0, o.y + y0, o.x + x1, o.y + y1].map(|v| v as f32);
        if self.clipped_out(pos.map(f64::from)) {
            return;
        }
        let aux = [s.x, s.y, s.x + s.w, s.y + s.h].map(f32::from);
        self.batch.push(MASK_SCALED, pos, aux, c, What::Ui);
    }

    /// Fills or strokes `path` (logical px) as atlas masks: one per [`MASK_MAX`] tile of its
    /// device box `dev`, each keyed by the shape relative to its whole-pixel corner, so the
    /// same shape anywhere on the pixel grid is one mask.
    fn path_masks(&mut self, path: &BezPath, stroke: Option<&Stroke>, color: u32, dev: Rect) {
        let (x0, y0) = (dev.x0.floor() as i32 - 1, dev.y0.floor() as i32 - 1);
        let (x1, y1) = (dev.x1.ceil() as i32 + 1, dev.y1.ceil() as i32 + 1);
        let shape = path_hash(path, self.base, (x0, y0), stroke);
        let base = self.base;
        for ty in (y0..y1).step_by(MASK_MAX as usize) {
            for tx in (x0..x1).step_by(MASK_MAX as usize) {
                let (w, h) = ((x1 - tx).min(MASK_MAX), (y1 - ty).min(MASK_MAX));
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
        for s in segs {
            self.segment(s, hw, color);
        }
    }

    /// One round-capped segment, device px, `hw` half width.
    fn segment(&mut self, s: [f32; 4], hw: f32, color: u32) {
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

    /// Strokes `path` in `stroke`: a mask when it is small, else segments.
    fn stroke(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        let c = color_word(color);
        if c >> 24 == 0 || stroke.width <= 0.0 {
            return;
        }
        let miter = if stroke.join == Join::Miter {
            stroke.miter_limit.max(1.0)
        } else {
            1.0
        };
        let caps = matches!(stroke.start_cap, Cap::Square) || matches!(stroke.end_cap, Cap::Square);
        let grow = stroke.width * 0.5 * miter * if caps { std::f64::consts::SQRT_2 } else { 1.0 };
        let dev = self
            .base
            .transform_rect_bbox(path.bounding_box().inflate(grow, grow));
        if self.clipped_out([dev.x0, dev.y0, dev.x1, dev.y1]) {
            return;
        }
        if dev.width() <= STROKE_MASK_MAX && dev.height() <= STROKE_MASK_MAX {
            self.path_masks(path, Some(stroke), c, dev);
        } else {
            self.path_segments(path, stroke, c);
        }
    }

    /// Fills `path` (non-zero): masks, tiled when large.
    fn fill(&mut self, path: &BezPath, color: Color) {
        let c = color_word(color);
        if c >> 24 == 0 || path.elements().is_empty() {
            return;
        }
        let dev = self.base.transform_rect_bbox(path.bounding_box());
        if self.clipped_out([dev.x0, dev.y0, dev.x1, dev.y1]) {
            return;
        }
        self.path_masks(path, None, c, dev);
    }
}

/// The whole-pixel cell (relative to the glyph's whole-pixel origin, which sits at `at`
/// inside it) around glyph `g`'s outline drawn through `t` with its origin at device `o`:
/// one pixel of antialiasing margin around, two vertically for hinting. `Some` of an empty
/// cell for a glyph with no outline; `None` for a face that does not parse.
fn glyph_cell(
    font: &FontData,
    px: f32,
    coords: &[i16],
    t: Affine,
    g: &Glyph,
    o: Point,
    at: (f64, f64),
) -> Option<[i32; 4]> {
    let face = skrifa::FontRef::from_index(font.data.data(), font.index).ok()?;
    let coords: Vec<skrifa::instance::NormalizedCoord> = coords
        .iter()
        .map(|&c| skrifa::instance::NormalizedCoord::from_bits(c))
        .collect();
    let size = skrifa::instance::Size::new(px);
    let metrics = face.glyph_metrics(size, skrifa::instance::LocationRef::new(&coords));
    let Some(b) = metrics.bounds(skrifa::GlyphId::new(g.id)) else {
        return Some([0, 0, 0, 0]);
    };
    // Outline corners in run space (font y up, run y down), through `t`, from the origin.
    let (gx, gy) = (f64::from(g.x), f64::from(g.y));
    let corners = [
        (b.x_min, b.y_min),
        (b.x_max, b.y_min),
        (b.x_min, b.y_max),
        (b.x_max, b.y_max),
    ]
    .map(|(x, y)| t * Point::new(gx + f64::from(x), gy - f64::from(y)) - o);
    let lo = corners
        .iter()
        .fold((f64::MAX, f64::MAX), |m, v| (m.0.min(v.x), m.1.min(v.y)));
    let hi = corners
        .iter()
        .fold((f64::MIN, f64::MIN), |m, v| (m.0.max(v.x), m.1.max(v.y)));
    let cell = [
        (at.0 + lo.0).floor() as i32 - 1,
        (at.1 + lo.1).floor() as i32 - 2,
        (at.0 + hi.0).ceil() as i32 + 1,
        (at.1 + hi.1).ceil() as i32 + 2,
    ];
    if cell[2] - cell[0] > MASK_MAX || cell[3] - cell[1] > MASK_MAX {
        return None; // ponytail: an outline over 5 ems of EM_MAX; none of the faces has one
    }
    assert!(
        cell[0] < cell[2] && cell[1] < cell[3],
        "a cell around the outline"
    );
    Some(cell)
}

/// A hash of `path` through `base` relative to device pixel `origin` (quantized to
/// [`PATH_STEPS`] per px) and of `stroke` (`None`: a non-zero fill): what decides a path
/// mask, whatever whole pixel it is drawn at.
fn path_hash(path: &BezPath, base: Affine, origin: (i32, i32), stroke: Option<&Stroke>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let o = (f64::from(origin.0), f64::from(origin.1));
    let pt = |h: &mut std::collections::hash_map::DefaultHasher, p: Point| {
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
                [a, b].into_iter().for_each(|p| pt(&mut h, p));
                2
            }
            PathEl::CurveTo(a, b, c) => {
                [a, b, c].into_iter().for_each(|p| pt(&mut h, p));
                3
            }
            PathEl::ClosePath => 4,
        };
        h.write_u8(tag);
    }
    base.as_coeffs()[..4]
        .iter()
        .for_each(|v| h.write_u64(v.to_bits()));
    match stroke {
        None => h.write_u8(0xf),
        Some(s) => {
            h.write_u64(s.width.to_bits());
            h.write_u64(s.miter_limit.to_bits());
            h.write_u8(s.join as u8);
            h.write_u8(s.start_cap as u8);
            h.write_u8(s.end_cap as u8);
            h.write_u64(s.dash_offset.to_bits());
            s.dash_pattern.iter().for_each(|d| h.write_u64(d.to_bits()));
        }
    }
    h.finish()
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
        let c = color_word(color);
        if c >> 24 == 0 || px <= 0.0 {
            return;
        }
        let t = self.base * at;
        let [a, b, cc, d, _, _] = t.as_coeffs();
        let em = (a * d - b * cc).abs().sqrt() * f64::from(px);
        if em <= 0.0 {
            return; // a degenerate transform draws nothing
        }
        if em > EM_MAX {
            for g in glyphs {
                self.glyph_scaled(font, px, coords, t, g, c, EM_MAX / em);
            }
            return;
        }
        let m = [a, b, cc, d].map(|v| v * f64::from(px));
        // The direction: the em matrix at unit em, which a zoom keeps.
        let dir = run_hash(m.map(|v| v / em), coords);
        let run = (em, hinted(&t), run_hash(m, coords), dir);
        for g in glyphs {
            self.glyph(font, px, coords, t, run, g, c);
        }
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

    fn canvas(
        &mut self,
        frame: &Frame,
        tables: [(&CanvasPrims, Change<'_>); 4],
        marks: (&CanvasPrims, &Marks, bool),
        palette: &Palette,
        mut texts: impl FnMut(&mut Self, usize),
    ) {
        let Some(rows) = self.rows.as_deref_mut() else {
            return; // a batch that draws no canvas (a benchmark of the UI alone)
        };
        assert!(rows.frame.is_none(), "one canvas per frame");
        for (src, (t, change)) in tables.into_iter().enumerate() {
            rows.pack(src, t, palette, change);
        }
        rows.pack_marks(marks.0, marks.1, marks.2);
        rows.frame = Some(*frame);
        // The canvas pass's stages among the batch's instances: each table's texts over its
        // rows and under the next table's, the marks over all, as the CPU painter draws them.
        self.batch.stage(Stage::Background);
        for src in 0..4 {
            self.batch.stage(Stage::Rows(src));
            self.push_clip(frame.view);
            texts(self, src);
            self.pop_clip();
        }
        self.batch.stage(Stage::Rows(MARKS));
    }

    fn overlay(
        &mut self,
        view: Rect,
        cam: &Camera,
        prims: &CanvasPrims,
        palette: &Palette,
        text: &mut TextSystem,
        scratch: &mut CanvasScratch,
    ) {
        let Some(rows) = self.rows.as_deref_mut() else {
            return;
        };
        assert!(
            rows.frame.is_some(),
            "the overlay goes over this frame's canvas"
        );
        rows.pack(OVERLAY, prims, palette, Change::All);
        self.batch.stage(Stage::Rows(OVERLAY));
        self.push_clip(view);
        canvas::draw_texts(self, view, cam, &prims.texts, None, palette, text, scratch);
        self.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phases are exact: a position rounds to the nearest eighth of a pixel, half a step up,
    /// and the last half step wraps to the next pixel's first; whole-pixel shifts keep the
    /// phase.
    #[test]
    fn subpixel_phases_are_exact() {
        assert_eq!(PHASES, 8.0);
        assert_eq!(phase(10.0), (10, 0));
        assert_eq!(phase(10.0624), (10, 0));
        assert_eq!(phase(10.0625), (10, 1), "half a step rounds up");
        assert_eq!(phase(10.1874), (10, 1));
        assert_eq!(phase(10.1875), (10, 2));
        assert_eq!(phase(10.93), (10, 7));
        assert_eq!(phase(10.9375), (11, 0), "the last half step wraps");
        assert_eq!(phase(-0.05), (0, 0));
        assert_eq!(phase(-0.1), (-1, 7));
        for k in [-3, 0, 7, 1000] {
            let v = 0.3;
            assert_eq!(phase(f64::from(k) + v).1, phase(v).1, "shift by {k}");
        }
    }

    /// Glyph keys: equal for what rasterizes alike (the same em matrix within 1/512 px, any
    /// position), different for another glyph, face, size, phase, direction or coords.
    #[test]
    fn glyph_keys_bucket_exactly() {
        let mut text = TextSystem::new();
        let mut layout = schemify_ui::parley::Layout::new();
        text.shape_into("a", 13.0, &mut layout);
        let font = layout
            .lines()
            .flat_map(|l| l.items())
            .find_map(|i| match i {
                schemify_ui::parley::PositionedLayoutItem::GlyphRun(r) => {
                    Some(r.run().font().clone())
                }
                _ => None,
            })
            .expect("a run");
        let m = [13.0, 0.0, 0.0, 13.0];
        let k = glyph_key(&font, 7, m, [1, 0], &[]);
        assert_eq!(
            k,
            glyph_key(&font, 7, [13.001, 0.0, 0.0, 13.0], [1, 0], &[])
        );
        assert_ne!(k, glyph_key(&font, 8, m, [1, 0], &[]), "glyph");
        assert_ne!(
            k,
            glyph_key(&font, 7, [13.25, 0.0, 0.0, 13.25], [1, 0], &[]),
            "size"
        );
        assert_ne!(k, glyph_key(&font, 7, m, [2, 0], &[]), "x phase");
        assert_ne!(k, glyph_key(&font, 7, m, [1, 1], &[]), "y phase");
        assert_ne!(
            k,
            glyph_key(&font, 7, [0.0, 13.0, -13.0, 0.0], [1, 0], &[]),
            "turned"
        );
        assert_ne!(k, glyph_key(&font, 7, m, [1, 0], &[100]), "coords");
        let mut other = font.clone();
        other.index += 1;
        assert_ne!(k, glyph_key(&other, 7, m, [1, 0], &[]), "face");
    }

    /// The same glyph drawn at two whole-pixel positions is one mask; at another phase it is a
    /// second; the masks land a pixel apart where the positions are.
    #[test]
    fn a_glyph_is_rasterized_once_per_phase() {
        let mut text = TextSystem::new();
        let mut layout = schemify_ui::parley::Layout::new();
        text.shape_into("e", 13.0, &mut layout);
        let (font, g) = layout
            .lines()
            .flat_map(|l| l.items())
            .find_map(|i| match i {
                schemify_ui::parley::PositionedLayoutItem::GlyphRun(r) => Some((
                    r.run().font().clone(),
                    r.positioned_glyphs().next().expect("a glyph"),
                )),
                _ => None,
            })
            .expect("a run");
        let glyph = Glyph {
            id: g.id,
            x: 0.0,
            y: 12.0,
        };
        let mut batch = Batch::default();
        batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut batch,
            base: Affine::IDENTITY,
            rows: None,
        };
        let at = |x: f64| Affine::translate((x, 0.0));
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(10.0), &[glyph]);
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(31.0), &[glyph]);
        assert_eq!(
            p.batch.rastered, 1,
            "one mask for both whole-pixel positions"
        );
        p.glyphs(&font, 13.0, &[], Color::WHITE, at(31.5), &[glyph]);
        assert_eq!(p.batch.rastered, 2, "another phase, another mask");
        assert_eq!(p.batch.len(), 3);
        let x0 = |i: usize| {
            let b = &p.batch.instances()[i * STRIDE..];
            f32::from_ne_bytes(b[..4].try_into().expect("4 bytes"))
        };
        assert_eq!(
            x0(1) - x0(0),
            21.0,
            "a whole-pixel shift moves the cell by it"
        );
        assert!(
            p.batch.atlas.pending_bytes() > 0,
            "the masks wait for the GPU"
        );
    }

    /// A zoom: once a frame's exact budget is spent, a glyph met at a new size is drawn from
    /// its mask at another size (scaled) and the batch is unsettled; frames with no input
    /// rasterize the rest and settle on exact masks, then stay settled rasterizing nothing
    /// (idle at zero).
    #[test]
    fn a_zoom_draws_stand_ins_then_settles() {
        let mut text = TextSystem::new();
        let mut layout = schemify_ui::parley::Layout::new();
        text.shape_into("x", 13.0, &mut layout);
        let (font, id) = layout
            .lines()
            .flat_map(|l| l.items())
            .find_map(|i| match i {
                schemify_ui::parley::PositionedLayoutItem::GlyphRun(r) => Some((
                    r.run().font().clone(),
                    r.positioned_glyphs().next().expect("a glyph").id,
                )),
                _ => None,
            })
            .expect("a run");
        let glyph = Glyph { id, x: 0.0, y: 0.0 };
        let mut batch = Batch::default();
        let n = EXACT_PER_FRAME as usize + 20;
        let frame = |batch: &mut Batch, sizes: &[f32]| {
            batch.begin_frame();
            let mut p = GpuPainter {
                batch,
                base: Affine::IDENTITY,
                rows: None,
            };
            for (k, &px) in sizes.iter().enumerate() {
                let at = Affine::translate((20.0 * k as f64, 30.0));
                p.glyphs(&font, px, &[], Color::WHITE, at, &[glyph]);
            }
            let kinds: Vec<u32> = (0..p.batch.len())
                .map(|i| {
                    let b = &p.batch.instances()[i * STRIDE + 52..][..4];
                    u32::from_ne_bytes(b.try_into().expect("4 bytes"))
                })
                .collect();
            (p.batch.settled(), kinds)
        };
        assert!(frame(&mut batch, &[10.0]).0, "nothing to stand in for");
        let sizes: Vec<f32> = (0..n).map(|k| 10.5 + 0.25 * k as f32).collect();
        let (settled, kinds) = frame(&mut batch, &sizes);
        assert!(!settled, "a zoom frame draws stand-ins");
        assert_eq!(kinds.len(), n, "every glyph drawn");
        let stand_ins = kinds.iter().filter(|&&k| k == MASK_SCALED).count();
        assert_eq!(stand_ins, n - EXACT_PER_FRAME as usize, "past the budget");
        let mut frames = 0;
        let kinds = loop {
            frames += 1;
            let (settled, kinds) = frame(&mut batch, &sizes);
            if settled {
                break kinds;
            }
            assert!(frames < 8, "settles");
        };
        assert!(kinds.iter().all(|&k| k == MASK), "settled on exact masks");
        let r = batch.rastered;
        assert!(frame(&mut batch, &sizes).0);
        assert_eq!(batch.rastered, r, "a settled frame rasterizes nothing");
    }
}
