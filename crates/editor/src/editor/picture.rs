//! `picture`: editor tables → [`Picture`], the borrowed, read-only input of one painted frame
//! (the humble view's input, gui-architecture step 3), and [`Editor::painted`], the one way
//! paint reports back. `schemify-app` paints a `Picture`; nothing here draws.
//!
//! Owner: the editor. Reads the shell's UI tables, the scene, decor, markers, the parity
//! extra, marks, overlay, the docs page's plot slots and the waves dock; writes nothing. Only
//! the two text-shaping caches are lent mutably (glyph caches, no model state).

use std::path::PathBuf;

use schemify_canvas::kurbo::Rect as KRect;
use schemify_canvas::{Camera, CanvasPrims, Drawn, Marks, Palette};
use schemify_ui::waves::{Geometry, WaveUi};
use schemify_ui::{Strings, TextSystem, UiLayout, UiState, UiTables};
use schemify_wave::Waves;

use super::Editor;

/// One UI's tables as drawn: the editor's window, or the waves dock's.
pub struct UiView<'a> {
    /// `ui_nodes`, `ui_styles`, `keymap`.
    pub tables: &'a UiTables,
    /// This frame's layout of `tables`.
    pub layout: &'a UiLayout,
    /// The strings every table names.
    pub strings: &'a Strings,
    /// Menus, dialog, hover and focus.
    pub state: &'a UiState,
    /// The shaped-text cache `layout` was measured with (paint shapes through it too).
    pub text: &'a mut TextSystem,
}

/// The canvas tables of one frame, in drawing order, with the generations they were
/// changed at (`gens[k]` advances whenever table `k` changes, so an unchanged generation
/// means unchanged rows).
pub struct CanvasPicture<'a> {
    /// The canvas rectangle in logical px.
    pub view: KRect,
    /// The active level's camera.
    pub cam: Camera,
    /// Grid pitch in milli-units (`i32::MAX / 4`: no grid).
    pub grid: i32,
    /// The palette as the View/Layers options shape it.
    pub palette: Palette,
    /// Generations of: scene as shown, decor, parity extra, selection marks, violation markers.
    pub gens: [u64; 5],
    /// The scene as built (the marks recolor its rows).
    pub scene: &'a CanvasPrims,
    /// The scene as displayed when the View options change it (instances as boxes, symbol
    /// texts off); `None`: `scene` itself.
    pub shown: Option<&'a CanvasPrims>,
    /// Rows of `scene` killed since [`Editor::painted`] last took them (`None`: unknown).
    pub scene_edits: Option<&'a [Vec<u32>; 6]>,
    /// Net highlight, staged diff and annotation decor.
    pub decor: &'a CanvasPrims,
    /// Violation markers.
    pub markers: &'a CanvasPrims,
    /// Rows of `markers` killed since [`Editor::painted`] last took them.
    pub markers_edits: Option<&'a [Vec<u32>; 6]>,
    /// The parity extra (compare overlay).
    pub extra: &'a CanvasPrims,
    /// Selection marks over `scene`.
    pub marks: &'a Marks,
    /// The gesture preview, drawn live over the cached layer.
    pub overlay: &'a CanvasPrims,
    /// The zoom has settled: draw crisp (else a stretched stand-in is fine).
    pub crisp: bool,
}

/// One filled docs-page plot slot: the pane rectangle, its clip and its traces.
pub struct GraphPicture<'a> {
    /// The `graph` node's laid-out rectangle.
    pub rect: schemify_ui::Rect,
    /// Its clip (`schemify_ui::NO_CLIP`: none).
    pub clip: schemify_ui::Rect,
    /// The one-pane viewer holding the traces.
    pub waves: &'a Waves,
}

/// The waves dock, laid out at its own origin and painted `dy` below the editor.
pub struct DockPicture<'a> {
    /// The dock's own UI tables.
    pub ui: UiView<'a>,
    /// Pane rectangles of this frame.
    pub geo: &'a Geometry,
    /// The viewer tables.
    pub waves: &'a Waves,
    /// The viewer's interaction state (drag, active pane, cursors).
    pub wave_ui: &'a WaveUi,
    /// Logical px the dock is painted below the window's top.
    pub dy: f64,
}

/// Everything one frame is painted from.
pub struct Picture<'a> {
    /// The window's UI.
    pub ui: UiView<'a>,
    /// The canvas; `None` while a text tab replaces it.
    pub canvas: Option<CanvasPicture<'a>>,
    /// The docs page's filled plot slots.
    pub graphs: Vec<GraphPicture<'a>>,
    /// The waves dock when open.
    pub dock: Option<DockPicture<'a>>,
}

/// A PNG export the editor asked for ([`Editor::take_raster`]): `schemify-app` rasterizes
/// the canvas tables through `cam` into a `w × h` logical-px image at `scale` device px per
/// logical px and hands the bytes back ([`Editor::rastered`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    /// Where the PNG goes.
    pub path: PathBuf,
    /// Image width in logical px (≥ 1).
    pub w: f64,
    /// Image height in logical px (≥ 1).
    pub h: f64,
    /// The view of the drawing.
    pub cam: Camera,
    /// Device px per logical px (> 0).
    pub scale: f64,
}

impl Editor {
    /// Borrows the tables of the next painted frame. Call after [`Editor::update`]; the
    /// borrow ends before the next one. A PNG export ([`Editor::take_raster`]) draws from
    /// the same picture's canvas.
    pub fn picture(&mut self) -> Picture<'_> {
        let text_tab = self.session.text_view().is_some();
        let (view, cam, palette) = (self.canvas_rect(), self.camera(), self.view_palette());
        let grid = if self.opts.show_grid {
            self.opts.grid
        } else {
            i32::MAX / 4
        };
        assert!(grid > 0, "a positive grid pitch");
        let crisp = self.now >= self.zoom_until;
        let dy = f64::from(self.viewport.1);
        assert!(dy >= 0.0, "a dock below the window's top");
        #[cfg(not(target_arch = "wasm32"))]
        let graphs = graph_pictures(&self.shell, &self.sim.plots, &self.graphs);
        #[cfg(target_arch = "wasm32")]
        let graphs = Vec::new();
        let Editor {
            shell,
            scene,
            shown,
            decor,
            markers,
            extra,
            marks,
            overlay,
            waves,
            canvas_gen,
            ..
        } = self;
        let canvas = (!text_tab).then(|| CanvasPicture {
            view,
            cam,
            grid,
            palette,
            gens: *canvas_gen,
            scene: &scene.prims,
            shown: shown.as_ref(),
            scene_edits: scene.edits(),
            decor,
            markers: &markers.prims,
            markers_edits: markers.edits(),
            extra,
            marks,
            overlay,
            crisp,
        });
        Picture {
            ui: UiView {
                tables: &shell.tables,
                layout: &shell.layout,
                strings: &shell.strings,
                state: &shell.ui,
                text: &mut shell.text,
            },
            canvas,
            graphs,
            dock: waves.as_mut().map(|w| w.picture(dy)),
        }
    }

    /// Takes the canvas outcome of the frame painted from [`Editor::picture`]: a stretched
    /// layer (mid-zoom) asks [`Editor::wake_ms`] for a crisp frame once the zoom settles, a
    /// partly recorded one for the next frame at once; any other outcome asks for nothing.
    /// `None` (no canvas was drawn) keeps what the last canvas frame asked for. `took_edits`:
    /// the painter's diff consumed the scene's and the markers' reported edits, so they are
    /// cleared (a painter that did not diff leaves them for the next frame).
    pub fn painted(&mut self, drawn: Option<Drawn>, took_edits: bool) {
        if took_edits {
            self.scene.clear_edits();
            self.markers.clear_edits();
        }
        let Some(d) = drawn else {
            return;
        };
        self.stretched = d == Drawn::Stretched;
        self.partial = d == Drawn::Partial;
        assert!(!(self.stretched && self.partial), "one outcome per frame");
    }

    /// The PNG export the last command asked for, if any (one at a time).
    pub fn take_raster(&mut self) -> Option<Raster> {
        let r = self.raster.take();
        assert!(self.raster.is_none());
        r
    }

    /// Writes the PNG `bytes` of `r` (or reports why there are none) and sets the status.
    pub fn rastered(&mut self, r: &Raster, bytes: Result<Vec<u8>, String>) {
        assert!(r.w >= 1.0 && r.h >= 1.0, "a drawable raster");
        // Web: a download (web spec §3).
        self.session.status = match bytes.and_then(|b| crate::platform::export(&r.path, &b)) {
            Ok(()) => format!("exported {}", r.path.display()),
            Err(e) => format!("export: {e}"),
        };
    }
}

/// Each filled plot slot of the docs page (`graph` nodes `graphs` names) with a drawable
/// rectangle, over the run's plot slots `plots`.
#[cfg(not(target_arch = "wasm32"))]
fn graph_pictures<'a>(
    s: &crate::shell::Shell,
    plots: &'a [crate::sim::Plot],
    graphs: &[(String, String)],
) -> Vec<GraphPicture<'a>> {
    let mut out = Vec::new();
    for (id, expr) in graphs {
        let Some(sym) = s.strings.find(id) else {
            continue;
        };
        let Some(row) = s.tables.nodes.find(sym).and_then(|n| s.layout.row_of(n)) else {
            continue;
        };
        let Some(Ok(waves)) = plots.iter().find(|x| x.expr == *expr).map(|x| &x.waves) else {
            continue;
        };
        let rect = s.layout.rect()[row];
        if rect.w < 16.0 || rect.h < 16.0 {
            continue;
        }
        let clip = s.layout.clip()[row];
        out.push(GraphPicture { rect, clip, waves });
    }
    assert!(out.len() <= graphs.len());
    out
}

impl Editor {
    /// Where the input method's candidate window goes (the focused text field), in logical
    /// px: `(x, y, w, h)`.
    pub fn ime_area(&self) -> Option<(f32, f32, f32, f32)> {
        let s = &self.shell;
        let r = s.ui.ime_area(&s.tables, &s.layout)?;
        assert!(r.w >= 0.0 && r.h >= 0.0);
        Some((r.x, r.y, r.w, r.h))
    }

    /// The accessibility tree of this frame's UI (D6: the UI is data, so is its tree).
    pub fn access_tree(&self) -> schemify_ui::accesskit::TreeUpdate {
        let s = &self.shell;
        schemify_ui::access::access_tree(&s.tables, &s.layout, &s.strings, &s.ui)
    }
}
