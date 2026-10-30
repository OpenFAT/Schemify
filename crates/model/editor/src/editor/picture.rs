//! `picture`: the canvas tables → [`CanvasPicture`], the borrowed, read-only input of the
//! canvas painter (the humble view's input, gui-architecture step 3), the docs page's plots
//! painted into their slots, and [`Editor::painted`], the one way the painter reports back.
//! `schemify-app` turns a `CanvasPicture` into GPU rows and labels; nothing here touches a GPU.
//!
//! Owner: the editor. Reads the scene, decor, markers, the parity extra, marks, overlay and
//! the docs page's plot slots; writes nothing.
//!
//! Contents:
//! - `pub struct CanvasPicture`
//! - `pub struct Raster`
//! - `Editor::canvas_picture`
//! - `Editor::painted`
//! - `Editor::take_raster`
//! - `Editor::rastered`
//! - crate-only `Editor::paint_graphs`

use std::path::PathBuf;

use schemify_canvas::kurbo::Rect as KRect;
use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};

use super::Editor;

/// The canvas tables of one frame, in drawing order, with the generations they were
/// changed at (`gens[k]` advances whenever table `k` changes, so an unchanged generation
/// means unchanged rows).
pub struct CanvasPicture<'frame> {
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
    pub scene: &'frame CanvasPrims,
    /// The scene as displayed when the View options change it (instances as boxes, symbol
    /// texts off); `None`: `scene` itself.
    pub shown: Option<&'frame CanvasPrims>,
    /// Rows of `scene` killed since [`Editor::painted`] last took them (`None`: unknown).
    pub scene_edits: Option<&'frame [Vec<u32>; 6]>,
    /// Net highlight, staged diff and annotation decor.
    pub decor: &'frame CanvasPrims,
    /// Violation markers.
    pub markers: &'frame CanvasPrims,
    /// Rows of `markers` killed since [`Editor::painted`] last took them.
    pub markers_edits: Option<&'frame [Vec<u32>; 6]>,
    /// The parity extra (compare overlay).
    pub extra: &'frame CanvasPrims,
    /// Selection marks over `scene`.
    pub marks: &'frame Marks,
    /// The gesture preview, drawn over the other tables.
    pub overlay: &'frame CanvasPrims,
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
    /// Borrows the canvas tables of the frame being painted; `None` while a text tab replaces
    /// the canvas. The borrow ends before the next [`Editor::update`]. A PNG export
    /// ([`Editor::take_raster`]) draws from the same tables through its own camera.
    pub fn canvas_picture(&self) -> Option<CanvasPicture<'_>> {
        if self.session.text_view().is_some() {
            return None;
        }
        let grid = if self.opts.show_grid {
            self.opts.grid
        } else {
            i32::MAX / 4
        };
        assert!(grid > 0, "a positive grid pitch");
        let p = CanvasPicture {
            view: self.canvas_rect(),
            cam: self.camera(),
            grid,
            palette: self.view_palette(),
            gens: self.canvas_gen,
            scene: &self.scene.prims,
            shown: self.shown.as_ref(),
            scene_edits: self.scene.edits(),
            decor: &self.decor,
            markers: &self.markers.prims,
            markers_edits: self.markers.edits(),
            extra: &self.extra,
            marks: &self.marks,
            overlay: &self.overlay,
        };
        debug_assert!(p.cam.scale > 0.0, "a positive camera scale");
        Some(p)
    }

    /// Takes what the canvas painter consumed from [`CanvasPicture`]. `took_edits`: the
    /// painter's diff took the scene's and the markers' reported edits, so they are cleared
    /// (a frame that did not diff, a text tab, leaves them for the next). Every painted frame
    /// is complete, so painting never asks [`Editor::wake_ms`] for another.
    pub fn painted(&mut self, took_edits: bool) {
        if took_edits {
            self.scene.clear_edits();
            self.markers.clear_edits();
        }
    }

    /// The PNG export the last command asked for, if any (one at a time).
    pub fn take_raster(&mut self) -> Option<Raster> {
        self.raster.take()
    }

    /// Writes the PNG `bytes` of `r` (or reports why there are none) and sets the status.
    ///
    /// # Panics
    ///
    /// If `r` is smaller than 1 × 1 px.
    pub fn rastered(&mut self, r: &Raster, bytes: Result<Vec<u8>, String>) {
        assert!(r.w >= 1.0 && r.h >= 1.0, "a drawable raster");
        // Web: a download (web spec §3).
        self.session.say(
            match bytes.and_then(|b| crate::platform::export(&r.path, &b)) {
                Ok(()) => format!("exported {}", r.path.display()),
                Err(e) => format!("export: {e}"),
            },
        );
    }

    /// Paints each filled plot slot of the docs page (the `graph` nodes `graphs` names) on its
    /// layer, clipped to the scroll pane it sits in (nothing else shares the page's layer, so
    /// the plot goes on top of it rather than into the reserved shape).
    pub(crate) fn paint_graphs(&self, ctx: &schemify_ui::egui::Context) {
        use schemify_ui::egui::{Rect, pos2, vec2};
        #[cfg(not(target_arch = "wasm32"))]
        for (id, expr) in &self.graphs {
            let s = &self.shell;
            let node = s.strings.find(id).and_then(|sym| s.tables.nodes.find(sym));
            let Some(row) = node.and_then(|n| s.layout.row_of(n)) else {
                continue;
            };
            let plot = self.sim.plots.iter().find(|x| x.expr == *expr).map(|x| &x.waves);
            let (Some(Ok(waves)), Some(slot)) = (plot, s.layout.slot()[row]) else {
                continue;
            };
            let (rect, clip) = (s.layout.rect()[row], s.layout.clip()[row]);
            if rect.w < 16.0 || rect.h < 16.0 {
                continue;
            }
            let clip = Rect::from_min_size(pos2(clip.x, clip.y), vec2(clip.w, clip.h));
            let painter = ctx.layer_painter(slot.layer).with_clip_rect(clip);
            schemify_ui::waves::paint_pane(&painter, rect, waves, 0, false, schemify_ui::waves::Drag::None);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
    }
}
