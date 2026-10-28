//! Image exports: SVG, PDF and EPS written here, PNG asked of `schemify-app` as a `Raster`.
//!
//! Contents:
//! - `Editor::export`
//! - `Editor::view_world_rect`
//! - `Editor::export_png`

use std::path::{Path, PathBuf};

use schemify_canvas::CanvasPrims;
use schemify_doc::DocHandle;

use crate::commands::Cmd;
use crate::scene::UNIT;
use crate::session::View;
use crate::{export, platform, scene};

use super::Editor;

/// Largest PNG export side in pixels (the CPU raster's target is `u16`; 16k keeps one
/// image under 1 GiB of RGBA).
const PNG_MAX_PX: u16 = 16_384;

impl Editor {
    /// The palette exports draw with: the view palette without dimming or hidden layers
    /// beyond the user's.
    fn export_look(&self) -> export::Look {
        export::Look {
            palette: self.view_palette(),
            color: self.opts.export_color,
            transparent: self.opts.export_transparent,
        }
    }
    /// Where an export goes: `arg` when given, else `.schemify/export/<cell>.<ext>`.
    fn export_path(&self, h: DocHandle, arg: &str, ext: &str) -> PathBuf {
        if !arg.trim().is_empty() {
            let p = PathBuf::from(arg.trim());
            return if p.is_absolute() {
                p
            } else {
                self.session.root.join(p)
            };
        }
        let stem = self.session.path(h).and_then(Path::file_stem).map_or_else(
            || "untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        self.session
            .root
            .join(".schemify")
            .join("export")
            .join(format!("{stem}.{ext}"))
    }
    /// File → Image export (PNG of the visible canvas or of the whole drawing, at the PNG
    /// scale; SVG, PDF and EPS of the drawing, the selection or the visible area, PDF and EPS
    /// fitted to the export page; a PDF page per schematic of the hierarchy).
    pub(super) fn export(&mut self, h: DocHandle, cmd: Cmd, arg: &str) {
        let look = self.export_look();
        let page = self.opts.export_page;
        let pad = 10 * UNIT;
        let whole = export::bounds(&self.scene.prims).map(|b| export::padded(b, pad));
        let selected = (!self.selection().is_empty()).then(|| {
            // ponytail: one flag per object, O(objects) once per export; fine for a command.
            let n = self.scene.objs.len() as u32;
            let keep: Vec<bool> = (0..n).map(|o| self.selected.contains(o)).collect();
            export::subset(&self.scene, &keep)
        });
        if matches!(cmd, Cmd::ExportPng | Cmd::ExportPngFull) {
            let path = self.export_path(h, arg, "png");
            let asked = match cmd {
                Cmd::ExportPng => self.export_png(path),
                _ => self.export_png_full(path),
            };
            if let Err(e) = asked {
                self.session.say(format!("export: {e}"));
            }
            return;
        }
        let (ext, bytes): (&str, Result<Vec<u8>, String>) = match cmd {
            Cmd::ExportSvg => (
                "svg",
                whole
                    .map(|b| export::svg(&self.scene.prims, b, &look).into_bytes())
                    .ok_or_else(|| "nothing to export".into()),
            ),
            Cmd::ExportEps => {
                let prims = selected.as_ref().unwrap_or(&self.scene.prims);
                (
                    "eps",
                    export::bounds(prims)
                        .map(|b| {
                            export::eps(prims, export::padded(b, pad), &look, page).into_bytes()
                        })
                        .ok_or_else(|| "nothing to export".into()),
                )
            }
            Cmd::ExportPdf => {
                let (prims, b) = match &selected {
                    Some(p) => (p, export::bounds(p).map(|b| export::padded(b, pad))),
                    None => (&self.scene.prims, Some(self.view_world_rect())),
                };
                (
                    "pdf",
                    b.map(|b| export::pdf(&[(prims, b)], &look, page))
                        .ok_or_else(|| "nothing to export".into()),
                )
            }
            Cmd::ExportPdfFull => (
                "pdf",
                whole
                    .map(|b| export::pdf(&[(&self.scene.prims, b)], &look, page))
                    .ok_or_else(|| "nothing to export".into()),
            ),
            _ => ("pdf", self.export_hierarchy(h, &look)),
        };
        let path = self.export_path(h, arg, ext);
        // Web: a download (web spec §3).
        match bytes.and_then(|b| platform::export(&path, &b)) {
            Ok(()) => self.session.say(format!("exported {}", path.display())),
            Err(e) => self.session.say(format!("export: {e}")),
        }
    }
    /// The visible canvas area in world milli-units.
    pub(super) fn view_world_rect(&self) -> [i32; 4] {
        let v = self.canvas_rect();
        let cam = self.camera();
        let (x0, y0) = cam.to_world(v, v.x0, v.y0);
        let (x1, y1) = cam.to_world(v, v.x1, v.y1);
        let r = |f: f64| f.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        [r(x0.min(x1)), r(y0.min(y1)), r(x0.max(x1)), r(y0.max(y1))]
    }
    /// Asks for the visible canvas as a PNG at `path` at the PNG scale
    /// (`EditOpts::png_scale`): the same grid, primitives and palette the window draws,
    /// without selection marks or overlay. At scale 1 the image has the canvas's size in
    /// logical pixels. `schemify-app` rasterizes it after the frame ([`Editor::take_raster`]).
    ///
    /// # Errors
    ///
    /// The canvas has no area, or the scaled image is over 16384 px a side.
    pub fn export_png(&mut self, path: PathBuf) -> Result<(), String> {
        let v = self.canvas_rect();
        // The camera is relative to the view's center: the image shows what the canvas shows.
        let cam = self.camera();
        self.ask_raster(path, v.width().round(), v.height().round(), cam)
    }
    /// Asks for the whole drawing (its bounds plus a 10-unit margin) as a PNG at `path`, at
    /// one logical pixel per schematic unit times the PNG scale, whatever the canvas shows.
    ///
    /// # Errors
    ///
    /// Nothing is drawn, or the image is over 16384 px a side (lower the scale).
    fn export_png_full(&mut self, path: PathBuf) -> Result<(), String> {
        let b = export::bounds(&self.scene.prims)
            .map(|b| export::padded(b, 10 * UNIT))
            .ok_or("nothing to export")?;
        let u = f64::from(UNIT);
        let (w, h) = (
            (f64::from(b[2]) - f64::from(b[0])) / u,
            (f64::from(b[3]) - f64::from(b[1])) / u,
        );
        let cam = schemify_canvas::Camera {
            cx: (f64::from(b[0]) + f64::from(b[2])) / 2.0,
            cy: (f64::from(b[1]) + f64::from(b[3])) / 2.0,
            scale: 1.0 / u,
        };
        self.ask_raster(path, w.ceil(), h.ceil(), cam)
    }
    /// Queues a `w × h` logical-pixel raster through `cam` at the PNG scale.
    fn ask_raster(
        &mut self,
        path: PathBuf,
        w: f64,
        h: f64,
        cam: schemify_canvas::Camera,
    ) -> Result<(), String> {
        let k = f64::from(self.opts.png_scale) / 100.0;
        assert!(k > 0.0 && cam.scale > 0.0, "a positive scale");
        let (pw, ph) = ((w * k).round(), (h * k).round());
        if w < 1.0 || h < 1.0 || pw < 1.0 || ph < 1.0 {
            return Err("the canvas has no drawable area".into());
        }
        let max = f64::from(PNG_MAX_PX);
        if pw > max || ph > max {
            return Err(format!(
                "a {pw} × {ph} px image is over {PNG_MAX_PX} px a side: lower the PNG scale \
                 (options.png_export_scale)"
            ));
        }
        self.raster = Some(super::Raster {
            path,
            w,
            h,
            cam,
            scale: k,
        });
        Ok(())
    }
    /// One PDF page per schematic from the active cell down (breadth first, each cell once):
    /// every page is that cell's scene built offscreen with the same symbol cache.
    fn export_hierarchy(&mut self, h: DocHandle, look: &export::Look) -> Result<Vec<u8>, String> {
        let docs = self.hierarchy_docs(h);
        let mut scenes: Vec<scene::Scene> = Vec::with_capacity(docs.len());
        for &d in &docs {
            self.refresh_symbols_of(d);
            let mut sc = scene::Scene::default();
            let epoch = self.session.epoch(d);
            scene::build(
                &mut sc,
                &self.session.project,
                d,
                View::Schematic,
                &self.symbols,
                epoch,
                None,
            );
            scenes.push(sc);
        }
        let pages: Vec<(&CanvasPrims, [i32; 4])> = scenes
            .iter()
            .filter_map(|sc| {
                export::bounds(&sc.prims).map(|b| (&sc.prims, export::padded(b, 10 * UNIT)))
            })
            .collect();
        if pages.is_empty() {
            return Err("nothing to export".into());
        }
        Ok(export::pdf(&pages, look, self.opts.export_page))
    }
}
