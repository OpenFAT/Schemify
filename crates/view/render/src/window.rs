//! The window: winit's event loop, egui-winit turning window events into egui input (and
//! egui's platform output into cursor, clipboard, IME and accessibility), egui-wgpu drawing
//! each frame's egui output with the canvas stages among it, straight into the surface.
//!
//! Event-driven (D39): input asks for a redraw, the frame runs when it is granted; a tick runs
//! when the app's [`App::wake_ms`] deadline passes or a worker's [`Waker`] posts; egui's own
//! repaint requests (an animation, a second layout pass) are honoured. With nothing pending
//! the loop waits: idle CPU is 0%.
//!
//! Contents: `pub trait App`, `pub struct Waker` (`wake`), `pub enum RunError`,
//! `pub fn run`, `pub fn block_on` (native)

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::device;
use crate::gpu::{CanvasGpu, InstanceRows};

/// What the application provides to [`run`]. Static dispatch: `run` is generic over it.
pub trait App {
    /// The egui context every frame runs in (fonts, memory); the window feeds it input.
    fn ctx(&self) -> egui::Context;
    /// One frame: egui's input in, its output out. The app packs the canvas into `canvas`
    /// (setting its frame, or clearing it when no canvas shows) while egui runs.
    fn frame(&mut self, input: egui::RawInput, canvas: &mut InstanceRows) -> egui::FullOutput;
    /// Milliseconds until the app wants a frame without input (it polls background work
    /// then), or `None` to sleep until the next input (D39 idle CPU 0%).
    fn wake_ms(&self) -> Option<u32> {
        None
    }
    /// Whether the app is done (its quit command ran); asked after every frame.
    fn exit_requested(&self) -> bool {
        false
    }
    /// The window is closing: settle what must be saved.
    fn close(&mut self) {}
    /// Takes the [`Waker`] background threads use to ask for a frame (called once).
    fn set_waker(&mut self, _waker: Waker) {}
}

/// Asks the window for a frame from any thread (a worker finished, a socket has a request).
#[derive(Clone, Debug)]
pub struct Waker(EventLoopProxy<()>);

/// Why [`run`] failed.
#[derive(Debug)]
pub enum RunError {
    /// The windowing system could not start an event loop.
    EventLoop(String),
    /// Neither a hardware adapter nor wgpu's software one could draw the window.
    NoGpu(String),
}

/// Opens a window titled `title` and runs `app` in it until it closes or asks to exit.
///
/// # Errors
///
/// No event loop, or no GPU to draw with.
pub fn run<A: App + 'static>(title: &str, mut app: A) -> Result<(), RunError> {
    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .map_err(|e| RunError::EventLoop(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    app.set_waker(Waker(event_loop.create_proxy()));
    let mut shell = Shell {
        app,
        title: title.to_owned(),
        open: None,
        failed: None,
    };
    event_loop.run_app(&mut shell).map_err(|e| RunError::EventLoop(e.to_string()))?;
    match shell.failed {
        Some(why) => Err(RunError::NoGpu(why)),
        None => Ok(()),
    }
}

/// Runs a future to completion on this thread (wgpu's adapter and device requests).
#[cfg(not(target_arch = "wasm32"))]
pub fn block_on<F: Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Wake};
    use std::thread::{self, Thread};
    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    // Bounded by the future: wgpu's native requests resolve on their first polls.
    loop {
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => thread::park(),
        }
    }
}

impl Waker {
    /// Asks for a frame; a no-op once the window is gone.
    pub fn wake(&self) {
        // Err only once the event loop is gone, when no frame is wanted anyway.
        let _ = self.0.send_event(());
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::EventLoop(e) => write!(f, "event loop: {e}"),
            RunError::NoGpu(e) => write!(f, "no GPU: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

// ---- private ----

struct Shell<A: App> {
    app: A,
    title: String,
    open: Option<Open>,
    failed: Option<String>,
}

/// A window with its GPU, egui-winit state and egui-wgpu renderer.
struct Open {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    state: egui_winit::State,
    /// When egui asked for its next frame (an animation, a tooltip delay).
    repaint_at: Option<Instant>,
}

impl<A: App> ApplicationHandler for Shell<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.open.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => return self.fail(event_loop, e.to_string()),
        };
        match open(window, self.app.ctx()) {
            Ok(o) => {
                o.window.request_redraw();
                self.open = Some(o);
            }
            Err(e) => self.fail(event_loop, e),
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(o)) = (cause, &self.open) {
            o.window.request_redraw(); // a deadline: the app's tick or egui's repaint
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
        if let Some(o) = &self.open {
            o.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(o) = self.open.as_mut() else { return };
        let response = o.state.on_window_event(&o.window, &event);
        match event {
            WindowEvent::CloseRequested => {
                self.app.close();
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                o.config.width = size.width.max(1);
                o.config.height = size.height.max(1);
                o.surface.configure(&o.device, &o.config);
                o.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                o.frame(&mut self.app);
                if self.app.exit_requested() {
                    event_loop.exit();
                    return;
                }
            }
            _ => {}
        }
        if response.repaint {
            o.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let app = self.app.wake_ms().map(|ms| Instant::now() + Duration::from_millis(u64::from(ms)));
        let egui = self.open.as_ref().and_then(|o| o.repaint_at);
        event_loop.set_control_flow(match app.into_iter().chain(egui).min() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

impl<A: App> Shell<A> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, why: String) {
        assert!(self.failed.is_none(), "the loop exits on the first failure");
        self.app.close();
        self.failed = Some(why);
        event_loop.exit();
    }
}

/// The window's GPU: a surface, the first adapter of the ladder that draws it, egui-wgpu's
/// renderer with the canvas in its callback resources, and egui-winit's state.
fn open(window: Arc<Window>, ctx: egui::Context) -> Result<Open, String> {
    let size = window.inner_size();
    let (w, h) = (size.width.max(1), size.height.max(1));
    let (surface, g) = device::ladder(|fallback| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let target: wgpu::SurfaceTarget<'static> = Arc::clone(&window).into();
        let surface = instance.create_surface(target).map_err(|e| e.to_string())?;
        let g = block_on(device::open(&instance, Some(&surface), fallback))?;
        Ok((surface, g))
    })?;
    let info = g.adapter.get_info();
    eprintln!("gpu: {} ({:?}, {:?})", info.name, info.device_type, info.backend);
    let caps = surface.get_capabilities(&g.adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| matches!(f, wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm))
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
    surface.configure(&g.device, &config);
    let mut renderer = Renderer::new(&g.device, format, RendererOptions::default());
    renderer.callback_resources.insert(CanvasGpu::new(&g.device, format));
    let max_side = g.device.limits().max_texture_dimension_2d as usize;
    let state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        None,
        Some(max_side),
    );
    ctx.set_pixels_per_point(window.scale_factor() as f32);
    Ok(Open {
        window,
        surface,
        device: g.device,
        queue: g.queue,
        config,
        renderer,
        state,
        repaint_at: None,
    })
}

impl Open {
    /// One frame: the app's egui pass over the queued input, its platform output handed to
    /// the window, its shapes tessellated and drawn with the canvas stages, presented.
    fn frame<A: App>(&mut self, app: &mut A) {
        let input = self.state.take_egui_input(&self.window);
        let canvas = self.renderer.callback_resources.get_mut::<CanvasGpu>().expect("the canvas is a resource");
        let mut out = app.frame(input, &mut canvas.rows);
        self.state.handle_platform_output(&self.window, std::mem::take(&mut out.platform_output));
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        self.repaint_at = (delay != Duration::MAX).then(|| Instant::now() + delay);
        let ppp = out.pixels_per_point;
        let ctx = app.ctx();
        let prims = ctx.tessellate(std::mem::take(&mut out.shapes), ppp);
        let screen = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: ppp,
        };
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                self.renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let canvas = self.renderer.callback_resources.get_mut::<CanvasGpu>().expect("the canvas is a resource");
        canvas.prepare(&self.device, &self.queue, &screen);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                out.textures_delta.clear();
                return;
            }
            _ => {
                out.textures_delta.clear();
                return;
            }
        };
        let target = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let mut cmds = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, &prims, &screen);
        draw(&self.renderer, &mut encoder, &target, &prims, &screen);
        cmds.push(encoder.finish());
        self.queue.submit(cmds);
        self.window.pre_present_notify();
        self.queue.present(frame);
        for id in &out.textures_delta.free {
            self.renderer.free_texture(id);
        }
        out.textures_delta.clear();
    }
}

/// One render pass into `target`, cleared black: egui's primitives with the canvas stages
/// among them.
pub(crate) fn draw(
    renderer: &Renderer,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    prims: &[egui::ClippedPrimitive],
    screen: &ScreenDescriptor,
) {
    let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("egui"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    renderer.render(&mut pass.forget_lifetime(), prims, screen);
}
