//! The window's GPU: a window in, a surface, a device and the frame's [`Pipelines`] out, and
//! each frame drawn and presented through them. Natively the fastest hardware adapter, else
//! wgpu's software one ([`gpu_ladder`]); on the web WebGPU, then WebGL2, then the CPU overlay
//! (`cpu_view`, web spec §7).
//!
//! Contents:
//! - `pub(super) struct Gpu`
//! - `Shell::open_gpu`, `Shell::fail`, `Shell::start_gpu`, `Shell::render` (`pub(super)`)
//! - `pub(super) fn gpu_ladder<T>(open: impl FnMut(bool) -> Result<T, String>) -> Result<T, String>`
//! - web: `pub(super) const TIERS`

use std::sync::Arc;

use kurbo::Affine;
#[cfg(not(target_arch = "wasm32"))]
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use super::{App, Shell, UserEvent, logical};
#[cfg(not(target_arch = "wasm32"))]
use super::block_on;
use crate::batch::GpuPainter;
use crate::gpu::Pipelines;

/// The window's GPU: its surface, the device that presents to it, the frame's pipelines.
pub(super) struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The canvas and batch pipelines, row buffers and atlas texture (`render::gpu`).
    pipes: Pipelines,
    /// Web, a tier after the first: the canvas laid over winit's that this GPU presents to
    /// (winit's holds the first tier's context for life); leaves the page with the GPU.
    #[cfg(target_arch = "wasm32")]
    overlay: Option<web_sys::HtmlCanvasElement>,
}

/// Web: the GPU tiers in the order they are tried (web spec §7), each drawing with the same
/// pipelines; past the last, frames are rasterized on the CPU (`cpu_view`).
#[cfg(target_arch = "wasm32")]
pub(super) const TIERS: [(wgpu::Backends, &str); 2] = [
    (wgpu::Backends::BROWSER_WEBGPU, "WebGPU"),
    (wgpu::Backends::GL, "WebGL2"),
];

/// Web: the element id of the WebGL2 tier's canvas (the browser tests find the tier by it).
#[cfg(target_arch = "wasm32")]
const GL_ID: &str = "schemify-webgl";

#[cfg(target_arch = "wasm32")]
impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(c) = self.overlay.take() {
            c.remove();
        }
    }
}

impl<A: App> Shell<A> {
    /// Native: a GPU for `window` down the ladder ([`gpu_ladder`]), its device's loss reported
    /// as `NoGpu`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn open_gpu(&self, window: &Arc<Window>) -> Result<Gpu, String> {
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let g = gpu_ladder(|fallback| {
            let desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
            let target = window.clone().into();
            block_on(Gpu::new(wgpu::Instance::new(desc), target, w, h, fallback))
        })?;
        let lost = self.proxy.clone();
        // `Destroyed` is our own drop of the device (the window closing, a reopen).
        g.device.set_device_lost_callback(move |reason, msg| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                let _ = lost.send_event(UserEvent::NoGpu(0, format!("device lost ({msg})")));
            }
        });
        Ok(g)
    }

    /// Native: no GPU can draw the window. The app settles as on a close and [`run`] returns
    /// the reason ([`RunError::NoGpu`]) instead of leaving a blank window.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn fail(&mut self, event_loop: &ActiveEventLoop, why: String) {
        self.app.close();
        self.failed = Some(why);
        event_loop.exit();
    }

    /// Web: makes tier `self.tier`'s GPU in the background: on winit's canvas for the first
    /// tier, on a canvas laid over it for a later one. Ends in `GpuReady` or `NoGpu`; a device
    /// lost later sends `NoGpu` too.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn start_gpu(&self, window: &Arc<Window>) {
        let tier = self.tier;
        assert!(tier < TIERS.len(), "a GPU tier");
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let (pending, proxy, win) = (self.pending.clone(), self.proxy.clone(), window.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let made = async {
                let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
                desc.backends = TIERS[tier].0;
                let instance = wgpu::Instance::new(desc);
                if tier == 0 {
                    return Gpu::new(instance, win.into(), w, h, false).await;
                }
                let canvas = super::cpu_view::overlay(&win, GL_ID)?;
                // The context made first wins, so wgpu's later request gets this one: wgpu's
                // own options plus a kept drawing buffer, which the probe (`shows_frames`)
                // reads after the frame was composited; without it the read sees a cleared
                // buffer and every WebGL2 frame would count as missing.
                let opts = wasm_bindgen_futures::js_sys::Object::new();
                for key in ["preserveDrawingBuffer", "antialias"] {
                    let on = key == "preserveDrawingBuffer";
                    let _ =
                        wasm_bindgen_futures::js_sys::Reflect::set(&opts, &key.into(), &on.into());
                }
                let _ = canvas.get_context_with_context_options("webgl2", &opts);
                let target = wgpu::SurfaceTarget::Canvas(canvas.clone());
                let made = Gpu::new(instance, target, w, h, false).await;
                if made.is_err() {
                    canvas.remove();
                }
                made.map(|mut g| {
                    g.overlay = Some(canvas);
                    g
                })
            };
            match made.await {
                Ok(g) => {
                    let lost = proxy.clone();
                    // `Destroyed` is our own drop of the device after a fallback.
                    g.device.set_device_lost_callback(move |reason, msg| {
                        if reason != wgpu::DeviceLostReason::Destroyed {
                            let why = format!("device lost ({msg})");
                            let _ = lost.send_event(UserEvent::NoGpu(tier, why));
                        }
                    });
                    *pending.borrow_mut() = Some((tier, g));
                    let _ = proxy.send_event(UserEvent::GpuReady);
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::NoGpu(tier, e));
                }
            }
        });
    }
    pub(super) fn render(&mut self) {
        // The granted redraw answers every input queued until now (`pace`). The frame is
        // presented even when that input changed nothing (a modifier press): winit's own
        // redraws (an expose, a resize) cannot be told from the ones asked for input.
        if !self.events.is_empty() {
            self.update();
        }
        debug_assert!(
            self.events.is_empty(),
            "the frame answers every queued event"
        );
        #[cfg(target_arch = "wasm32")]
        if self.gpu.is_none() {
            if let (Some(w), Some(cpu)) = (&self.window, &mut self.cpu) {
                let size = w.inner_size();
                cpu.present(&mut self.app, size.width, size.height, self.scale);
            }
            return;
        }
        let (Some(window), Some(gpu)) = (&self.window, &mut self.gpu) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if (gpu.config.width, gpu.config.height) != (size.width, size.height) {
            gpu.resize(size.width, size.height);
        }
        let viewport = logical(size.width, size.height, self.scale);
        self.batch.begin_frame();
        let mut painter = GpuPainter {
            batch: &mut self.batch,
            base: Affine::scale(self.scale),
            rows: Some(&mut self.rows),
        };
        self.app.paint(&mut painter, viewport);
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                window.request_redraw();
                return;
            }
            _ => return,
        };
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // One pass straight into the surface: the batch in painting order, the canvas rows at
        // their stages in it, after the rows, instances and masks the frame wrote are uploaded.
        gpu.pipes.upload(&gpu.device, &gpu.queue, &mut self.rows);
        gpu.pipes
            .upload_batch(&gpu.device, &gpu.queue, &mut self.batch);
        let bg = self.app.background().components.map(f64::from);
        let clear = wgpu::Color {
            r: bg[0],
            g: bg[1],
            b: bg[2],
            a: bg[3],
        };
        let canvas = self.rows.frame.take();
        gpu.pipes.draw(
            &mut encoder,
            &gpu.queue,
            &target,
            clear,
            &self.rows,
            &self.batch,
            canvas.as_ref(),
            self.scale,
            (size.width, size.height),
        );
        gpu.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        gpu.queue.present(frame);
        // Glyphs drawn from stand-ins (another size, scaled; a zoom): one more frame, with no
        // input, rasterizes more of the exact masks, until the picture settles (D39 idle 0%).
        if !self.batch.settled() {
            window.request_redraw();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let now = std::time::Instant::now();
            if self.frame_log
                && let Some((oldest, newest)) = self.shown
            {
                let ms = |t: std::time::Instant| (now - t).as_secs_f64() * 1e3;
                let (atlas, instances) = gpu.pipes.take_batch_uploaded();
                eprintln!(
                    "frame: input to present {:.2} ms oldest, {:.2} ms newest, {} updates, period {:.2} ms, atlas {} B, instances {} B",
                    ms(oldest),
                    ms(newest),
                    self.updates,
                    self.presented.map_or(0.0, ms),
                    atlas,
                    instances
                );
            }
            (self.shown, self.updates, self.presented) = (None, 0, Some(now));
        }
        // Web: a browser can take every GPU call and still show nothing (web spec §7), so
        // each tier's first frame is looked for on the page.
        #[cfg(target_arch = "wasm32")]
        if !std::mem::replace(&mut self.probed, true) {
            use winit::platform::web::WindowExtWebSys;
            if let Some(canvas) = gpu.overlay.clone().or_else(|| window.canvas()) {
                let (proxy, tier) = (self.proxy.clone(), self.tier);
                wasm_bindgen_futures::spawn_local(async move {
                    if !super::cpu_view::shows_frames(canvas).await {
                        let why = "frames do not reach the page".to_owned();
                        let _ = proxy.send_event(UserEvent::NoGpu(tier, why));
                    }
                });
            }
        }
    }
}


/// The native GPU ladder: `open(false)` (the fastest hardware adapter), then `open(true)`
/// (wgpu's software adapter: WARP, lavapipe or llvmpipe). The error names both failures.
// ponytail: no CPU rung (`CpuPainter` through `softbuffer`): WARP and lavapipe cover the
// machines without a GPU we know of; a new dependency waits for one where both fail.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(super) fn gpu_ladder<T>(mut open: impl FnMut(bool) -> Result<T, String>) -> Result<T, String> {
    let hardware = match open(false) {
        Ok(g) => return Ok(g),
        Err(e) => e,
    };
    eprintln!("gpu: no hardware adapter ({hardware}); trying the software one");
    open(true).map_err(|software| format!("hardware: {hardware}; software: {software}"))
}

// ---- private ----

impl Gpu {
    /// Opens a surface on `target` (the window, or on the web a canvas) on the fastest adapter
    /// of `instance` that presents to it: a discrete GPU before an integrated one
    /// (`WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF` override, as in wgpu), with one frame in
    /// flight and mailbox presentation where the surface offers it (an input shows at the next
    /// vertical blank instead of two frames later; D39). The device is held to WebGL2's
    /// guaranteed limits on every backend, so a pipeline the web's WebGL2 tier could not build
    /// fails validation natively too. Logs the adapter once, so a report names the GPU that ran.
    /// `fallback` asks for wgpu's software adapter instead (WARP, lavapipe, llvmpipe), ignoring
    /// `WGPU_ADAPTER_NAME`.
    async fn new(
        instance: wgpu::Instance,
        target: wgpu::SurfaceTarget<'static>,
        w: u32,
        h: u32,
        fallback: bool,
    ) -> Result<Gpu, String> {
        assert!(w > 0 && h > 0, "a surface has a size");
        let surface = instance.create_surface(target).map_err(|e| e.to_string())?;
        let named = match fallback {
            false => wgpu::util::initialize_adapter_from_env(&instance, Some(&surface))
                .await
                .ok(),
            true => None,
        };
        let adapter = match named {
            Some(a) => a,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    force_fallback_adapter: fallback,
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await
                .map_err(|e| e.to_string())?,
        };
        let info = adapter.get_info();
        let named = format!(
            "gpu: {} ({:?}, {:?})",
            info.name, info.device_type, info.backend
        );
        #[cfg(not(target_arch = "wasm32"))]
        eprintln!("{named}");
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&named.into());
        let required_limits =
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
                )
            })
            .ok_or("no 8-bit surface format")?;
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: w,
            height: h,
            present_mode,
            color_space: wgpu::SurfaceColorSpace::Auto,
            desired_maximum_frame_latency: 1,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        Ok(Gpu {
            pipes: Pipelines::new(&device, format),
            device,
            queue,
            surface,
            config,
            #[cfg(target_arch = "wasm32")]
            overlay: None,
        })
    }

    /// Sizes the surface to `w × h` device px (both non-zero).
    fn resize(&mut self, w: u32, h: u32) {
        assert!(w > 0 && h > 0, "a surface has a size");
        (self.config.width, self.config.height) = (w, h);
        self.surface.configure(&self.device, &self.config);
    }
}

#[cfg(test)]
mod tests {
    use super::gpu_ladder;

    /// The native ladder tries the hardware adapter first and the software one only after it
    /// failed; a total failure names both reasons.
    #[test]
    fn gpu_ladder_order() {
        let mut tried = Vec::new();
        let first = gpu_ladder(|fallback| {
            tried.push(fallback);
            Ok::<_, String>(fallback)
        });
        assert_eq!(
            (first, &tried[..]),
            (Ok(false), &[false][..]),
            "hardware first, alone"
        );
        tried.clear();
        let second = gpu_ladder(|fallback| {
            tried.push(fallback);
            if fallback {
                Ok(1)
            } else {
                Err("no Vulkan".to_owned())
            }
        });
        assert_eq!(
            (second, &tried[..]),
            (Ok(1), &[false, true][..]),
            "then software"
        );
        let none = gpu_ladder::<()>(|fallback| Err(format!("{fallback}")));
        assert_eq!(
            none,
            Err("hardware: false; software: true".to_owned()),
            "both named"
        );
    }
}
