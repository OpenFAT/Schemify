//! The web's no-GPU fallback (web spec §7): when the browser offers no WebGPU adapter
//! (browsers without WebGPU), loses the device, or presents frames that never reach the page
//! ([`shows_frames`]; headless Chrome in CI), frames are rasterized on the CPU by
//! [`CpuPainter`] (vello_cpu, the painter of the native goldens) and put on a 2D canvas laid
//! over winit's canvas. winit's canvas keeps the input: the overlay ignores pointer events.
//! A separate canvas because winit's already holds the `webgpu` context the GPU attempt made,
//! and a canvas has one context kind for life.
//!
//! Cost: one full raster per frame (no damage tracking): fine for an editor that redraws
//! on input only (D39), slow for continuous pans of large drawings.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use wasm_bindgen_futures::js_sys;
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

/// How long after the first presented WebGPU frame [`shows_frames`] looks at the page: the
/// browser composites the frame at its next rendering update (~16 ms); 250 ms leaves slack
/// for a loaded CI runner.
const PROBE_MS: i32 = 250;

/// Whether `canvas`, a WebGPU canvas that has presented a frame, shows it to the page.
/// Some browsers take every WebGPU call and composite nothing: headless Chrome on
/// SwiftShader (no error at all) or on Vulkan (the swap-chain image fails to allocate).
/// The page would stay blank, so the caller falls back to [`CpuView`] when this is false.
///
/// Waits [`PROBE_MS`], then reads one pixel at the centre back through a 2D canvas (the
/// WebGPU spec's canvas readback returns the last presented frame). Every frame starts
/// from the app's opaque background, so a shown frame has alpha 255 and a missing one 0.
/// A readback that fails (no document, no 2D context) counts as shown: the probe only
/// switches on evidence.
pub(crate) async fn shows_frames(canvas: HtmlCanvasElement) -> bool {
    let (w, h) = (canvas.width(), canvas.height());
    if w == 0 || h == 0 {
        return true;
    }
    let wait = js_sys::Promise::new(&mut |done, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&done, PROBE_MS);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(wait).await;
    let alpha = || -> Result<u8, JsValue> {
        let doc = canvas.owner_document().ok_or(JsValue::NULL)?;
        let probe: HtmlCanvasElement = doc.create_element("canvas")?.dyn_into()?;
        probe.set_width(1);
        probe.set_height(1);
        let ctx: CanvasRenderingContext2d =
            probe.get_context("2d")?.ok_or(JsValue::NULL)?.dyn_into()?;
        let (x, y) = (f64::from(w / 2), f64::from(h / 2));
        ctx.draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
            &canvas, x, y, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0,
        )?;
        let px = ctx.get_image_data(0.0, 0.0, 1.0, 1.0)?.data();
        assert_eq!(px.len(), 4, "one RGBA pixel");
        Ok(px[3])
    };
    !matches!(alpha(), Ok(0))
}

fn js(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
