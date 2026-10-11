//! The canvas layer: batched schematic primitives in `i32` world milli-units (D33) drawn through
//! a pan/zoom [`Camera`].
//!
//! # Data design
//!
//! 1. **In → out.** [`CanvasPrims`] (filled by `elab`/`doc` later, synthetic in the demo) +
//!    [`Camera`] + [`Palette`] → one stroke path and one fill path per layer → painter calls.
//! 2. **How many.** 10^2–10^5 visible primitives per frame (D39 budgets 120 fps at 10^5).
//! 3. **How wide.** Coordinates `i32` milli-units (±2.1 M covers the corpus, D33); layer `u8`
//!    (xschem has 22); arc angles `i32` millidegrees; text size `i32` milli-units; screen math
//!    in `f64` (the camera) and `f32` only at the GPU boundary (vello's encoding).
//! 4. **Access.** Every frame reads all coordinates of one primitive kind in one linear pass;
//!    layer decides the batch. So each kind is SoA (`x0`, `y0`, … columns) and batching is by
//!    layer into reused paths: one draw call per (layer, stroke|fill), not per primitive.
//! 5. **Lifetime.** Prims: document lifetime (rebuilt by their owner on edit). Paths and glyph
//!    caches in [`CanvasScratch`]: reused across frames, never freed while the canvas lives.
//!    The picture itself: one raster layer per canvas ([`draw_cached`]), re-recorded only when
//!    the content key or the zoom changes or a pan leaves its margin, so a steady pan or zoom
//!    frame encodes nothing (D39).
//! 6. **Parallel.** The per-kind passes are independent (they could fill per-layer paths on
//!    separate threads), but they run only when the layer is re-recorded, so they stay serial.

use std::collections::HashMap;

use vello::Glyph;
use vello::kurbo::{Affine, BezPath, Rect};
use vello::peniko::FontData;

use schemify_ui::TextSystem;
use schemify_ui::parley::PositionedLayoutItem;

pub use crate::damage::{CanvasDiff, DIFF_EDITS_MAX};
use crate::painter::{Chains, Painter, rgba};

/// Number of palette layers.
pub const LAYERS: usize = 32;

/// Pan/zoom: the world point at the view center and the zoom in pixels per milli-unit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// World x (milli-units) at the view center.
    pub cx: f64,
    /// World y (milli-units, y down) at the view center.
    pub cy: f64,
    /// Logical pixels per milli-unit.
    pub scale: f64,
}

impl Camera {
    /// Smallest and largest zoom: 1 px per 10^6 milli-units to 10 px per milli-unit.
    pub const SCALE_MIN: f64 = 1e-6;
    /// Largest zoom.
    pub const SCALE_MAX: f64 = 10.0;

    /// World → screen for a view rectangle `view`.
    #[inline]
    pub fn to_screen(&self, view: Rect, x: i32, y: i32) -> (f64, f64) {
        let (ox, oy) = self.origin(view);
        (
            f64::from(x) * self.scale + ox,
            f64::from(y) * self.scale + oy,
        )
    }

    /// Screen position of world (0, 0): `screen = world · scale + origin`.
    #[inline]
    pub fn origin(&self, view: Rect) -> (f64, f64) {
        (
            view.center().x - self.cx * self.scale,
            view.center().y - self.cy * self.scale,
        )
    }

    /// Screen → world (unrounded milli-units).
    pub fn to_world(&self, view: Rect, sx: f64, sy: f64) -> (f64, f64) {
        let (ox, oy) = self.origin(view);
        ((sx - ox) / self.scale, (sy - oy) / self.scale)
    }

    /// Moves the picture by `(dx, dy)` screen pixels.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.cx -= dx / self.scale;
        self.cy -= dy / self.scale;
    }

    /// Multiplies the zoom by `factor`, keeping the world point under `(sx, sy)` fixed.
    pub fn zoom_at(&mut self, view: Rect, sx: f64, sy: f64, factor: f64) {
        assert!(factor > 0.0 && factor.is_finite(), "bad zoom factor");
        let (wx, wy) = self.to_world(view, sx, sy);
        self.scale = (self.scale * factor).clamp(Self::SCALE_MIN, Self::SCALE_MAX);
        let (nx, ny) = self.to_world(view, sx, sy);
        self.cx += wx - nx;
        self.cy += wy - ny;
    }

    /// Fits world bounds `[x0, y0, x1, y1]` into `view` with a 5% margin.
    pub fn fit(&mut self, view: Rect, b: [i32; 4]) {
        let (w, h) = (
            f64::from(b[2]) - f64::from(b[0]),
            f64::from(b[3]) - f64::from(b[1]),
        );
        assert!(w >= 0.0 && h >= 0.0, "inverted bounds");
        self.cx = (f64::from(b[0]) + f64::from(b[2])) / 2.0;
        self.cy = (f64::from(b[1]) + f64::from(b[3])) / 2.0;
        let s = (view.width() / w.max(1.0)).min(view.height() / h.max(1.0)) * 0.95;
        self.scale = s.clamp(Self::SCALE_MIN, Self::SCALE_MAX);
    }
}

/// Line segments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lines {
    /// Start x.
    pub x0: Vec<i32>,
    /// Start y.
    pub y0: Vec<i32>,
    /// End x.
    pub x1: Vec<i32>,
    /// End y.
    pub y1: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Axis-aligned rectangles (outlined; also filled when the layer's palette entry fills).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rects {
    /// Corner x.
    pub x0: Vec<i32>,
    /// Corner y.
    pub y0: Vec<i32>,
    /// Opposite corner x.
    pub x1: Vec<i32>,
    /// Opposite corner y.
    pub y1: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Circular arcs; a full circle is `sweep = 360_000`. Angles are millidegrees, clockwise on
/// screen (y down), like the orientation transform in format spec §3.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arcs {
    /// Center x.
    pub cx: Vec<i32>,
    /// Center y.
    pub cy: Vec<i32>,
    /// Radius.
    pub r: Vec<i32>,
    /// Start angle.
    pub start: Vec<i32>,
    /// Sweep angle.
    pub sweep: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
}

/// Point chains: polyline `i` uses points `end[i-1]..end[i]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polys {
    /// One past each chain's last point.
    pub end: Vec<u32>,
    /// Point x.
    pub x: Vec<i32>,
    /// Point y.
    pub y: Vec<i32>,
    /// Palette layer per chain.
    pub layer: Vec<u8>,
}

/// Text labels: top-left anchored, `size` = font size in milli-units.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Texts {
    /// Anchor x.
    pub x: Vec<i32>,
    /// Anchor y.
    pub y: Vec<i32>,
    /// Font size.
    pub size: Vec<i32>,
    /// Palette layer.
    pub layer: Vec<u8>,
    /// One past each label's last byte in `text`.
    pub end: Vec<u32>,
    /// UTF-8 bytes of all labels.
    pub text: String,
}

/// One canvas's primitives: a table per kind (existence-based; no per-primitive objects).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasPrims {
    /// Segments (wires, symbol lines).
    pub lines: Lines,
    /// Rectangles.
    pub rects: Rects,
    /// Arcs and circles.
    pub arcs: Arcs,
    /// Open polylines.
    pub polylines: Polys,
    /// Closed polygons (filled when the layer fills).
    pub polygons: Polys,
    /// Labels.
    pub texts: Texts,
}

impl CanvasPrims {
    /// Maximum primitives per kind.
    pub const CAPACITY: usize = 1 << 22;

    /// Appends a segment.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        let t = &mut self.lines;
        assert!(t.x0.len() < Self::CAPACITY && usize::from(layer) < LAYERS);
        t.x0.push(x0);
        t.y0.push(y0);
        t.x1.push(x1);
        t.y1.push(y1);
        t.layer.push(layer);
    }

    /// Appends a rectangle.
    pub fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, layer: u8) {
        let t = &mut self.rects;
        assert!(t.x0.len() < Self::CAPACITY && usize::from(layer) < LAYERS);
        t.x0.push(x0);
        t.y0.push(y0);
        t.x1.push(x1);
        t.y1.push(y1);
        t.layer.push(layer);
    }

    /// Appends an arc (`sweep = 360_000` for a circle).
    pub fn arc(&mut self, cx: i32, cy: i32, r: i32, start: i32, sweep: i32, layer: u8) {
        let t = &mut self.arcs;
        assert!(t.cx.len() < Self::CAPACITY && usize::from(layer) < LAYERS && r >= 0);
        t.cx.push(cx);
        t.cy.push(cy);
        t.r.push(r);
        t.start.push(start);
        t.sweep.push(sweep);
        t.layer.push(layer);
    }

    /// Appends a polyline (`closed = false`) or polygon (`closed = true`) of at least 2 points.
    pub fn poly(&mut self, pts: &[(i32, i32)], closed: bool, layer: u8) {
        assert!(
            pts.len() >= 2 && usize::from(layer) < LAYERS,
            "poly needs 2 points"
        );
        let t = if closed {
            &mut self.polygons
        } else {
            &mut self.polylines
        };
        assert!(t.end.len() < Self::CAPACITY);
        t.x.extend(pts.iter().map(|p| p.0));
        t.y.extend(pts.iter().map(|p| p.1));
        t.end.push(t.x.len() as u32);
        t.layer.push(layer);
    }

    /// Appends a label.
    pub fn text(&mut self, x: i32, y: i32, size: i32, layer: u8, s: &str) {
        let t = &mut self.texts;
        assert!(t.x.len() < Self::CAPACITY && usize::from(layer) < LAYERS && size > 0);
        t.x.push(x);
        t.y.push(y);
        t.size.push(size);
        t.layer.push(layer);
        t.text.push_str(s);
        t.end.push(t.text.len() as u32);
    }

    /// Total primitive count over all kinds.
    pub fn len(&self) -> usize {
        self.lines.x0.len()
            + self.rects.x0.len()
            + self.arcs.cx.len()
            + self.polylines.end.len()
            + self.polygons.end.len()
            + self.texts.x.len()
    }

    /// True when nothing is drawn.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Per-layer drawing style: variation by data, one row per layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Stroke/fill/text color, `0xRRGGBBAA`.
    pub color: [u32; LAYERS],
    /// Stroke width in logical pixels.
    pub width: [f32; LAYERS],
    /// Bit `l` set: closed shapes on layer `l` are also filled.
    pub fill: u32,
    /// Canvas background.
    pub background: u32,
    /// Grid dots.
    pub grid: u32,
}

impl Palette {
    /// The xschem dark scheme as it appears on screen (colors read off running xschem's
    /// screenshots, layer numbering from its Layers menu: 0 BG, 1 wire, 2 grid, 3 text, 4 symbol,
    /// 5 pin).
    // ponytail: hard-coded rows until the palette is a `ui_styles` selector set (`layer.N`).
    pub fn xschem_dark() -> Palette {
        let mut color = [0x8888_88ff; LAYERS];
        let named: [u32; 22] = [
            0x0000_00ff,
            0x00cc_ffff,
            0x5050_50ff,
            0xd0d0_d0ff,
            0x88dd_00ff,
            0xbb22_00ff,
            0x00a0_ffff,
            0xff70_ffff,
            0xffff_00ff,
            0x00ff_80ff,
            0xff80_00ff,
            0x8080_ffff,
            0xff40_40ff,
            0x40ff_ffff,
            0xc0c0_00ff,
            0xa040_ffff,
            0x40a0_40ff,
            0xffa0_a0ff,
            0xa0a0_ffff,
            0xe0e0_e0ff,
            0x80c0_ffff,
            0xffc0_40ff,
        ];
        color[..22].copy_from_slice(&named);
        Palette {
            color,
            width: [1.0; LAYERS],
            fill: 1 << 5,
            background: 0x0000_00ff,
            grid: 0x7070_70ff,
        }
    }
}

/// Primitive kind for [`Marks`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimKind {
    /// Row of [`CanvasPrims::lines`].
    Line,
    /// Row of [`CanvasPrims::rects`].
    Rect,
    /// Row of [`CanvasPrims::arcs`].
    Arc,
    /// Row of [`CanvasPrims::polylines`].
    Polyline,
    /// Row of [`CanvasPrims::polygons`].
    Polygon,
}

/// Recolored primitives drawn on top: selection highlight, visual-diff colors, annotation
/// markers. One table covers all three; group rows by color for fewer draw calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Which table.
    pub kind: Vec<PrimKind>,
    /// Row in that table.
    pub index: Vec<u32>,
    /// Color, `0xRRGGBBAA`.
    pub color: Vec<u32>,
}

impl Marks {
    /// Appends a mark.
    pub fn push(&mut self, kind: PrimKind, index: u32, color: u32) {
        self.kind.push(kind);
        self.index.push(index);
        self.color.push(color);
    }
}

/// Reused per-frame buffers and the shaped-label cache (glyphs per distinct string, shared by
/// every primitive table drawn through it). Keep one per canvas.
#[derive(Default)]
pub struct CanvasScratch {
    open: Vec<Chains>,
    closed: Vec<Chains>,
    curves: Vec<BezPath>,
    fill: Vec<BezPath>,
    dots: Chains,
    path: BezPath,
    text_face: Option<FontData>,
    /// Glyph range in `text_glyphs` of each string shaped so far.
    text_runs: HashMap<Box<str>, (u32, u32)>,
    text_glyphs: Vec<Glyph>,
    text_layout: schemify_ui::parley::Layout<()>,
}

impl std::fmt::Debug for CanvasScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasScratch")
            .field("strings", &self.text_runs.len())
            .field("glyphs", &self.text_glyphs.len())
            .finish()
    }
}

/// Labels are shaped once at this size and scaled by the camera.
const TEXT_REF_PX: f32 = 64.0;
/// Labels smaller than this on screen are skipped (unreadable, and the cheapest cull).
const TEXT_MIN_PX: f64 = 3.0;
/// Distinct strings the label cache keeps before it starts over (a phase boundary, like the UI
/// text cache).
const TEXT_RUNS_MAX: usize = 1 << 16;

/// The raster cache of one canvas: which painter layer holds its picture and for what camera.
/// One per canvas; the picture itself lives in the painter (see [`Painter::layer`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasCache {
    /// Painter layer slot (`< painter::LAYER_SLOTS`), distinct per canvas sharing a painter.
    pub slot: usize,
    /// Tag of the recorded layer; 0 = nothing recorded.
    stamp: u64,
    /// Content fingerprint the layer was recorded for.
    key: u64,
    /// Camera zoom it was recorded at.
    scale: f64,
    /// Screen position of world (0, 0) when recorded (logical px).
    origin: (f64, f64),
    /// Logical-px rectangle the layer covered when recorded (the view plus [`CACHE_MARGIN`]).
    rect: Rect,
    /// Key of the previous call (what its `damage` was relative to).
    last: u64,
    /// World boxes where the content of `last` differs from the layer's (`key`); `None` =
    /// unknown.
    pending: Option<Vec<[i32; 4]>>,
}

/// How [`draw_cached`] drew a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// The recorded layer, copied pixel for pixel at an integer offset (a pan).
    Copied,
    /// The recorded layer stretched to a new zoom (a stand-in until the next crisp frame).
    Stretched,
    /// The changed parts of the layer redrawn (see `damage` of [`draw_cached`]), then copied.
    Patched,
    /// Content drawn into a fresh layer, then copied.
    Recorded,
    /// Content drawn straight into the frame (the painter keeps no layers).
    Direct,
}

/// Share of the view's size recorded beyond each edge, so pans and zoom-outs up to it reuse the
/// layer (larger = rarer re-records, each costing more pixels).
pub const CACHE_MARGIN: f64 = 0.25;

/// Unique layer tags across every cache in the process.
static STAMPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Logical px a damage box is widened by: half the widest stroke, mark strokes, antialiasing
/// and pixel snapping. Content must not draw farther than this outside its rows' boxes.
pub const PATCH_PAD_PX: f64 = 8.0;
/// Patches are whole multiples of this many logical px (so of 16 device px at integral scales),
/// on the layer's own grid: vello's tiles, so every pixel outside the patches sees exactly the
/// path segments it saw when recorded, and pixel snapping lands alike inside them.
const PATCH_TILE: f64 = 16.0;
/// Most patches one frame redraws (each culls every primitive once).
const PATCHES_MAX: usize = 4;
/// Most distinct damaged parts merged down to [`PATCHES_MAX`] (the merge is quadratic).
const PARTS_MAX: usize = 64;
/// Most damage boxes kept between frames before the change counts as unknown.
const DAMAGE_BOXES_MAX: usize = 4096;

/// Draws a canvas through its raster cache: `draw(p, view, cam)` paints the content (it must
/// fill `view` opaquely with `background` first) and runs only when the cache cannot serve
/// `key` at `cam`. A pan by whole pixels copies the recorded layer; a zoom with
/// `crisp == false` stretches it (view pixels it no longer covers show `background`); new
/// content whose change is known and small redraws only the changed parts; anything else
/// records `view` plus [`CACHE_MARGIN`] afresh. Content outside the cache (gesture
/// previews) is drawn by the caller afterwards.
///
/// `key` must change whenever anything `draw` reads changes, except the camera. `damage`
/// says where the content for `key` differs from the content of the previous call's key: world
/// boxes (each row added or removed, e.g. from [`CanvasDiff::update`]; `draw` must paint
/// nothing farther than [`PATCH_PAD_PX`] outside them), empty when the key is the same, or
/// `None` when unknown.
///
/// Cost: O(1) when copied or stretched; `draw` per changed part (up to 4) when patched;
/// `draw` over view + margin when recorded.
#[allow(clippy::too_many_arguments)]
pub fn draw_cached<P: Painter>(
    p: &mut P,
    cache: &mut CanvasCache,
    view: Rect,
    cam: &Camera,
    key: u64,
    damage: Option<&[[i32; 4]]>,
    crisp: bool,
    background: vello::peniko::Color,
    mut draw: impl FnMut(&mut P, Rect, &Camera),
) -> Drawn {
    assert!(view.width() >= 0.0 && view.height() >= 0.0 && cam.scale > 0.0);
    let origin = cam.origin(view);
    // What changed since the layer was recorded: this call's damage on top of the earlier.
    if key != cache.last {
        cache.pending = match (cache.pending.take(), damage) {
            (Some(mut boxes), Some(d)) if boxes.len() + d.len() <= DAMAGE_BOXES_MAX => {
                boxes.extend_from_slice(d);
                Some(boxes)
            }
            _ => None,
        };
        cache.last = key;
    }
    if cache.stamp != 0 && view.area() > 0.0 {
        let (o0, r) = (cache.origin, cache.rect);
        if cache.scale == cam.scale {
            let (dx, dy) = (origin.0 - o0.0, origin.1 - o0.1);
            let aligned = (dx - dx.round()).abs() < 1e-6 && (dy - dy.round()).abs() < 1e-6;
            let at = r + vello::kurbo::Vec2::new(dx.round(), dy.round());
            let place = Affine::translate((at.x0, at.y0));
            if aligned && at.contains_rect(view) {
                if cache.key == key && p.draw_layer(cache.slot, cache.stamp, place, view, None) {
                    return Drawn::Copied;
                }
                if cache.key != key && patch(p, cache, &mut draw) {
                    cache.key = key;
                    cache.pending = Some(Vec::new());
                    let shown = p.draw_layer(cache.slot, cache.stamp, place, view, None);
                    assert!(shown, "a patched layer draws");
                    return Drawn::Patched;
                }
            }
        } else if !crisp && cache.key == key {
            // Layer px u ↔ screen u + r.p0 at the recorded camera ↔ world (u + r.p0 - o0) / s0
            // ↔ screen now: (u + r.p0 - o0) · s1/s0 + o1. Where a zoom-out uncovers the view,
            // background shows until the crisp frame (no re-record mid-gesture).
            let at = Affine::translate(origin)
                * Affine::scale(cam.scale / cache.scale)
                * Affine::translate((r.x0 - o0.0, r.y0 - o0.1));
            if p.draw_layer(cache.slot, cache.stamp, at, view, Some(background)) {
                return Drawn::Stretched;
            }
        }
    }
    // Record view + margin, its corner on the integer-pixel grid so later pans copy exactly.
    let s = p.device_scale();
    let (mx, my) = (
        (view.width() * CACHE_MARGIN).max(64.0),
        (view.height() * CACHE_MARGIN).max(64.0),
    );
    let (x0, y0) = ((view.x0 - mx).floor(), (view.y0 - my).floor());
    // ponytail: 8192 device px is wgpu's default texture limit; a bigger window (> 5.4k logical
    // px wide at scale 1) records without margin, then 8192 caps the picture itself.
    let w = (((view.x1 + mx - x0) * s).ceil() as u32).clamp(1, 8192);
    let h = (((view.y1 + my - y0) * s).ceil() as u32).clamp(1, 8192);
    let local = Rect::new(0.0, 0.0, f64::from(w) / s, f64::from(h) / s);
    // The same picture in layer coordinates: world (0, 0) lands at origin - (x0, y0).
    let lcam = Camera {
        cx: (local.center().x - (origin.0 - x0)) / cam.scale,
        cy: (local.center().y - (origin.1 - y0)) / cam.scale,
        scale: cam.scale,
    };
    let stamp = STAMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if p.layer(cache.slot, stamp, w, h, |p| draw(p, local, &lcam)) {
        *cache = CanvasCache {
            slot: cache.slot,
            stamp,
            key,
            scale: cam.scale,
            origin,
            rect: local + vello::kurbo::Vec2::new(x0, y0),
            last: key,
            pending: Some(Vec::new()),
        };
        let shown = p.draw_layer(cache.slot, stamp, Affine::translate((x0, y0)), view, None);
        assert!(shown, "a recorded layer draws");
        return Drawn::Recorded;
    }
    cache.stamp = 0;
    p.push_clip(view);
    draw(p, view, cam);
    p.pop_clip();
    Drawn::Direct
}

/// Redraws the parts of the recorded layer that `cache.pending` covers, at the camera it was
/// recorded with. False (the layer may be partly redrawn: record it afresh) when the damage
/// is unknown or too large, or a [`PATCH_TILE`] is no whole number of device px (patch
/// offsets must be whole device px and whole logical px, so pixel snapping lands alike).
///
/// A part redrawn alone matches the recording within ±1 per channel, not bit for bit: its
/// unsnapped coordinates (arcs, glyphs) round to `f32` at other magnitudes.
fn patch<P: Painter>(
    p: &mut P,
    cache: &CanvasCache,
    draw: &mut impl FnMut(&mut P, Rect, &Camera),
) -> bool {
    let s = p.device_scale();
    let Some(boxes) = cache.pending.as_deref() else {
        return false;
    };
    if (PATCH_TILE * s).fract() != 0.0 || s <= 0.0 {
        return false;
    }
    // Layer px of world (0, 0) and the layer's size, as recorded.
    let (ox, oy) = (
        cache.origin.0 - cache.rect.x0,
        cache.origin.1 - cache.rect.y0,
    );
    let (lw, lh) = (
        (cache.rect.width() * s).round(),
        (cache.rect.height() * s).round(),
    );
    let (pad, step) = (PATCH_PAD_PX * s, PATCH_TILE * s);
    let mut parts: Vec<Rect> = Vec::new();
    for b in boxes {
        let (x0, y0) = (
            f64::from(b[0]) * cache.scale + ox,
            f64::from(b[1]) * cache.scale + oy,
        );
        let (x1, y1) = (
            f64::from(b[2]) * cache.scale + ox,
            f64::from(b[3]) * cache.scale + oy,
        );
        let tile = |v: f64, up: bool| {
            let t = v / step;
            (if up { t.ceil() } else { t.floor() } * step).clamp(0.0, 1e9)
        };
        let part = Rect::new(
            tile(x0 * s - pad, false),
            tile(y0 * s - pad, false),
            tile(x1 * s + pad, true).min(lw),
            tile(y1 * s + pad, true).min(lh),
        );
        if part.x1 > part.x0 && part.y1 > part.y0 {
            parts.push(part);
        }
        if parts.len() > PARTS_MAX {
            return false; // scattered: one recording beats merging them
        }
    }
    // Merge overlapping parts, then the closest pairs until few are left.
    let mut merged = true;
    while merged {
        merged = false;
        'pairs: for i in 0..parts.len() {
            for j in i + 1..parts.len() {
                let overlap = parts[i].intersect(parts[j]).area() > 0.0;
                let grow = parts[i].union(parts[j]).area() - parts[i].area() - parts[j].area();
                if overlap || (parts.len() > PATCHES_MAX && grow <= 0.0) {
                    parts[i] = parts[i].union(parts[j]);
                    parts.swap_remove(j);
                    merged = true;
                    break 'pairs;
                }
            }
        }
        if !merged && parts.len() > PATCHES_MAX {
            let (mut best, mut cost) = ((0, 1), f64::INFINITY);
            for i in 0..parts.len() {
                for j in i + 1..parts.len() {
                    let c = parts[i].union(parts[j]).area();
                    if c < cost {
                        (best, cost) = ((i, j), c);
                    }
                }
            }
            parts[best.0] = parts[best.0].union(parts[best.1]);
            parts.swap_remove(best.1);
            merged = true;
        }
    }
    let area: f64 = parts.iter().map(Rect::area).sum();
    if area > lw * lh / 2.0 {
        return false; // most of the layer: one recording is cheaper than the culling passes
    }
    assert!(parts.len() <= PATCHES_MAX);
    for part in &parts {
        let (x, y, w, h) = (part.x0, part.y0, part.width(), part.height());
        // The part's picture in its own coordinates: world (0, 0) lands at (ox, oy) - (x, y)/s.
        let local = Rect::new(0.0, 0.0, w / s, h / s);
        let pcam = Camera {
            cx: (local.center().x - (ox - x / s)) / cache.scale,
            cy: (local.center().y - (oy - y / s)) / cache.scale,
            scale: cache.scale,
        };
        let (x, y, w, h) = (x as u32, y as u32, w as u32, h as u32);
        if !p.patch_layer(cache.slot, cache.stamp, x, y, w, h, |p| {
            draw(p, local, &pcam)
        }) {
            return false;
        }
    }
    true
}

/// Draws background, grid, primitives, overlay primitives and marks into `view`, clipped.
/// The diagonal route for the common case; each step is also public.
#[allow(clippy::too_many_arguments)]
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
    p.push_clip(view);
    p.fill_rect(view, rgba(palette.background));
    draw_grid(p, view, cam, grid_step, palette, scratch);
    draw_prims(p, view, cam, prims, palette, text, scratch);
    draw_prims(p, view, cam, overlay, palette, text, scratch);
    draw_marks(p, view, cam, prims, marks, scratch);
    p.pop_clip();
}

/// Grid dots every `step` milli-units, doubled until they are ≥ 10 px apart (xschem's rule).
pub fn draw_grid<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    step: i32,
    palette: &Palette,
    scratch: &mut CanvasScratch,
) {
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
        return; // degenerate zoom: no readable grid
    }
    let (ox, oy) = cam.origin(view);
    let dots = &mut scratch.dots;
    dots.clear();
    for j in j0..=j1 {
        let y = pixel(j as f64 * s * cam.scale + oy) as f32;
        for i in i0..=i1 {
            let x = pixel(i as f64 * s * cam.scale + ox) as f32;
            dots.push(&[[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]]);
        }
    }
    p.fill_chains(dots, rgba(palette.grid));
}

fn push_rect(path: &mut BezPath, x0: f64, y0: f64, x1: f64, y1: f64) {
    path.move_to((x0, y0));
    path.line_to((x1, y0));
    path.line_to((x1, y1));
    path.line_to((x0, y1));
    path.close_path();
}

/// Appends a circular arc as ≤ 4 cubics (one per started quarter turn); a sweep of a full turn
/// or more closes into a circle.
///
/// One `sin_cos` per arc plus a rotation per segment: kurbo's adaptive `Arc` costs ~8× more,
/// which dominated the 100k-primitive frame. Radial error ≤ 2.7e-4 · r (sub-pixel below
/// r = 3000 px).
fn push_arc(path: &mut BezPath, cx: f64, cy: f64, r: f64, start: f64, sweep: f64) {
    let full = sweep.abs() >= std::f64::consts::TAU;
    let sweep = sweep.clamp(-std::f64::consts::TAU, std::f64::consts::TAU);
    let n = ((sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as u32).clamp(1, 4);
    let step = sweep / f64::from(n);
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let (mut sn, mut cs) = start.sin_cos();
    let (ss, cc) = step.sin_cos();
    path.move_to((cx + r * cs, cy + r * sn));
    for _ in 0..n {
        let (s2, c2) = (sn * cc + cs * ss, cs * cc - sn * ss);
        path.curve_to(
            (cx + r * (cs - k * sn), cy + r * (sn + k * cs)),
            (cx + r * (c2 + k * s2), cy + r * (s2 - k * c2)),
            (cx + r * c2, cy + r * s2),
        );
        (sn, cs) = (s2, c2);
    }
    if full {
        path.close_path();
    }
}

/// Snaps a coordinate to the pixel center so 1 px strokes stay crisp. The 1e-6 px bias keeps a
/// coordinate that lands on a pixel edge on the same side whether its camera origin was
/// computed directly or rebuilt for a cached layer (they differ by rounding, ~1e-9 px).
#[inline]
fn snap(v: f64) -> f64 {
    pixel(v) + 0.5
}

/// The pixel `v` falls in, with [`snap`]'s bias.
#[inline]
fn pixel(v: f64) -> f64 {
    (v + 1e-6).floor()
}

/// Builds one stroke path and one fill path per layer from all primitive tables, then issues
/// at most two draw calls per used layer. Off-screen primitives are culled.
///
/// # Time complexity
///
/// O(primitives + glyphs of visible labels); allocation-free once `scratch` has grown.
pub fn draw_prims<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    let l = &prims.lines;
    assert!(
        l.x0.len() == l.y0.len()
            && l.x0.len() == l.x1.len()
            && l.x0.len() == l.y1.len()
            && l.x0.len() == l.layer.len()
    );
    let sc = &mut *scratch;
    sc.open.resize_with(LAYERS, Chains::default);
    sc.closed.resize_with(LAYERS, Chains::default);
    sc.curves.resize_with(LAYERS, BezPath::new);
    sc.fill.resize_with(LAYERS, BezPath::new);
    for c in sc.open.iter_mut().chain(sc.closed.iter_mut()) {
        c.clear();
    }
    for path in sc.curves.iter_mut().chain(sc.fill.iter_mut()) {
        path.truncate(0);
    }
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let cull = view.inflate(2.0, 2.0);
    let outside = |ax: f64, ay: f64, bx: f64, by: f64| {
        ax.max(bx) < cull.x0 || ax.min(bx) > cull.x1 || ay.max(by) < cull.y0 || ay.min(by) > cull.y1
    };
    let pt = |x: f64, y: f64| [snap(x) as f32, snap(y) as f32];

    for i in 0..l.x0.len() {
        let (ax, ay) = (f64::from(l.x0[i]) * s + ox, f64::from(l.y0[i]) * s + oy);
        let (bx, by) = (f64::from(l.x1[i]) * s + ox, f64::from(l.y1[i]) * s + oy);
        if outside(ax, ay, bx, by) {
            continue;
        }
        sc.open[usize::from(l.layer[i])].push(&[pt(ax, ay), pt(bx, by)]);
    }

    let r = &prims.rects;
    for i in 0..r.x0.len() {
        let (ax, ay) = (f64::from(r.x0[i]) * s + ox, f64::from(r.y0[i]) * s + oy);
        let (bx, by) = (f64::from(r.x1[i]) * s + ox, f64::from(r.y1[i]) * s + oy);
        if outside(ax, ay, bx, by) {
            continue;
        }
        let layer = usize::from(r.layer[i]);
        let (x0, y0, x1, y1) = (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by));
        sc.closed[layer].push(&[pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)]);
        if palette.fill & (1 << layer) != 0 {
            push_rect(&mut sc.fill[layer], x0, y0, x1, y1);
        }
    }

    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        let (cx, cy, rad) = (
            f64::from(a.cx[i]) * s + ox,
            f64::from(a.cy[i]) * s + oy,
            f64::from(a.r[i]) * s,
        );
        if outside(cx - rad, cy - rad, cx + rad, cy + rad) {
            continue;
        }
        let layer = usize::from(a.layer[i]);
        let (start, sweep) = (
            f64::from(a.start[i]).to_radians() / 1000.0,
            f64::from(a.sweep[i]).to_radians() / 1000.0,
        );
        push_arc(&mut sc.curves[layer], cx, cy, rad, start, sweep);
        if a.sweep[i].abs() >= 360_000 && palette.fill & (1 << layer) != 0 {
            push_arc(&mut sc.fill[layer], cx, cy, rad, start, sweep);
        }
    }

    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            assert!(
                end >= start + 2 && end <= polys.x.len(),
                "malformed poly table"
            );
            let layer = usize::from(polys.layer[k]);
            let chains = if closed {
                &mut sc.closed[layer]
            } else {
                &mut sc.open[layer]
            };
            for j in start..end {
                chains.pts.push(pt(
                    f64::from(polys.x[j]) * s + ox,
                    f64::from(polys.y[j]) * s + oy,
                ));
            }
            chains.end.push(chains.pts.len() as u32);
            if closed && palette.fill & (1 << layer) != 0 {
                let f = &mut sc.fill[layer];
                f.move_to((
                    f64::from(polys.x[start]) * s + ox,
                    f64::from(polys.y[start]) * s + oy,
                ));
                for j in start + 1..end {
                    f.line_to((
                        f64::from(polys.x[j]) * s + ox,
                        f64::from(polys.y[j]) * s + oy,
                    ));
                }
                f.close_path();
            }
            start = end;
        }
    }

    for layer in 0..LAYERS {
        let (color, width) = (rgba(palette.color[layer]), f64::from(palette.width[layer]));
        if !sc.fill[layer].is_empty() {
            p.fill_path(&sc.fill[layer], color);
        }
        if !sc.closed[layer].is_empty() {
            p.stroke_chains(&sc.closed[layer], true, width, color);
        }
        if !sc.open[layer].is_empty() {
            p.stroke_chains(&sc.open[layer], false, width, color);
        }
        if !sc.curves[layer].is_empty() {
            p.stroke_path(&sc.curves[layer], width, color);
        }
    }
    draw_texts(p, view, cam, &prims.texts, palette, text, scratch);
}

fn draw_texts<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    t: &Texts,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) {
    assert_eq!(t.x.len(), t.end.len());
    let (ox, oy) = cam.origin(view);
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        let px = f64::from(t.size[i]) * cam.scale;
        let (x, y) = (
            f64::from(t.x[i]) * cam.scale + ox,
            f64::from(t.y[i]) * cam.scale + oy,
        );
        let visible = px >= TEXT_MIN_PX && x < view.x1 && y < view.y1 && y + px * 2.0 > view.y0;
        if visible {
            let (g0, g1) = shaped(&t.text[start..end], text, scratch);
            if let (Some(face), true) = (scratch.text_face.clone(), g1 > g0) {
                let at = Affine::translate((x, y)) * Affine::scale(px / f64::from(TEXT_REF_PX));
                let color = rgba(palette.color[usize::from(t.layer[i])]);
                let glyphs = &scratch.text_glyphs[g0 as usize..g1 as usize];
                p.glyphs(&face, TEXT_REF_PX, &[], color, at, glyphs);
            }
        }
        start = end;
    }
}

/// The glyph range of `s` shaped at [`TEXT_REF_PX`] (positions relative to its top-left),
/// shaping it on first use: a string is shaped once per cache lifetime, however many labels or
/// frames show it.
fn shaped(s: &str, text: &mut TextSystem, scratch: &mut CanvasScratch) -> (u32, u32) {
    if let Some(&r) = scratch.text_runs.get(s) {
        return r;
    }
    if scratch.text_runs.len() >= TEXT_RUNS_MAX {
        scratch.text_runs.clear();
        scratch.text_glyphs.clear();
    }
    let g0 = scratch.text_glyphs.len() as u32;
    text.shape_into(s, TEXT_REF_PX, &mut scratch.text_layout);
    for line in scratch.text_layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                // ponytail: one face for all labels (the only registered UI font). Font
                // fallback would need a face per glyph range.
                scratch
                    .text_face
                    .get_or_insert_with(|| run.run().font().clone());
                scratch
                    .text_glyphs
                    .extend(run.positioned_glyphs().map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }));
            }
        }
    }
    let r = (g0, scratch.text_glyphs.len() as u32);
    scratch.text_runs.insert(s.into(), r);
    assert!(r.0 <= r.1);
    r
}

/// Redraws marked primitives in their mark color, 1 px wider than the base stroke.
pub fn draw_marks<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    marks: &Marks,
    scratch: &mut CanvasScratch,
) {
    assert!(marks.kind.len() == marks.index.len() && marks.kind.len() == marks.color.len());
    let (ox, oy) = cam.origin(view);
    let s = cam.scale;
    let pt = |x: i32, y: i32| (snap(f64::from(x) * s + ox), snap(f64::from(y) * s + oy));
    let mut i = 0;
    // One path per run of equal colors.
    while i < marks.kind.len() {
        let color = marks.color[i];
        let path = &mut scratch.path;
        path.truncate(0);
        while i < marks.kind.len() && marks.color[i] == color {
            let k = marks.index[i] as usize;
            match marks.kind[i] {
                PrimKind::Line => {
                    let l = &prims.lines;
                    path.move_to(pt(l.x0[k], l.y0[k]));
                    path.line_to(pt(l.x1[k], l.y1[k]));
                }
                PrimKind::Rect => {
                    let r = &prims.rects;
                    let (a, b) = (pt(r.x0[k], r.y0[k]), pt(r.x1[k], r.y1[k]));
                    push_rect(path, a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
                }
                PrimKind::Arc => {
                    let a = &prims.arcs;
                    let (cx, cy) = (f64::from(a.cx[k]) * s + ox, f64::from(a.cy[k]) * s + oy);
                    let (start, sweep) = (
                        f64::from(a.start[k]).to_radians() / 1000.0,
                        f64::from(a.sweep[k]).to_radians() / 1000.0,
                    );
                    push_arc(path, cx, cy, f64::from(a.r[k]) * s, start, sweep);
                }
                PrimKind::Polyline | PrimKind::Polygon => {
                    let polys = if marks.kind[i] == PrimKind::Polygon {
                        &prims.polygons
                    } else {
                        &prims.polylines
                    };
                    let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
                    for j in start..polys.end[k] as usize {
                        let q = pt(polys.x[j], polys.y[j]);
                        if j == start {
                            path.move_to(q)
                        } else {
                            path.line_to(q)
                        }
                    }
                    if marks.kind[i] == PrimKind::Polygon {
                        path.close_path();
                    }
                }
            }
            i += 1;
        }
        p.stroke_path(path, 2.0, rgba(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
