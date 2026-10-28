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
//! - `pub fn frame<P: Painter>(editor: &mut Editor, caches: &mut Caches, painter: &mut P)`
//! - `pub fn serve_raster(editor: &mut Editor)`

use schemify_canvas::kurbo::{Affine, BezPath, Rect as KRect, Stroke};
use schemify_editor::editor::{DockPicture, Editor, GraphPicture, Picture, Raster, UiView};
use schemify_render::canvas::{self as draw, CanvasScratch};
use schemify_render::peniko::{Color, FontData};
use schemify_render::waves::{self, WaveLook, WaveScratch};
use schemify_render::widgets::{self, UiScratch};
use schemify_render::{CpuPainter, Glyph, Painter};
use schemify_ui::waves::Drag;
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
struct Shifted<'painter, P: Painter> {
    painter: &'painter mut P,
    dy: f64,
    /// Clips pushed and not yet popped: each `pop_clip` matches a `push_clip`.
    clips: u32,
}

/// Paints the editor's next frame and hands back what its canvas diff took
/// (`Editor::painted`): the whole paint step of a window or a headless golden test.
///
/// # Panics
///
/// The editor's waves dock sits above its top (a negative `DockPicture::dy`), or a graph's
/// clip rectangle has a negative size: both are bugs in `Editor::picture`. The dock's drawing
/// pops a clip it did not push, or leaves one pushed: a bug in `schemify_render::waves`.
pub fn frame<P: Painter>(editor: &mut Editor, caches: &mut Caches, painter: &mut P) {
    let Picture {
        mut ui,
        canvas,
        graphs,
        dock,
    } = editor.picture();
    draw_layer(painter, &mut ui, Layer::Main, &mut caches.ui);
    // False while a text tab replaces the canvas: the scene's edits stay reported.
    let took = canvas.is_some_and(|picture| caches.canvas.paint(painter, &picture, &mut *ui.text));
    paint_graphs(painter, &graphs, &mut *ui.text, caches);
    draw_layer(painter, &mut ui, Layer::Overlay, &mut caches.ui);
    if let Some(dock) = dock {
        let dy = dock.dy;
        assert!(dy >= 0.0, "the dock sits below the editor, got dy {dy}");
        let mut shifted = Shifted {
            painter,
            dy,
            clips: 0,
        };
        paint_dock(&mut shifted, dock, caches);
        assert_eq!(shifted.clips, 0, "the dock pops every clip it pushes");
    }
    editor.painted(took);
}

/// Fulfils the PNG export the last frame asked for (`Editor::take_raster`), if any: the
/// canvas rasterized on the CPU and handed back through `Editor::rastered`, with the error
/// when a text tab replaces the canvas or encoding fails.
///
/// # Panics
///
/// The requested raster is under 1 or over `u16::MAX` pixels on a side (`Editor` clamps it),
/// or its scale is not positive.
pub fn serve_raster(editor: &mut Editor) {
    if let Some(raster) = editor.take_raster() {
        let bytes = raster_png(editor, &raster);
        editor.rastered(&raster, bytes);
    }
}

impl<P: Painter> Painter for Shifted<'_, P> {
    fn fill_rect(&mut self, rect: KRect, color: Color) {
        let rect = self.rect(rect);
        self.painter.fill_rect(rect, color);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        let path = self.path(path);
        self.painter.fill_path(&path, color);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        let path = self.path(path);
        self.painter.stroke_path(&path, width, color);
    }

    fn stroke_styled(&mut self, path: &BezPath, stroke: &Stroke, color: Color) {
        let path = self.path(path);
        self.painter.stroke_styled(&path, stroke, color);
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
        self.painter.glyphs(font, px, coords, color, at, glyphs);
    }

    fn push_clip(&mut self, rect: KRect) {
        let rect = self.rect(rect);
        self.painter.push_clip(rect);
        self.clips += 1;
    }

    fn pop_clip(&mut self) {
        assert!(self.clips > 0, "a pop_clip matches a push_clip of the dock");
        self.painter.pop_clip();
        self.clips -= 1;
    }
}

// ---- private ----

/// One layer of a UI's tables.
fn draw_layer<P: Painter>(
    painter: &mut P,
    ui: &mut UiView<'_>,
    layer: Layer,
    scratch: &mut UiScratch,
) {
    widgets::draw_ui(
        painter, ui.tables, ui.layout, ui.strings, ui.text, ui.state, layer, scratch,
    );
}

/// Each filled plot slot of the docs page as an embedded waves pane, clipped to the page.
fn paint_graphs<P: Painter>(
    painter: &mut P,
    graphs: &[GraphPicture<'_>],
    text: &mut TextSystem,
    caches: &mut Caches,
) {
    for graph in graphs {
        let clipped = graph.clip != NO_CLIP;
        if clipped {
            let clip = graph.clip;
            assert!(clip.w >= 0.0, "graph clip width {}", clip.w);
            assert!(clip.h >= 0.0, "graph clip height {}", clip.h);
            let (x0, y0) = (f64::from(clip.x), f64::from(clip.y));
            let (x1, y1) = (x0 + f64::from(clip.w), y0 + f64::from(clip.h));
            painter.push_clip(KRect::new(x0, y0, x1, y1));
        }
        waves::draw_pane(
            painter,
            graph.rect,
            graph.waves,
            0,     // the graph's only pane
            false, // never the active pane: an embedded graph takes no input
            Drag::None,
            &caches.plot_look,
            text,
            &mut caches.plot,
        );
        if clipped {
            painter.pop_clip();
        }
    }
}

/// The waves dock: its UI main layer, the panes, its overlay.
fn paint_dock<P: Painter>(painter: &mut P, dock: DockPicture<'_>, caches: &mut Caches) {
    let DockPicture {
        mut ui,
        geo,
        waves,
        wave_ui,
        ..
    } = dock;
    draw_layer(painter, &mut ui, Layer::Main, &mut caches.dock_ui);
    let look = &caches.dock_look;
    let scratch = &mut caches.dock;
    waves::draw_window(painter, geo, waves, wave_ui, look, &mut *ui.text, scratch);
    draw_layer(painter, &mut ui, Layer::Overlay, &mut caches.dock_ui);
}

/// Rasterizes a PNG export's view of the canvas tables (grid, the scene as shown, decor;
/// no marks, no overlay) on the CPU and encodes it.
fn raster_png(editor: &mut Editor, raster: &Raster) -> Result<Vec<u8>, String> {
    assert!(raster.scale > 0.0, "raster scale {}", raster.scale);
    let width_px = (raster.w * raster.scale).round();
    let height_px = (raster.h * raster.scale).round();
    let side = 1.0..=f64::from(u16::MAX);
    assert!(side.contains(&width_px), "raster width {width_px}");
    assert!(side.contains(&height_px), "raster height {height_px}");
    // In range of u16 by the asserts above.
    let (width_px, height_px) = (width_px as u16, height_px as u16);
    let picture = editor.picture();
    let text = picture.ui.text;
    let canvas = picture.canvas.ok_or("a text tab shows no canvas")?;
    let mut painter = CpuPainter::new(width_px, height_px, raster.scale);
    let view = KRect::new(0.0, 0.0, raster.w, raster.h);
    let mut scratch = CanvasScratch::default();
    let palette = &canvas.palette;
    painter.fill_rect(view, schemify_render::rgba(palette.background));
    let paths = &mut scratch.paths;
    draw::draw_grid(
        &mut painter,
        view,
        &raster.cam,
        canvas.grid,
        palette.grid,
        paths,
    );
    for prims in [canvas.shown.unwrap_or(canvas.scene), canvas.decor] {
        draw::draw_prims(
            &mut painter,
            view,
            &raster.cam,
            prims,
            palette,
            text,
            &mut scratch,
        );
    }
    let pixmap = painter.finish(Color::BLACK);
    assert_eq!(pixmap.width(), width_px, "the CPU painter's own size");
    assert_eq!(pixmap.height(), height_px, "the CPU painter's own size");
    pixmap.into_png().map_err(|error| format!("png: {error:?}"))
}

impl<P: Painter> Shifted<'_, P> {
    fn rect(&self, rect: KRect) -> KRect {
        KRect::new(rect.x0, rect.y0 + self.dy, rect.x1, rect.y1 + self.dy)
    }

    fn path(&self, path: &BezPath) -> BezPath {
        let mut out = path.clone();
        out.apply_affine(Affine::translate((0.0, self.dy)));
        out
    }
}
