//! One drawing interface over two backends: vello's GPU [`Scene`] and the `vello_cpu` rasterizer
//! (golden images, GPU-less fallback, D5). Monomorphized, so it costs nothing at runtime.

use vello::kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use vello::low_level::vello_encoding::Transform;
use vello::peniko::{Color, Fill, FontData};
use vello::{Glyph, Scene};

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
}

impl Painter for ScenePainter<'_> {
    fn fill_rect(&mut self, r: Rect, color: Color) {
        self.scene.fill(Fill::NonZero, self.base, color, None, &r);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        self.scene.fill(Fill::NonZero, self.base, color, None, path);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
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
        self.scene
            .draw_glyphs(font)
            .font_size(px)
            .normalized_coords(coords)
            .transform(self.base * at)
            .brush(color)
            .draw(Fill::NonZero, glyphs.iter().copied());
    }

    fn push_clip(&mut self, r: Rect) {
        self.scene.push_clip_layer(Fill::NonZero, self.base, &r);
    }

    fn pop_clip(&mut self) {
        self.scene.pop_layer();
    }

    fn stroke_chains(&mut self, chains: &Chains, closed: bool, width: f64, color: Color) {
        assert!(width > 0.0, "zero-width stroke");
        self.encode_chains(chains, closed, Some(width), color);
    }

    fn fill_chains(&mut self, chains: &Chains, color: Color) {
        self.encode_chains(chains, true, None, color);
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

/// Paints with `vello_cpu` into a pixmap: golden images and the no-GPU fallback.
pub struct CpuPainter {
    ctx: vello_cpu::RenderContext,
    resources: vello_cpu::Resources,
    base: Affine,
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
        }
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
}
