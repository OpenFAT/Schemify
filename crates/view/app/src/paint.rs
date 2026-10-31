//! `paint`: the humble view's last step (architecture.md, frame spine step 12; D5, D39). The
//! editor's egui pass draws the UI; this module fills its canvas slot from the canvas tables
//! (`crate::canvas`: GPU rows plus egui labels) and serves PNG exports, which draw the canvas
//! offscreen through the same pipelines.
//!
//! Owner: `schemify-app`. Writes only [`Caches`] and the canvas rows it is handed. Reports
//! back only through `Editor::painted` (whether the canvas diff took the scene's edits, via
//! the canvas hook) and `Editor::rastered` (a PNG export's bytes).
//!
//! Contents:
//! - `pub struct Caches`
//! - `pub fn frame(editor, caches, input, now_ms, rows) -> Frame`
//! - `pub fn serve_raster(editor, caches)`

use schemify_canvas::kurbo::Rect as KRect;
use schemify_canvas::{CanvasPrims, Marks};
use schemify_editor::editor::{CanvasPicture, Editor, Frame, Raster};
use schemify_render::egui;
use schemify_render::gpu::InstanceRows;

use crate::canvas::CanvasCache;

/// What painting keeps between frames: the window canvas's diff and label indexes, and the
/// offscreen GPU a PNG export opens on first use.
#[derive(Default)]
pub struct Caches {
    canvas: CanvasCache,
    raster: Option<schemify_render::Raster>,
}

impl std::fmt::Debug for Caches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caches")
            .field("raster", &self.raster.is_some())
            .finish_non_exhaustive()
    }
}

/// One window frame: the editor's frame over `input` with its canvas packed into `rows` (the
/// window's GPU rows) and drawn as stages and labels in the canvas slot.
pub fn frame(
    editor: &mut Editor,
    caches: &mut Caches,
    input: egui::RawInput,
    now_ms: u64,
    rows: &mut InstanceRows,
) -> Frame {
    rows.frame = None; // a frame without a canvas (a text tab) draws none
    let canvas = &mut caches.canvas;
    editor.update(input, now_ms, &mut |ctx, picture| {
        canvas.paint(ctx, picture, rows)
    })
}

/// Serves the PNG export the last frame asked for, if any: the scene and decor through the
/// export's camera, drawn offscreen and encoded.
pub fn serve_raster(editor: &mut Editor, caches: &mut Caches) {
    if let Some(raster) = editor.take_raster() {
        let bytes = raster_png(editor, caches, &raster);
        editor.rastered(&raster, bytes);
    }
}

// ---- private ----

fn raster_png(editor: &Editor, caches: &mut Caches, raster: &Raster) -> Result<Vec<u8>, String> {
    assert!(raster.scale > 0.0, "raster scale {}", raster.scale);
    let side = 1.0..=f64::from(u16::MAX);
    let (w_px, h_px) = (
        (raster.w * raster.scale).round(),
        (raster.h * raster.scale).round(),
    );
    assert!(
        side.contains(&w_px) && side.contains(&h_px),
        "raster size {w_px} × {h_px}"
    );
    if caches.raster.is_none() {
        // ponytail: the web has no blocking GPU open; a web PNG export would draw on the
        // window's device instead.
        #[cfg(target_arch = "wasm32")]
        return Err("PNG export needs the desktop build".to_owned());
        #[cfg(not(target_arch = "wasm32"))]
        {
            caches.raster = Some(schemify_render::Raster::new()?);
        }
    }
    let gpu = caches.raster.as_mut().expect("opened above");
    let picture = editor
        .canvas_picture()
        .ok_or("a text tab shows no canvas")?;
    let (empty, no_marks) = (CanvasPrims::default(), Marks::default());
    // The drawing as exported: scene and decor through the export's view, nothing transient.
    let picture = CanvasPicture {
        view: KRect::new(0.0, 0.0, raster.w, raster.h),
        cam: raster.cam,
        markers: &empty,
        markers_edits: None,
        extra: &empty,
        marks: &no_marks,
        overlay: &empty,
        scene_edits: None,
        ..picture
    };
    let ctx = schemify_ui::context();
    ctx.set_pixels_per_point(raster.scale as f32);
    let size = egui::vec2(raster.w as f32, raster.h as f32);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..egui::RawInput::default()
    };
    let rows = gpu.canvas();
    *rows = InstanceRows::default();
    let mut canvas = CanvasCache::default();
    let out = ctx.run_ui(input, |ui| {
        let (shape, _) = canvas.paint(ui.ctx(), &picture, rows);
        ui.painter().add(shape);
    });
    let image = gpu.render(&ctx, out, (w_px as u32, h_px as u32));
    image.png()
}
