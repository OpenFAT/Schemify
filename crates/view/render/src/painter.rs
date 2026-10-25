//! One drawing interface over two backends: vello's GPU [`Scene`] and the `vello_cpu` rasterizer
//! (golden images, GPU-less fallback, D5). Monomorphized, so it costs nothing at runtime.
//!
//! Both draw the canvas from the instance rows [`crate::gpu`] packs ([`Painter::canvas`]): the
//! GPU painter leaves them to the window's instanced pass, the CPU painter rasterizes them.
//! The GPU painter also keeps *live layers* ([`Painter::live_layer`]): chrome recorded every
//! frame but rasterized only when it changed, so a frame that only moved the camera composes
//! without vello (D39).

use std::hash::{Hash, Hasher};

use vello::kurbo::{Affine, BezPath, PathEl, Rect, Shape, Stroke};
use vello::low_level::vello_encoding::Transform;
use vello::peniko::{
    BlendMode, Color, Compose, Fill, FontData, ImageBrush, ImageData, ImageQuality, ImageSampler,
    Mix,
};
use vello::{Glyph, Scene};

use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
use schemify_ui::TextSystem;

use crate::canvas::{self, CanvasScratch, Paths};
use crate::gpu::{Change, Frame, InstanceRows, MARKS};
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

    /// Paints `draw` (logical px, as usual) through raster layer `slot`, which follows its own
    /// picture: recorded every frame, rasterized again only when its calls changed, and drawn
    /// as an image over `r`, which `draw` must cover opaquely (a composed frame keeps the
    /// previous frame around it). Chrome that changes on every pointer move (the status bar)
    /// goes through this, so such a frame composes without vello (D39). A painter without
    /// layers paints `draw` directly.
    ///
    /// # Panics
    ///
    /// Panics on `slot ≥ LAYER_SLOTS`.
    fn live_layer(&mut self, slot: usize, r: Rect, draw: impl FnOnce(&mut Self))
    where
        Self: Sized,
    {
        assert!(slot < LAYER_SLOTS, "bad layer slot");
        let _ = r;
        draw(self);
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
    /// clipped to `frame.view`. The GPU painter leaves the rows to the window's canvas pass
    /// under this frame and cuts a hole in the frame over `frame.view` (so its texts land
    /// over the rows, the marks under them); the CPU painter rasterizes them in place
    /// ([`crate::canvas::draw_rows`]). The default draws nothing: a painter that never
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
    /// The canvas's instance rows ([`Painter::canvas`] packs into them and leaves the frame
    /// to draw in [`InstanceRows::frame`]); `None` draws no canvas (a benchmark of the UI
    /// alone).
    pub rows: Option<&'a mut InstanceRows>,
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

    /// Paints `f` into the frame scene and, when the call lands over this frame's layers, into
    /// [`Layers`]'s top part too (what a composed frame lays over the layer quads).
    fn paint(&mut self, f: impl Fn(&mut Scene)) {
        f(self.scene);
        let Some(l) = self.layers.as_deref_mut() else {
            return;
        };
        if l.log.recording || l.log.draws.is_empty() {
            return;
        }
        // Clipped by a clip opened before the layers: the top part alone cannot reproduce it.
        l.log.tangled |= l.log.depth != l.log.top_depth;
        f(&mut l.top);
        debug_assert!(l.log.top_depth <= l.log.depth);
    }
}

impl Painter for ScenePainter<'_> {
    fn fill_rect(&mut self, r: Rect, color: Color) {
        self.log((1u8, rect_bits(r), color_bits(color)));
        let base = self.base;
        self.paint(|s| s.fill(Fill::NonZero, base, color, None, &r));
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        self.log((2u8, PathBits(path), color_bits(color)));
        let base = self.base;
        self.paint(|s| s.fill(Fill::NonZero, base, color, None, path));
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        self.log((3u8, PathBits(path), width.to_bits(), color_bits(color)));
        let (base, stroke) = (self.base, Stroke::new(width));
        self.paint(|s| s.stroke(&stroke, base, color, None, path));
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        let dashes: Vec<u64> = stroke.dash_pattern.iter().map(|d| d.to_bits()).collect();
        let style = (
            stroke.width.to_bits(),
            stroke.join as u8,
            stroke.start_cap as u8,
            dashes,
        );
        self.log((10u8, PathBits(path), style, color_bits(color)));
        let base = self.base;
        self.paint(|s| s.stroke(stroke, base, color, None, path));
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
        let base = self.base;
        self.paint(|s| {
            s.draw_glyphs(font)
                .font_size(px)
                .normalized_coords(coords)
                .transform(base * at)
                .brush(color)
                .draw(Fill::NonZero, glyphs.iter().copied());
        });
    }

    fn push_clip(&mut self, r: Rect) {
        // A clip paints nothing: hashed, but not something painted over a layer (a live layer
        // drawn before it must stay composable).
        if let Some(l) = self.layers.as_deref_mut() {
            l.log.mark((5u8, rect_bits(r)));
        }
        self.scene.push_clip_layer(Fill::NonZero, self.base, &r);
        if let Some(l) = self.layers.as_deref_mut().filter(|l| !l.log.recording) {
            // A clip opened over the layers is the top part's too.
            l.log.depth += 1;
            if !l.log.draws.is_empty() {
                l.top.push_clip_layer(Fill::NonZero, self.base, &r);
                l.log.top_depth += 1;
            }
            assert!(l.log.top_depth <= l.log.depth);
        }
    }

    fn pop_clip(&mut self) {
        // Ends a clip without painting: not something that could cover a layer.
        if let Some(l) = self.layers.as_deref_mut().filter(|l| !l.log.recording) {
            l.log.mark(6u8);
            assert!(l.log.depth > 0, "a clip to pop");
            // Clips nest, and the top part's open after the others: the innermost open clip is
            // the top part's while any of its clips is open.
            if l.log.top_depth > 0 {
                l.top.pop_layer();
                l.log.top_depth -= 1;
            }
            l.log.depth -= 1;
        }
        self.scene.pop_layer();
    }

    fn image(&mut self, image: &ImageData, r: Rect) {
        // The blob's id names the pixels (it is cached per image, see `image_data`).
        self.log((9u8, image.data.id(), rect_bits(r)));
        let at = self.base * image_to(image, r);
        self.paint(|s| s.draw_image(image, at));
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
            return; // a scene that draws no canvas (a benchmark of the UI alone)
        };
        assert!(rows.frame.is_none(), "one canvas per frame");
        for (src, (t, change)) in tables.into_iter().enumerate() {
            rows.pack(src, t, palette, change);
        }
        rows.pack_marks(marks.0, marks.1, marks.2);
        rows.frame = Some(*frame);
        // The hole: the canvas pass runs under this frame, so what was painted inside the view
        // so far is cut out; what comes after (texts, the gesture overlay, popups) lands over
        // it. "Destination out" of an opaque rect cuts exactly the rect (a `Clear` layer would
        // clear whole 16 px tiles of vello's around it).
        let (base, view) = (self.base, frame.view);
        self.log((12u8, rect_bits(view)));
        self.paint(|s| {
            let cut = BlendMode::new(Mix::Normal, Compose::DestOut);
            s.push_layer(Fill::NonZero, cut, 1.0, base, &view);
            s.fill(Fill::NonZero, base, Color::BLACK, None, &view);
            s.pop_layer();
        });
        self.push_clip(view);
        for k in 0..4 {
            texts(self, k);
        }
        self.pop_clip();
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
        let base = self.base;
        self.paint(|s| encode_chains(s, base, chains, closed, Some(width), color));
    }

    fn fill_chains(&mut self, chains: &Chains, color: Color) {
        self.log((8u8, ChainBits(chains), color_bits(color)));
        let base = self.base;
        self.paint(|s| encode_chains(s, base, chains, true, None, color));
    }

    fn device_scale(&self) -> f64 {
        self.base.as_coeffs()[0]
    }

    fn live_layer(&mut self, slot: usize, r: Rect, draw: impl FnOnce(&mut Self)) {
        assert!(slot < LAYER_SLOTS, "bad layer slot");
        let Some(layers) = self.layers.as_deref_mut() else {
            return draw(self);
        };
        // The layer holds r's device pixels, whole ones; the recording puts r's top-left at
        // the layer's origin and hashes its calls, which name the picture.
        let dev = self.base.transform_rect_bbox(r);
        let (x0, y0) = (dev.x0.floor(), dev.y0.floor());
        let (w, h) = ((dev.x1.ceil() - x0) as u32, (dev.y1.ceil() - y0) as u32);
        if w == 0 || h == 0 {
            return;
        }
        let mut scene = std::mem::take(&mut layers.sized(slot, w, h).scene);
        scene.reset();
        let base = self.base;
        self.base = Affine::translate((-x0, -y0)) * base;
        let (scene, hash) = self.record(scene, draw);
        self.base = base;
        let layers = self.layers.as_deref_mut().expect("checked above");
        let stamp = layers.live(slot, scene, hash);
        let s = self.device_scale();
        let at = Affine::translate((x0 / s, y0 / s));
        let drawn = self.draw_layer(slot, stamp, at, r);
        assert!(drawn, "a live layer is drawn the frame it is recorded");
    }
}

impl ScenePainter<'_> {
    /// Draws live layer `slot`, tagged `stamp`, through `at` (layer logical px → logical px),
    /// clipped to `clip`, pixel for pixel (`at` covers `clip`). Returns false, drawing nothing,
    /// unless this painter holds the layer so tagged.
    fn draw_layer(&mut self, slot: usize, stamp: u64, at: Affine, clip: Rect) -> bool {
        let base = self.base;
        let s = self.device_scale();
        let Some(layers) = self.layers.as_deref_mut() else {
            return false;
        };
        let Some(image) = layers.held(slot, stamp).cloned() else {
            return false;
        };
        // Where it lands is not part of the frame hash: a frame that only moved its layers
        // can be composed without vello (`Layers::reusable`).
        let to_device = base * at * Affine::scale(1.0 / s);
        let draw = (slot, to_device, base.transform_rect_bbox(clip));
        if layers.log.draws.is_empty() {
            layers.log.mark(11u8); // where the layers start: what comes after is the top part
        }
        // A layer over the top part would be composed under it.
        layers.log.tangled |= layers.log.after;
        layers.log.draws.push(draw);
        let brush = ImageBrush {
            image,
            sampler: ImageSampler::default().with_quality(ImageQuality::Low),
        };
        self.scene.push_clip_layer(Fill::NonZero, base, &clip);
        self.scene.draw_image(&brush, to_device);
        self.scene.pop_layer();
        true
    }

    /// Paints `draw` into `scene` (swapped in for the frame scene) and returns it with the hash
    /// of its calls. The calls belong to a live layer, not to the frame log; they are hashed
    /// with the base transform, so the same picture at the same place hashes the same from
    /// frame to frame.
    fn record(&mut self, mut scene: Scene, draw: impl FnOnce(&mut Self)) -> (Scene, u64) {
        let layers = self
            .layers
            .as_deref_mut()
            .expect("only a painter with layers records");
        assert!(!layers.log.recording, "recordings do not nest");
        let frame = std::mem::take(&mut layers.log.hash);
        layers.log.recording = true;
        self.base
            .as_coeffs()
            .map(f64::to_bits)
            .hash(&mut layers.log.hash);
        std::mem::swap(self.scene, &mut scene);
        draw(self);
        std::mem::swap(self.scene, &mut scene);
        let layers = self.layers.as_deref_mut().expect("checked above");
        layers.log.recording = false;
        let hash = std::mem::replace(&mut layers.log.hash, frame).finish();
        (scene, hash)
    }
}

/// Encodes `chains` into `scene` straight into vello's path stream: no `PathEl` per segment.
/// This is what `Scene::stroke`/`Scene::fill` do internally (transform, style, path, brush),
/// minus the intermediate path.
fn encode_chains(
    scene: &mut Scene,
    base: Affine,
    chains: &Chains,
    closed: bool,
    stroke: Option<f64>,
    color: Color,
) {
    let enc = scene.encoding_mut();
    enc.encode_transform(Transform::from_kurbo(&base));
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
