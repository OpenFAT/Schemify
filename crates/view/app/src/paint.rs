//! `paint`: [`Picture`] → pixels, the humble view of one frame. Draws the editor's tables
//! through a [`Painter`] in a fixed order: UI main layer, canvas (grid, scene, decor,
//! markers, extra and marks through the painter's GPU rows (`Painter::canvas`, its texts
//! over them) or as a cached raster layer on the CPU, then the gesture overlay live) or the
//! text tab, the docs page's graphs, the UI overlay (menus, dialogs), then the waves dock
//! below.
//!
//! Owner: `schemify-app`. Writes only [`Caches`] (the canvas raster layer, its diff and
//! indexes, glyph and path buffers; the GPU rows live with the window's painter) and
//! reports the canvas outcome, which [`frame`] hands to `Editor::painted`, the one way paint
//! informs the model.

use schemify_canvas::kurbo::{Affine, BezPath, Rect as KRect, Stroke};
use schemify_canvas::{Camera, CanvasDiff, CanvasPrims, Drawn, PrimIndex, Rows, TableChange};
use schemify_editor::editor::{CanvasPicture, DockPicture, Editor, GraphPicture, Picture, UiView};
use schemify_render::canvas::{self, CanvasCache, CanvasScratch};
use schemify_render::gpu::{Change, Frame};
use schemify_render::vello::Glyph;
use schemify_render::vello::peniko::{Color, FontData};
use schemify_render::waves::{WaveLook, WaveScratch, draw_window};
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{Painter, rgba};
use schemify_ui::{Layer, TextSystem};

/// The `gens` entry of each canvas table in drawing order (scene, decor, markers, extra);
/// entry 3 is the selection marks, which recolor scene rows.
const CANVAS_GEN: [usize; 4] = [0, 1, 4, 2];

/// Tables smaller than this are drawn whole: culling them costs less than an index.
const INDEX_ROWS_MIN: usize = 4096;

/// Everything paint keeps between frames: caches only, rebuilt from the tables if dropped.
#[derive(Default)]
pub struct Caches {
    /// Label glyphs and path buffers of every canvas table (scene, decor, overlay).
    canvas_scratch: CanvasScratch,
    /// The canvas's raster layer (D39: pan/zoom frames reuse it instead of re-encoding).
    canvas_cache: CanvasCache,
    /// What changed in the canvas tables since the last painted frame (an edit patches only
    /// there): the diff's snapshot, the generations and style key it was taken at, and the
    /// changed boxes (`None` = unknown).
    canvas_diff: (CanvasDiff, [u64; 5], u64, Option<Vec<[i32; 4]>>),
    /// Whether that diff last read the scene's own table (not `shown`): only then do the
    /// scene's reported edits describe what changed since.
    canvas_scene: bool,
    /// Spatial indexes of the canvas tables in drawing order (the shown scene, decor,
    /// violation markers, extra; with the generation each was built for), so recording one
    /// canvas tile looks only at the rows near it; and the rows buffer.
    canvas_index: [(u64, PrimIndex); 4],
    canvas_rows: Rows,
    /// Per canvas table the largest label size (milli-units) and the generation it was
    /// taken at plus 1: the GPU path skips a table's labels when even that one is under the
    /// text cut.
    text_max: [(u64, i32); 4],
    ui_scratch: UiScratch,
    /// Embedded graph chrome and buffers (docs plot slots).
    plot_look: WaveLook,
    plot_scratch: WaveScratch,
    /// The waves dock's chrome and buffers.
    dock_look: WaveLook,
    dock_scratch: WaveScratch,
    dock_ui: UiScratch,
}

/// Paints the editor's next frame and reports its canvas outcome back to it: the whole
/// paint step of a window or a headless golden test. Returns the outcome it reported.
pub fn frame<P: Painter>(ed: &mut Editor, caches: &mut Caches, p: &mut P) -> Option<Drawn> {
    let (drawn, took_edits) = paint(ed.picture(), caches, p);
    ed.painted(drawn, took_edits);
    drawn
}

/// Paints `pic`. Returns how the canvas was drawn (`None` while a text tab replaces it) and
/// whether the canvas diff took the scene's and the markers' reported edits.
pub fn paint<P: Painter>(pic: Picture<'_>, c: &mut Caches, p: &mut P) -> (Option<Drawn>, bool) {
    let Picture {
        mut ui,
        canvas,
        graphs,
        dock,
    } = pic;
    draw_layer(p, &mut ui, Layer::Main, &mut c.ui_scratch);
    let (drawn, took) = match canvas {
        Some(cp) => {
            let (d, t) = paint_canvas(p, &cp, &mut *ui.text, c);
            (Some(d), t)
        }
        None => (None, false),
    };
    paint_graphs(p, &graphs, &mut *ui.text, c);
    draw_layer(p, &mut ui, Layer::Overlay, &mut c.ui_scratch);
    if let Some(d) = dock {
        let dy = d.dy;
        paint_dock(&mut Shifted { p, dy }, d, c);
    }
    (drawn, took)
}

/// One layer of a UI's tables.
fn draw_layer<P: Painter>(p: &mut P, ui: &mut UiView<'_>, layer: Layer, scratch: &mut UiScratch) {
    draw_ui(
        p, ui.tables, ui.layout, ui.strings, ui.text, ui.state, layer, scratch,
    );
}

/// The canvas tables through the raster layer, then the live gesture overlay.
fn paint_canvas<P: Painter>(
    p: &mut P,
    cp: &CanvasPicture<'_>,
    text: &mut TextSystem,
    c: &mut Caches,
) -> (Drawn, bool) {
    let (view, cam, grid, palette) = (cp.view, cp.cam, cp.grid, &cp.palette);
    // Everything but the gesture preview goes through the raster layer: a pan or zoom
    // frame re-encodes nothing and an edit redraws only where its tables changed (D39).
    // The palette as the View/Layers options shape it (hidden layers, dimming, colors,
    // widths) and the grid are the `style`: no damage box describes a change of either,
    // so it records afresh.
    use std::hash::{Hash, Hasher};
    let style = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            grid,
            palette.color,
            palette.fill,
            palette.background,
            palette.grid,
        )
            .hash(&mut h);
        palette.width.map(f32::to_bits).hash(&mut h);
        h.finish()
    };
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (cp.gens, style).hash(&mut h);
        h.finish()
    };
    let shown = cp.shown.unwrap_or(cp.scene);
    let (diff, diff_gen, diff_style, boxes) = &mut c.canvas_diff;
    let diffed = *diff_gen != cp.gens || *diff_style != style;
    // What the GPU rows rewrite (`Painter::canvas`): nothing while the key stands.
    let mut changes = [Change::Same; 4];
    let mut marks_changed = false;
    if diffed {
        // The scene reports its own edits (O(change)); the decor and parity tables are
        // hashed when they changed.
        let mut out = boxes.take().unwrap_or_default();
        out.clear();
        let changed = |k: usize| diff_gen[k] != cp.gens[k];
        let any = |k: usize| match changed(k) {
            true => TableChange::Any,
            false => TableChange::Same,
        };
        let reported = (cp.shown.is_none() && c.canvas_scene)
            .then_some(cp.scene_edits)
            .flatten();
        let scene = match (changed(0), reported) {
            (true, Some(killed)) => TableChange::Rows(killed),
            _ => any(0),
        };
        let markers = match (changed(4), cp.markers_edits) {
            (true, Some(killed)) => TableChange::Rows(killed),
            _ => any(4),
        };
        let tables = [
            (shown, scene),
            (cp.decor, any(1)),
            (cp.markers, markers),
            (cp.extra, any(2)),
        ];
        let marks = (cp.scene, cp.marks, changed(0) || changed(3));
        let known = diff.update(&tables, marks, &mut out);
        // The GPU rows bake the palette, so a style change repacks everything. Otherwise each
        // table's own change: its producer's report is exact whatever the diff made of the
        // other tables; a hashed table's edit script only when the diff could follow.
        let restyled = *diff_style != style;
        for (k, &(_, change)) in tables.iter().enumerate() {
            changes[k] = match change {
                _ if restyled => Change::All,
                TableChange::Same => Change::Same,
                TableChange::Rows(killed) => Change::Killed(killed),
                TableChange::Any if known => Change::Edits(diff.edits_of(k)),
                TableChange::Any => Change::All,
            };
        }
        marks_changed = restyled || marks.2;
        // An index current before the change follows it (a patch then reads the rows near
        // it, not every row); else it waits for the next tile to rebuild it.
        for (k, ix) in c.canvas_index.iter_mut().enumerate() {
            let g = CANVAS_GEN[k];
            if !(known && changed(g) && ix.0 == diff_gen[g] + 1) {
                continue;
            }
            let follows = match tables[k].1 {
                TableChange::Rows(_) => ix.1.follow(tables[k].0),
                _ => ix.1.apply(diff.edits_of(k)),
            };
            if follows {
                ix.0 = cp.gens[g] + 1;
            }
        }
        *boxes = (known && *diff_style == style).then_some(out);
        (*diff_gen, *diff_style) = (cp.gens, style);
        // The diff has taken the scene's and the markers' edits so far.
        c.canvas_scene = cp.shown.is_none();
    } else if let Some(b) = boxes.as_mut() {
        b.clear(); // the same key: nothing changed since the last frame
    }
    let damage = boxes.as_deref();
    let (decor, extra, marks, markers) = (cp.decor, cp.extra, cp.marks, cp.markers);
    let scratch = &mut c.canvas_scratch;
    // A painter with GPU rows draws every table but its texts in its own pass under this
    // frame; the texts (culled, under 3 px skipped) and the gesture overlay go over it as
    // before. The CPU painter, the goldens' oracle, falls through to the tile cache.
    let frame = Frame {
        view,
        cam,
        grid,
        background: palette.background,
        grid_color: palette.grid,
    };
    let tables = [
        (shown, changes[0]),
        (decor, changes[1]),
        (markers, changes[2]),
        (extra, changes[3]),
    ];
    let (index, rows, gens) = (&mut c.canvas_index, &mut c.canvas_rows, cp.gens);
    if p.canvas(&frame, tables, (cp.scene, marks, marks_changed), palette) {
        p.push_clip(view);
        let near = world_box(view, &cam);
        for (k, t) in [shown, decor, markers, extra].into_iter().enumerate() {
            // A table whose largest label is under the text cut at this zoom draws none (the
            // far zoom's frames look at no label); else only the rows near the view.
            let g = gens[CANVAS_GEN[k]];
            let largest = &mut c.text_max[k];
            if largest.0 != g + 1 {
                *largest = (g + 1, t.texts.size.iter().copied().max().unwrap_or(0));
            }
            if f64::from(largest.1) * cam.scale < canvas::TEXT_MIN_PX {
                continue;
            }
            let listed = rows_near(&mut index[k], t, g, true, near, rows);
            let listed = listed.then_some(&rows.texts[..]);
            canvas::draw_texts(p, view, &cam, &t.texts, listed, palette, text, scratch);
        }
        if !cp.overlay.is_empty() {
            canvas::draw_prims(p, view, &cam, cp.overlay, palette, text, scratch);
        }
        p.pop_clip();
        return (Drawn::Direct, diffed);
    }
    // A sixteenth of a tile: the smallest part a costly tile is recorded in.
    let part_area = (f64::from(canvas::TILE_PX) / 4.0).powi(2) * 0.99;
    let drawn = canvas::draw_cached(
        p,
        &mut c.canvas_cache,
        view,
        &cam,
        key,
        damage,
        cp.crisp,
        rgba(palette.background),
        |p, v, cam| {
            p.fill_rect(v, rgba(palette.background));
            canvas::draw_grid(p, v, cam, grid, palette, scratch);
            // A tile or a part of one looks at the rows near it (the index, rebuilt after a
            // change); a smaller patch (an edit's, right after the change) culls every row
            // while the index is stale.
            let tile = v.area() * p.device_scale().powi(2) >= part_area;
            let near = world_box(v, cam);
            let mut cost = 0;
            for (k, t) in [shown, decor, markers, extra].into_iter().enumerate() {
                let listed = rows_near(&mut index[k], t, gens[CANVAS_GEN[k]], tile, near, rows);
                let listed = listed.then_some(&*rows);
                cost += canvas::draw_prims_in(p, v, cam, t, listed, palette, text, scratch);
            }
            canvas::draw_marks(p, v, cam, cp.scene, marks, scratch);
            cost
        },
    );
    if !cp.overlay.is_empty() {
        p.push_clip(view);
        canvas::draw_prims(p, view, &cam, cp.overlay, palette, text, scratch);
        p.pop_clip();
    }
    (drawn, diffed)
}

/// Each filled plot slot of the docs page as an embedded waves pane, clipped to the page.
fn paint_graphs<P: Painter>(
    p: &mut P,
    graphs: &[GraphPicture<'_>],
    text: &mut TextSystem,
    c: &mut Caches,
) {
    for g in graphs {
        let clipped = g.clip != schemify_ui::NO_CLIP;
        if clipped {
            let r = g.clip;
            p.push_clip(KRect::new(
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.x + r.w),
                f64::from(r.y + r.h),
            ));
        }
        schemify_render::waves::draw_pane(
            p,
            g.rect,
            g.waves,
            0,
            false,
            schemify_ui::waves::Drag::None,
            &c.plot_look,
            text,
            &mut c.plot_scratch,
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
    draw_window(
        p,
        geo,
        waves,
        wave_ui,
        &c.dock_look,
        &mut *ui.text,
        &mut c.dock_scratch,
    );
    draw_layer(p, &mut ui, Layer::Overlay, &mut c.dock_ui);
}

/// Rasterizes a PNG export's view of the canvas tables (grid, the scene as shown, decor;
/// no marks, no overlay) on the CPU and encodes it.
///
/// # Errors
///
/// A text tab replaces the canvas, or PNG encoding fails.
pub fn raster_png(ed: &mut Editor, r: &schemify_editor::editor::Raster) -> Result<Vec<u8>, String> {
    let (pw, ph) = ((r.w * r.scale).round(), (r.h * r.scale).round());
    assert!(pw >= 1.0 && ph >= 1.0 && pw <= f64::from(u16::MAX) && ph <= f64::from(u16::MAX));
    let pic = ed.picture();
    let text = pic.ui.text;
    let cp = pic.canvas.ok_or("a text tab shows no canvas")?;
    let mut p = schemify_render::CpuPainter::new(pw as u16, ph as u16, r.scale);
    let view = KRect::new(0.0, 0.0, r.w, r.h);
    let mut scratch = CanvasScratch::default();
    p.fill_rect(view, rgba(cp.palette.background));
    canvas::draw_grid(&mut p, view, &r.cam, cp.grid, &cp.palette, &mut scratch);
    for prims in [cp.shown.unwrap_or(cp.scene), cp.decor] {
        canvas::draw_prims(&mut p, view, &r.cam, prims, &cp.palette, text, &mut scratch);
    }
    let px = p.finish(Color::BLACK);
    assert_eq!((px.width(), px.height()), (pw as u16, ph as u16));
    px.into_png().map_err(|e| format!("png: {e:?}"))
}

/// Fulfils the PNG export the last frame asked for, if any.
pub fn serve_raster(ed: &mut Editor) {
    if let Some(r) = ed.take_raster() {
        let bytes = raster_png(ed, &r);
        ed.rastered(&r, bytes);
    }
}

/// Lists in `rows` the rows of `t` near world box `near`, from its index `ix` (rebuilt for
/// generation `version` when stale, but only for a tile or a part of one: a small patch
/// right after an edit culls every row instead of paying for a rebuild). Returns false
/// (draw every row) for a small table or a stale index.
fn rows_near(
    ix: &mut (u64, PrimIndex),
    t: &CanvasPrims,
    version: u64,
    tile: bool,
    near: [i32; 4],
    rows: &mut Rows,
) -> bool {
    if t.len() < INDEX_ROWS_MIN {
        return false;
    }
    // Generation 0 is the never-built state of a fresh index, so the stored one is plus 1.
    if ix.0 != version + 1 {
        if !tile {
            return false;
        }
        *ix = (version + 1, PrimIndex::build(t));
    }
    ix.1.query(t, near, rows);
    true
}

/// The world box `view` shows through `cam`, widened by the patch pad (strokes and labels
/// reach that far beyond their rows' boxes).
fn world_box(view: KRect, cam: &Camera) -> [i32; 4] {
    let (a0, b0) = cam.to_world(view, view.x0, view.y0);
    let (a1, b1) = cam.to_world(view, view.x1, view.y1);
    let pad = canvas::PATCH_PAD_PX / cam.scale;
    let c = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    [
        c((a0.min(a1) - pad).floor()),
        c((b0.min(b1) - pad).floor()),
        c((a0.max(a1) + pad).ceil()),
        c((b0.max(b1) + pad).ceil()),
    ]
}

/// A painter that draws `dy` logical pixels lower: the waves dock lays out at its own origin
/// and is painted below the editor through this. Paths are copied per call (the dock draws
/// hundreds, not the canvas's hundreds of thousands).
pub struct Shifted<'a, P: Painter> {
    /// The window's painter.
    pub p: &'a mut P,
    /// Vertical offset in logical pixels.
    pub dy: f64,
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
