//! The web's no-GPU fallback (web spec §7): when the browser offers no WebGPU adapter
//! (headless Chrome in CI, browsers without WebGPU), frames are rasterized on the CPU by
//! [`CpuPainter`] (vello_cpu, the painter of the native goldens) and put on a 2D canvas laid
//! over winit's canvas. winit's canvas keeps the input: the overlay ignores pointer events.
//! A separate canvas because winit's already holds the `webgpu` context the GPU attempt made,
//! and a canvas has one context kind for life.
//!
//! Cost: one full raster per frame (no damage tracking): fine for an editor that redraws
//! on input only (D39), slow for continuous pans of large drawings.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};
use winit::platform::web::WindowExtWebSys;
use winit::window::Window;

use crate::painter::CpuPainter;
use crate::window::App;

/// The overlay canvas's element id (the browser tests find the fallback by it).
pub(crate) const ID: &str = "schemify-cpu";

/// The overlay and the painter that fills it.
pub(crate) struct CpuView {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    /// Kept across frames for its raster layers; rebuilt when the size changes.
    painter: Option<CpuPainter>,
}

impl CpuView {
    /// Lays a 2D canvas over `window`'s canvas.
    ///
    /// # Errors
    ///
    /// The window has no canvas in a document, or the browser gives no 2D context.
    pub(crate) fn new(window: &Window) -> Result<CpuView, String> {
        let under = window.canvas().ok_or("no window canvas")?;
        let doc = under.owner_document().ok_or("no document")?;
        let canvas: HtmlCanvasElement = doc
            .create_element("canvas")
            .map_err(js)?
            .dyn_into()
            .map_err(|_| "no canvas element".to_owned())?;
        canvas.set_id(ID);
        let s = canvas.style();
        s.set_property("position", "absolute").map_err(js)?;
        s.set_property("left", &format!("{}px", under.offset_left()))
            .map_err(js)?;
        s.set_property("top", &format!("{}px", under.offset_top()))
            .map_err(js)?;
        s.set_property("pointer-events", "none").map_err(js)?;
        under
            .parent_node()
            .ok_or("window canvas outside the page")?
            .append_child(&canvas)
            .map_err(js)?;
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")
            .map_err(js)?
            .ok_or("no 2d context")?
            .dyn_into()
            .map_err(|_| "no 2d context".to_owned())?;
        Ok(CpuView {
            canvas,
            ctx,
            painter: None,
        })
    }

    /// Paints one frame of `app` at `w × h` physical pixels (`scale` physical per logical)
    /// and shows it. A size past 65535 or zero draws nothing.
    pub(crate) fn present<A: App>(&mut self, app: &mut A, w: u32, h: u32, scale: f64) {
        let (Ok(w16), Ok(h16)) = (u16::try_from(w), u16::try_from(h)) else {
            return;
        };
        if w16 == 0 || h16 == 0 {
            return;
        }
        if self.painter.is_none() || self.canvas.width() != w || self.canvas.height() != h {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            let s = self.canvas.style();
            let _ = s.set_property("width", &format!("{}px", f64::from(w) / scale));
            let _ = s.set_property("height", &format!("{}px", f64::from(h) / scale));
            self.painter = Some(CpuPainter::new(w16, h16, scale));
        }
        let p = self.painter.as_mut().expect("made above");
        p.reset();
        let viewport = crate::window::logical(w, h, scale);
        app.paint(p, viewport);
        let pixels = p.finish(app.background());
        assert_eq!((pixels.width(), pixels.height()), (w16, h16));
        // Premultiplied RGBA: equal to the straight RGBA `ImageData` wants because every
        // frame starts from the app's opaque background.
        let shown =
            ImageData::new_with_u8_clamped_array_and_sh(Clamped(pixels.data_as_u8_slice()), w, h)
                .and_then(|d| self.ctx.put_image_data(&d, 0.0, 0.0));
        if let Err(e) = shown {
            web_sys::console::error_1(&e);
        }
    }
}

fn js(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
