//! `paint`: [`Picture`] → pixels, the humble view of one frame (architecture.md, frame
//! spine step 12; D5, D39). Draws the editor's tables through a [`Painter`] in a fixed
//! order: UI main layer, the canvas (`crate::canvas`: grid, tables, marks, gesture overlay)
//! unless a text tab replaces it, the docs page's graphs, the UI overlay (menus, dialogs),
//! then the waves dock below. Every frame is complete: a pan or zoom writes no row, an edit
//! only the rows it changed.
//!
//! Owner: `schemify-app`. Writes only [`Caches`]; the painter's rows live with the painter
//! (the window's for the GPU, the CPU painter's own). Reports back only through
//! `Editor::painted` (whether the canvas diff took the scene's edits) and
//! `Editor::rastered` (a PNG export's bytes).
//!
//! Contents:
//! - `pub struct Caches`
//! - `pub fn frame<P: Painter>(ed: &mut Editor, caches: &mut Caches, p: &mut P)`
//! - `pub fn serve_raster(ed: &mut Editor)`

use schemify_canvas::kurbo::{Affine, BezPath, Rect as KRect, Stroke};
use schemify_editor::editor::{DockPicture, Editor, GraphPicture, Picture, Raster, UiView};
use schemify_render::canvas::{self as draw, CanvasScratch};
use schemify_render::peniko::{Color, FontData};
use schemify_render::waves::{WaveLook, WaveScratch, draw_pane, draw_window};
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{CpuPainter, Glyph, Painter, rgba};
use schemify_ui::{Layer, NO_CLIP, TextSystem};

use crate::canvas::CanvasCache;

/// Everything paint keeps between frames: caches only, each rebuilt from the tables if
/// dropped, so a fresh `Caches::default()` paints the same pixels (one full repack slower).
#[derive(Default)]
pub struct Caches {
    /// The canvas diff, the per-table label indexes and glyph scratch.
    canvas: CanvasCache,
    /// The window UI's row keys and buffers.
    ui: UiScratch,
    /// Embedded graph chrome and buffers (docs plot slots).
    plot_look: WaveLook,
    plot: WaveScratch,
    /// The waves dock's chrome, buffers and UI.
    dock_look: WaveLook,
    dock: WaveScratch,
    dock_ui: UiScratch,
}

/// A painter that draws `dy` logical pixels lower: the waves dock lays out at its own
/// origin and is painted below the editor through this.
// ponytail: paths are copied per call (the dock draws hundreds, not the canvas's hundreds of
// thousands); translate in the painter if the dock ever draws as many.
struct Shifted<'a, P: Painter> {
    p: &'a mut P,
    dy: f64,
}

/// Paints the editor's next frame and hands back what its canvas diff took
/// (`Editor::painted`): the whole paint step of a window or a headless golden test.
pub fn frame<P: Painter>(ed: &mut Editor, caches: &mut Caches, p: &mut P) {
    let Picture {
        mut ui,
        canvas,
        graphs,
        dock,
    } = ed.picture();
    draw_layer(p, &mut ui, Layer::Main, &mut caches.ui);
    // False while a text tab replaces the canvas: the scene's edits stay reported.
    let took = canvas.is_some_and(|cp| caches.canvas.paint(p, &cp, &mut *ui.text));
    paint_graphs(p, &graphs, &mut *ui.text, caches);
    draw_layer(p, &mut ui, Layer::Overlay, &mut caches.ui);
    if let Some(d) = dock {
        assert!(d.dy >= 0.0, "the dock sits below the editor");
        let dy = d.dy;
        paint_dock(&mut Shifted { p, dy }, d, caches);
    }
    ed.painted(took);
}

/// Fulfils the PNG export the last frame asked for (`Editor::take_raster`), if any: the
/// canvas rasterized on the CPU and handed back through `Editor::rastered`, with the error
/// when a text tab replaces the canvas or encoding fails.
///
/// # Panics
///
/// The requested raster is under 1 or over `u16::MAX` pixels on a side (`Editor` clamps it).
pub fn serve_raster(ed: &mut Editor) {
    if let Some(r) = ed.take_raster() {
        let bytes = raster_png(ed, &r);
        ed.rastered(&r, bytes);
    }
}

impl<P: Painter> Painter for Shifted<'_, P> {
    fn fill_rect(&mut self, r: KRect, color: Color) {
        let r = self.rect(r);
        self.p.fill_rect(r, color);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        let path = self.path(path);
        self.p.fill_path(&path, color);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        let path = self.path(path);
        self.p.stroke_path(&path, width, color);
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        let path = self.path(path);
        self.p.stroke_styled(&path, stroke, color);
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
        let at = Affine::translate((0.0, self.dy)) * at;
        self.p.glyphs(font, px, coords, color, at, glyphs);
    }

    fn push_clip(&mut self, r: KRect) {
        let r = self.rect(r);
        self.p.push_clip(r);
    }

    fn pop_clip(&mut self) {
        self.p.pop_clip();
    }
}

// ---- private ----

/// One layer of a UI's tables.
fn draw_layer<P: Painter>(p: &mut P, ui: &mut UiView<'_>, layer: Layer, scratch: &mut UiScratch) {
    draw_ui(
        p, ui.tables, ui.layout, ui.strings, ui.text, ui.state, layer, scratch,
    );
}

/// Each filled plot slot of the docs page as an embedded waves pane, clipped to the page.
fn paint_graphs<P: Painter>(
    p: &mut P,
    graphs: &[GraphPicture<'_>],
    text: &mut TextSystem,
    c: &mut Caches,
) {
    for g in graphs {
        let clipped = g.clip != NO_CLIP;
        if clipped {
            let r = g.clip;
            let (x0, y0) = (f64::from(r.x), f64::from(r.y));
            p.push_clip(KRect::new(x0, y0, x0 + f64::from(r.w), y0 + f64::from(r.h)));
        }
        let drag = schemify_ui::waves::Drag::None;
        draw_pane(
            p,
            g.rect,
            g.waves,
            0,
            false,
            drag,
            &c.plot_look,
            text,
            &mut c.plot,
        );
        if clipped {
            p.pop_clip();
        }
    }
}

/// The waves dock: its UI main layer, the panes, its overlay.
fn paint_dock<P: Painter>(p: &mut P, d: DockPicture<'_>, c: &mut Caches) {
    let DockPicture {
        mut ui,
        geo,
        waves,
        wave_ui,
        ..
    } = d;
    draw_layer(p, &mut ui, Layer::Main, &mut c.dock_ui);
    let look = &c.dock_look;
    draw_window(p, geo, waves, wave_ui, look, &mut *ui.text, &mut c.dock);
    draw_layer(p, &mut ui, Layer::Overlay, &mut c.dock_ui);
}

/// Rasterizes a PNG export's view of the canvas tables (grid, the scene as shown, decor;
/// no marks, no overlay) on the CPU and encodes it.
fn raster_png(ed: &mut Editor, r: &Raster) -> Result<Vec<u8>, String> {
    let (pw, ph) = ((r.w * r.scale).round(), (r.h * r.scale).round());
    let side = 1.0..=f64::from(u16::MAX);
    assert!(side.contains(&pw), "raster width {pw}");
    assert!(side.contains(&ph), "raster height {ph}");
    let (pw, ph) = (pw as u16, ph as u16);
    let pic = ed.picture();
    let text = pic.ui.text;
    let cp = pic.canvas.ok_or("a text tab shows no canvas")?;
    let mut p = CpuPainter::new(pw, ph, r.scale);
    let view = KRect::new(0.0, 0.0, r.w, r.h);
    let mut scratch = CanvasScratch::default();
    p.fill_rect(view, rgba(cp.palette.background));
    let paths = &mut scratch.paths;
    draw::draw_grid(&mut p, view, &r.cam, cp.grid, cp.palette.grid, paths);
    for prims in [cp.shown.unwrap_or(cp.scene), cp.decor] {
        draw::draw_prims(&mut p, view, &r.cam, prims, &cp.palette, text, &mut scratch);
    }
    let px = p.finish(Color::BLACK);
    assert_eq!((px.width(), px.height()), (pw, ph));
    px.into_png().map_err(|e| format!("png: {e:?}"))
}

impl<P: Painter> Shifted<'_, P> {
    fn rect(&self, r: KRect) -> KRect {
        KRect::new(r.x0, r.y0 + self.dy, r.x1, r.y1 + self.dy)
    }

    fn path(&self, path: &BezPath) -> BezPath {
        let mut out = path.clone();
        out.apply_affine(Affine::translate((0.0, self.dy)));
        out
    }
}
