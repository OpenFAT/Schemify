//! One drawing interface over two backends: vello's GPU [`Scene`] and the `vello_cpu` rasterizer
//! (golden images, GPU-less fallback, D5). Monomorphized, so it costs nothing at runtime.
//!
//! Both backends also keep *raster layers* across frames ([`Painter::layer`],
//! [`Painter::draw_layer`]): a picture recorded once and composed as an image afterwards, so a
//! canvas that only moved costs one image draw instead of re-encoding every primitive
//! (`canvas::draw_cached`, D39).

use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, PathEl, Rect, Shape, Stroke};
use vello::low_level::vello_encoding::Transform;
use vello::peniko::{Color, Fill, FontData, ImageBrush, ImageData, ImageQuality, ImageSampler};
use vello::{Glyph, Scene};

use crate::layers::{LAYER_SLOTS, Layers};

/// The drawing calls every frame builder uses. Coordinates are logical pixels; each painter
/// applies its own base transform (the window's scale factor).
pub trait Painter {
    /// Fills an axis-aligned rectangle.
    fn fill_rect(&mut self, r: Rect, color: Color);
    /// Fills a path (non-zero winding).
    fn fill_path(&mut self, path: &BezPath, color: Color);
    /// Strokes a path with butt caps and miter joins; `width` in logical pixels.
    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color);
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

    /// Records `draw` into raster layer `slot` (a `w × h` device-pixel picture whose origin is
    /// its top-left; `draw` paints in logical pixels as usual) and tags it `stamp`. The layer is
    /// rasterized before the frame is composed and stays until `slot` is recorded again.
    /// Returns false without calling `draw` when this painter keeps no layers; the caller then
    /// paints directly.
    ///
    /// # Panics
    ///
    /// Panics on `slot ≥ LAYER_SLOTS`, an empty size, or `stamp == 0` (the "no layer" tag).
    fn layer(
        &mut self,
        slot: usize,
        stamp: u64,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool
    where
        Self: Sized,
    {
        let _ = (slot, stamp, w, h, draw);
        false
    }

    /// Redraws the part `[x, x + w) × [y, y + h)` (device px) of layer `slot` with `draw`,
    /// which paints that part as [`Painter::layer`]'s `draw` paints a whole layer (its origin
    /// is the part's top-left); the rest of the layer keeps its pixels. Returns false without
    /// calling `draw` unless this painter holds layer `slot` tagged `stamp`.
    ///
    /// # Panics
    ///
    /// Panics on an empty part or one reaching outside the layer.
    #[allow(clippy::too_many_arguments)]
    fn patch_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool
    where
        Self: Sized,
    {
        let _ = (slot, stamp, x, y, w, h, draw);
        false
    }

    /// Whether this painter holds layer `slot` tagged `stamp` (recorded by it and not
    /// replaced since).
    fn holds_layer(&self, slot: usize, stamp: u64) -> bool {
        let _ = (slot, stamp);
        false
    }

    /// Draws layer `slot` through `at` (layer logical px → logical px), clipped to `clip`.
    /// `stretch: None` copies pixels (`at` must cover `clip`); `Some(bg)` samples bilinearly (a
    /// scaled stand-in) and paints the part of `clip` the layer misses with `bg`. Returns
    /// false, drawing nothing, unless this painter holds layer `slot` tagged `stamp` (a fresh
    /// painter, or one that never recorded it, holds none).
    fn draw_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        at: Affine,
        clip: Rect,
        stretch: Option<Color>,
    ) -> bool {
        let _ = (slot, stamp, at, clip, stretch);
        false
    }

    /// Draws `image` (premultiplied RGBA8, as prose images are) scaled into `r`. A backend
    /// without images fills `r` with a faint grey instead.
    fn image(&mut self, image: &ImageData, r: Rect) {
        debug_assert!(image.width > 0 && image.height > 0);
        self.fill_rect(r, Color::from_rgba8(128, 128, 128, 48));
    }
}

fn sampler(stretch: Option<Color>) -> ImageSampler {
    ImageSampler::default().with_quality(match stretch {
        Some(_) => ImageQuality::Medium,
        None => ImageQuality::Low,
    })
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
        data: vello::peniko::Blob::new(image.rgba.clone()),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::AlphaPremultiplied,
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

/// Converts packed `0xRRGGBBAA` (the `ui_styles` encoding) to a color.
pub fn rgba(v: u32) -> Color {
    let [r, g, b, a] = v.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

/// Paints into a vello [`Scene`] (GPU path).
#[allow(missing_debug_implementations)]
pub struct ScenePainter<'a> {
    /// Target scene; the caller resets it per frame.
    pub scene: &'a mut Scene,
    /// Logical → physical pixels (the window scale factor).
    pub base: Affine,
    /// Raster layers kept across frames; `None` paints every layer directly. The owner calls
    /// [`Layers::begin_frame`] before painting and [`Layers::raster`] after, before rendering
    /// `scene` (or composing over the last frame when [`Layers::reusable`]).
    pub layers: Option<&'a mut Layers>,
}

/// Bit patterns of a rectangle, for the frame log.
fn rect_bits(r: Rect) -> [u64; 4] {
    [r.x0, r.y0, r.x1, r.y1].map(f64::to_bits)
}

/// Bit patterns of a color, for the frame log.
fn color_bits(c: Color) -> [u32; 4] {
    c.components.map(f32::to_bits)
}

/// A path's elements as bit patterns, for the frame log (hashed without allocating).
struct PathBits<'a>(&'a BezPath);

impl std::hash::Hash for PathBits<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        let mut pt = |p: vello::kurbo::Point| {
            h.write_u64(p.x.to_bits());
            h.write_u64(p.y.to_bits());
        };
        for el in self.0.elements() {
            match *el {
                PathEl::MoveTo(p) => pt(p),
                PathEl::LineTo(p) => pt(p),
                PathEl::QuadTo(a, b) => [a, b].into_iter().for_each(&mut pt),
                PathEl::CurveTo(a, b, c) => [a, b, c].into_iter().for_each(&mut pt),
                PathEl::ClosePath => {}
            }
        }
        // Element kinds separately, so `MoveTo`/`LineTo` sequences of equal points differ.
        for el in self.0.elements() {
            h.write_u8(match el {
                PathEl::MoveTo(_) => 0,
                PathEl::LineTo(_) => 1,
                PathEl::QuadTo(..) => 2,
                PathEl::CurveTo(..) => 3,
                PathEl::ClosePath => 4,
            });
        }
    }
}

/// Glyph ids and positions as bit patterns, for the frame log.
struct GlyphBits<'a>(&'a [Glyph]);

impl std::hash::Hash for GlyphBits<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        h.write_usize(self.0.len());
        for g in self.0 {
            h.write_u32(g.id);
            h.write_u32(g.x.to_bits());
            h.write_u32(g.y.to_bits());
        }
    }
}

/// Chain points and ends as bit patterns, for the frame log.
struct ChainBits<'a>(&'a Chains);

impl std::hash::Hash for ChainBits<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.0.end.hash(h);
        for p in &self.0.pts {
            h.write_u32(p[0].to_bits());
            h.write_u32(p[1].to_bits());
        }
    }
}

impl ScenePainter<'_> {
    /// Logs one call outside the layers into the frame hash (see [`Layers::reusable`]).
    fn log(&mut self, data: impl std::hash::Hash) {
        if let Some(l) = self.layers.as_deref_mut() {
            l.log.call(data);
        }
    }
}

impl Painter for ScenePainter<'_> {
    fn fill_rect(&mut self, r: Rect, color: Color) {
        self.log((1u8, rect_bits(r), color_bits(color)));
        self.scene.fill(Fill::NonZero, self.base, color, None, &r);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        self.log((2u8, PathBits(path), color_bits(color)));
        self.scene.fill(Fill::NonZero, self.base, color, None, path);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        self.log((3u8, PathBits(path), width.to_bits(), color_bits(color)));
        self.scene
            .stroke(&Stroke::new(width), self.base, color, None, path);
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
        let at_bits = at.as_coeffs().map(f64::to_bits);
        self.log((
            4u8,
            font.data.id(),
            font.index,
            px.to_bits(),
            coords,
            color_bits(color),
        ));
        self.log((at_bits, GlyphBits(glyphs)));
        self.scene
            .draw_glyphs(font)
            .font_size(px)
            .normalized_coords(coords)
            .transform(self.base * at)
            .brush(color)
            .draw(Fill::NonZero, glyphs.iter().copied());
    }

    fn push_clip(&mut self, r: Rect) {
        self.log((5u8, rect_bits(r)));
        self.scene.push_clip_layer(Fill::NonZero, self.base, &r);
    }

    fn pop_clip(&mut self) {
        // Ends a clip without painting: not something that could cover a layer.
        if let Some(l) = self.layers.as_deref_mut() {
            l.log.mark(6u8);
        }
        self.scene.pop_layer();
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        // The blob's id names the pixels (it is cached per image, see `image_data`).
        self.log((9u8, image.data.id(), rect_bits(r)));
        self.scene.draw_image(image, self.base * image_to(image, r));
    }

    fn stroke_chains(&mut self, chains: &Chains, closed: bool, width: f64, color: Color) {
        assert!(width > 0.0, "zero-width stroke");
        self.log((
            7u8,
            ChainBits(chains),
            closed,
            width.to_bits(),
            color_bits(color),
        ));
        self.encode_chains(chains, closed, Some(width), color);
    }

    fn fill_chains(&mut self, chains: &Chains, color: Color) {
        self.log((8u8, ChainBits(chains), color_bits(color)));
        self.encode_chains(chains, true, None, color);
    }

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn holds_layer(&self, slot: usize, stamp: u64) -> bool {
        self.layers.as_deref().is_some_and(|l| {
            l.slots
                .get(slot)
                .is_some_and(|x| stamp != 0 && x.stamp == stamp && x.image.is_some())
        })
    }

    fn layer(
        &mut self,
        slot: usize,
        stamp: u64,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool {
        assert!(
            slot < LAYER_SLOTS && w > 0 && h > 0 && stamp != 0,
            "bad layer"
        );
        let Some(layers) = self.layers.as_deref_mut() else {
            return false;
        };
        // Record into the layer's own scene by swapping it in for the frame scene; the calls
        // belong to the layer (its stamp names them), not to the frame log.
        let mut other = std::mem::take(&mut layers.sized(slot, w, h).scene);
        other.reset();
        layers.log.recording = true;
        std::mem::swap(self.scene, &mut other);
        draw(self);
        std::mem::swap(self.scene, &mut other);
        let layers = self.layers.as_deref_mut().expect("checked above");
        layers.log.recording = false;
        let l = &mut layers.slots[slot];
        l.scene = other;
        l.stamp = stamp;
        l.dirty = true;
        l.patches.clear(); // the fresh picture covers them
        true
    }

    fn patch_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool {
        let Some(layers) = self.layers.as_deref_mut() else {
            return false;
        };
        let Some(l) = layers
            .slots
            .get(slot)
            .filter(|l| stamp != 0 && l.stamp == stamp)
        else {
            return false;
        };
        let image = l.image.as_ref().expect("a recorded layer has an image");
        assert!(
            w > 0 && h > 0 && x + w <= image.width && y + h <= image.height,
            "patch outside its layer"
        );
        // Like `layer`: record into a scene of its own, outside the frame log.
        let mut other = Scene::new();
        layers.log.recording = true;
        std::mem::swap(self.scene, &mut other);
        draw(self);
        std::mem::swap(self.scene, &mut other);
        let layers = self.layers.as_deref_mut().expect("checked above");
        layers.log.recording = false;
        layers.slots[slot].patches.push((other, [x, y, w, h]));
        true
    }

    fn draw_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        at: Affine,
        clip: Rect,
        stretch: Option<Color>,
    ) -> bool {
        let base = self.base;
        let s = self.device_scale();
        let Some(layers) = self.layers.as_deref_mut() else {
            return false;
        };
        let Some(image) = layers
            .slots
            .get(slot)
            .filter(|l| stamp != 0 && l.stamp == stamp)
            .and_then(|l| l.image.clone())
        else {
            return false;
        };
        // Neither where it lands nor how many tiles there are is part of the frame hash: a
        // frame that only moved its layers can be composed without vello (`Layers::reusable`);
        // the caller's clip around them names the covered area.
        let to_device = base * at * Affine::scale(1.0 / s);
        let draw = (slot, to_device, base.transform_rect_bbox(clip), stretch);
        layers.log.draws.push(draw);
        if let Some(bg) = stretch {
            self.scene.fill(Fill::NonZero, base, bg, None, &clip);
        }
        let brush = ImageBrush {
            image,
            sampler: sampler(stretch),
        };
        self.scene.push_clip_layer(Fill::NonZero, base, &clip);
        self.scene.draw_image(&brush, to_device);
        self.scene.pop_layer();
        true
    }
}

impl ScenePainter<'_> {
    /// Encodes straight into vello's path stream: no `PathEl` per segment. This is what
    /// `Scene::stroke`/`Scene::fill` do internally (transform, style, path, brush), minus the
    /// intermediate path.
    fn encode_chains(&mut self, chains: &Chains, closed: bool, stroke: Option<f64>, color: Color) {
        let enc = self.scene.encoding_mut();
        enc.encode_transform(Transform::from_kurbo(&self.base));
        match stroke {
            Some(w) => {
                let encoded = enc.encode_stroke_style(&Stroke::new(w));
                debug_assert!(encoded, "non-zero width always encodes");
            }
            None => enc.encode_fill_style(Fill::NonZero),
        }
        let mut pe = enc.encode_path(stroke.is_none());
        let mut start = 0usize;
        for &end in &chains.end {
            let end = end as usize;
            let [x, y] = chains.pts[start];
            pe.move_to(x, y);
            for &[x, y] in &chains.pts[start + 1..end] {
                pe.line_to(x, y);
            }
            if closed {
                pe.close();
            }
            start = end;
        }
        if pe.finish(true) != 0 {
            enc.encode_brush(color, 1.0);
        }
    }
}

/// Paints with `vello_cpu` into a pixmap: golden images and the no-GPU fallback. Raster layers
/// are rasterized when recorded and kept across [`CpuPainter::reset`].
pub struct CpuPainter {
    ctx: vello_cpu::RenderContext,
    resources: vello_cpu::Resources,
    base: Affine,
    /// `(stamp, pixels)` per slot; stamp 0 = empty.
    layers: Vec<(u64, Arc<vello_cpu::Pixmap>)>,
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
            layers: Vec::new(),
        }
    }

    /// Starts a new frame on the same target, keeping the raster layers.
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
        let _ = run.fill_glyphs(glyphs.iter().map(|g| vello_cpu::Glyph {
            id: g.id,
            x: g.x,
            y: g.y,
        }));
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

    fn holds_layer(&self, slot: usize, stamp: u64) -> bool {
        self.layers
            .get(slot)
            .is_some_and(|l| stamp != 0 && l.0 == stamp)
    }

    fn layer(
        &mut self,
        slot: usize,
        stamp: u64,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool {
        assert!(
            slot < LAYER_SLOTS && w > 0 && h > 0 && stamp != 0,
            "bad layer"
        );
        let (w, h) = (
            u16::try_from(w).expect("layer width fits u16"),
            u16::try_from(h).expect("layer height fits u16"),
        );
        // Record into a context of the layer's size by swapping it in for the frame's.
        let mut other = vello_cpu::RenderContext::new(w, h);
        std::mem::swap(&mut self.ctx, &mut other);
        draw(self);
        std::mem::swap(&mut self.ctx, &mut other);
        let mut pixmap = vello_cpu::Pixmap::new(w, h);
        other.flush();
        other.render(&mut pixmap, &mut self.resources);
        if self.layers.len() <= slot {
            self.layers
                .resize_with(slot + 1, || (0, Arc::new(vello_cpu::Pixmap::new(1, 1))));
        }
        self.layers[slot] = (stamp, Arc::new(pixmap));
        true
    }

    fn patch_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        draw: impl FnOnce(&mut Self),
    ) -> bool {
        if self
            .layers
            .get(slot)
            .is_none_or(|l| stamp == 0 || l.0 != stamp)
        {
            return false;
        }
        let lw = u32::from(self.layers[slot].1.width());
        let lh = u32::from(self.layers[slot].1.height());
        assert!(
            w > 0 && h > 0 && x + w <= lw && y + h <= lh,
            "patch outside its layer"
        );
        let (pw, ph) = (w as u16, h as u16);
        let mut other = vello_cpu::RenderContext::new(pw, ph);
        std::mem::swap(&mut self.ctx, &mut other);
        draw(self);
        std::mem::swap(&mut self.ctx, &mut other);
        let mut part = vello_cpu::Pixmap::new(pw, ph);
        other.flush();
        other.render(&mut part, &mut self.resources);
        // Copy-on-write: a frame still sampling the old picture keeps it.
        let dst = Arc::make_mut(&mut self.layers[slot].1).data_mut();
        let (lw, w, x, y) = (lw as usize, w as usize, x as usize, y as usize);
        for (r, src) in part.data().chunks_exact(w).enumerate() {
            let at = (y + r) * lw + x;
            dst[at..at + w].copy_from_slice(src);
        }
        true
    }

    fn draw_layer(
        &mut self,
        slot: usize,
        stamp: u64,
        at: Affine,
        clip: Rect,
        stretch: Option<Color>,
    ) -> bool {
        let Some((_, pixmap)) = self.layers.get(slot).filter(|l| stamp != 0 && l.0 == stamp) else {
            return false;
        };
        let s = self.device_scale();
        let to_logical = at * Affine::scale(1.0 / s);
        let size = Rect::new(
            0.0,
            0.0,
            f64::from(pixmap.width()),
            f64::from(pixmap.height()),
        );
        let image = vello_cpu::Image {
            image: vello_cpu::ImageSource::Pixmap(pixmap.clone()),
            sampler: sampler(stretch),
        };
        self.ctx.set_transform(self.base);
        if let Some(bg) = stretch {
            self.ctx.set_paint(bg);
            self.ctx.fill_rect(&clip);
        }
        // Only where the image lies: outside it the sampler would smear its edge pixels.
        let area = clip.intersect(to_logical.transform_rect_bbox(size));
        self.ctx.set_paint_transform(to_logical);
        self.ctx.set_paint(image);
        self.ctx.fill_rect(&area);
        self.ctx.reset_paint_transform();
        true
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
