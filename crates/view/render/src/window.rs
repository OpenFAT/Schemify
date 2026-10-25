//! Window, event loop and GPU surface (winit + wgpu), native and web from one code
//! path. Redraw is event-driven: nothing runs while idle (D39). Input is paced by the
//! compositor ([`pace`]): an event asks for a redraw, and [`App::update`] runs when the
//! `RedrawRequested` is granted (on Wayland, after the previous frame's callback), once over
//! every event queued until then, so a frame shows the newest input and one update serves
//! one frame. An app with background work (a simulation, a child process, a plugin timer)
//! asks for a tick through [`App::wake_ms`]; a tick updates at once, since a hidden window
//! is granted no redraw and the run it polls must go on. The loop then sleeps until that
//! deadline or the next input, never busy-polls.

use std::sync::Arc;

use accesskit::{Action, TreeUpdate};
use kurbo::Affine;
use peniko::Color;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use schemify_ui::{Button, Chord, InputEvent, InputEvents, Key, Mods};

use crate::batch::{Batch, GpuPainter};
use crate::gpu::{InstanceRows, Pipelines};
use crate::painter::Painter;

/// What the application provides to [`run`]. Static dispatch: `run` is generic over it.
pub trait App {
    /// Consumes one batch of input (possibly empty: a tick, a resize) for a viewport in
    /// logical pixels; returns true when the picture changed and a frame must be drawn. Called
    /// once per granted redraw with every event queued until then, and once per tick.
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool;
    /// Encodes one frame.
    fn paint<P: Painter>(&mut self, p: &mut P, viewport: (f32, f32));
    /// Full accessibility tree for the current frame.
    fn access(&mut self) -> TreeUpdate;
    /// Milliseconds until the app wants [`App::update`] called again without input (it
    /// polls background work then), or `None` to sleep until the next input. Asked after
    /// every update; `None` whenever nothing runs keeps idle CPU at 0% (D39).
    fn wake_ms(&self) -> Option<u32> {
        None
    }
    /// Color under everything painted.
    fn background(&self) -> Color {
        Color::BLACK
    }
    /// Whether the app is done (its quit command ran). Asked after every batch of updates;
    /// true exits the event loop, so [`run`] returns and drops the app (its `Drop` runs, which
    /// a `process::exit` inside `update` would skip).
    fn exit_requested(&self) -> bool {
        false
    }
    /// Where the platform should put an IME candidate window: `(x, y, w, h)` in logical px
    /// around the text being composed (the focused field's caret). Asked after every update
    /// that changed the picture; `None` leaves the window where it was.
    fn ime_area(&self) -> Option<(f32, f32, f32, f32)> {
        None
    }
    /// Takes the [`Waker`] that background threads use to ask for a frame. Called once by
    /// [`run`] before the first update; an app with no threads ignores it.
    fn set_waker(&mut self, _waker: Waker) {}
    /// The window's close button was pressed; the event loop exits right after. The app
    /// finishes what its quit command would (a close leaves the same files a quit does).
    fn close(&mut self) {}
}

/// Asks the window for one [`App::update`] with no input, from any thread: a worker that
/// posts work wakes the loop instead of the loop polling for it (idle CPU 0%, D39).
/// Cheap to clone; waking a closed window does nothing.
#[derive(Clone)]
pub struct Waker(EventLoopProxy<UserEvent>);

impl Waker {
    /// Schedules a tick. Wakes coalesce: several before the next frame give one update.
    pub fn wake(&self) {
        // Err only once the event loop is gone, when no frame is wanted anyway.
        let _ = self.0.send_event(UserEvent::Wake);
    }
}

/// Why [`run`] failed.
#[derive(Debug)]
pub enum RunError {
    /// The windowing system could not start an event loop.
    EventLoop(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::EventLoop(e) => write!(f, "event loop: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

enum UserEvent {
    Access(accesskit_winit::Event),
    /// [`Waker::wake`]: update with no input.
    Wake,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    GpuReady,
    /// Web: WebGPU is unusable (no adapter or device, the device lost, or presented frames
    /// that never reach the page), for the reason given; frames go to the CPU overlay
    /// (`cpu_view`) from now on. The first one is logged; later ones are ignored.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    NoGpu(String),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::Access(e)
    }
}

/// The window's GPU: its surface, the device that presents to it, the frame's pipelines.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The canvas and batch pipelines, row buffers and atlas texture (`render::gpu`).
    pipes: Pipelines,
}

impl Gpu {
    /// Opens the window's surface on the fastest adapter that presents to it: a discrete GPU
    /// before an integrated one (`WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF` override, as in
    /// wgpu), with one frame in flight and mailbox presentation where the surface offers it
    /// (an input shows at the next vertical blank instead of two frames later; D39). Logs the
    /// adapter once, so a report names the GPU that ran.
    async fn new(window: Arc<Window>, w: u32, h: u32) -> Result<Gpu, String> {
        assert!(w > 0 && h > 0, "a surface has a size");
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
        let named = wgpu::util::initialize_adapter_from_env(&instance, Some(&surface)).await;
        let adapter = match named {
            Ok(a) => a,
            Err(_) => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await
                .map_err(|e| e.to_string())?,
        };
        let info = adapter.get_info();
        eprintln!(
            "gpu: {} ({:?}, {:?})",
            info.name, info.device_type, info.backend
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
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
        })
    }

    /// Sizes the surface to `w × h` device px (both non-zero).
    fn resize(&mut self, w: u32, h: u32) {
        assert!(w > 0 && h > 0, "a surface has a size");
        (self.config.width, self.config.height) = (w, h);
        self.surface.configure(&self.device, &self.config);
    }
}

struct Shell<A: App> {
    app: A,
    title: String,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    #[cfg(target_arch = "wasm32")]
    pending: std::rc::Rc<std::cell::RefCell<Option<Gpu>>>,
    /// Web without a GPU: the CPU-rastered overlay (web spec §7).
    #[cfg(target_arch = "wasm32")]
    cpu: Option<crate::cpu_view::CpuView>,
    /// Web: the first GPU frame was presented and `cpu_view::shows_frames` checks it.
    #[cfg(target_arch = "wasm32")]
    probed: bool,
    adapter: Option<accesskit_winit::Adapter>,
    events: InputEvents,
    mods: u8,
    pointer: (f32, f32),
    scale: f64,
    /// The frame's batch and the coverage atlas behind it (kept across frames).
    batch: Batch,
    /// The canvas's instance rows (kept across frames; `Painter::canvas` patches them).
    rows: InstanceRows,
    access_dirty: bool,
    /// The deadline [`App::wake_ms`] asked for has passed: update with no input.
    tick: bool,
    /// The IME area last given to the window ([`App::ime_area`]).
    ime: Option<(f32, f32, f32, f32)>,
    /// `SCHEMIFY_FRAME_LOG=1`: log each frame's input-to-present times on stderr.
    #[cfg(not(target_arch = "wasm32"))]
    frame_log: bool,
    /// When the oldest and the newest input no update has consumed yet arrived.
    #[cfg(not(target_arch = "wasm32"))]
    queued: Option<(std::time::Instant, std::time::Instant)>,
    /// The same for the input the updates since the last present consumed: what the next
    /// present shows. The oldest is the lag a log reader sees, the newest what the hand feels.
    #[cfg(not(target_arch = "wasm32"))]
    shown: Option<(std::time::Instant, std::time::Instant)>,
    /// Updates that consumed input since the last present (the frame's coalescing).
    #[cfg(not(target_arch = "wasm32"))]
    updates: u32,
    /// When the last frame was presented (the log's frame period).
    #[cfg(not(target_arch = "wasm32"))]
    presented: Option<std::time::Instant>,
}

/// Opens a window and runs `app` until it closes. Native: blocks. Web (wasm32): attaches a
/// canvas to the document body, returns immediately, and the browser drives the loop.
///
/// # Errors
///
/// Fails when no event loop can be created (no display server). Native: a GPU without
/// Vulkan/Metal/DX12 support is reported on stderr and leaves the window blank. Web: when
/// WebGPU is unusable (no adapter, the device lost, or frames that never reach the page) the
/// editor draws on the CPU instead, logged once as `gpu: …; drawing on the CPU` (web spec §7).
pub fn run<A: App + 'static>(title: &str, mut app: A) -> Result<(), RunError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| RunError::EventLoop(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    app.set_waker(Waker(event_loop.create_proxy()));
    let shell = Shell {
        app,
        title: title.to_owned(),
        proxy: event_loop.create_proxy(),
        window: None,
        gpu: None,
        #[cfg(target_arch = "wasm32")]
        pending: Default::default(),
        #[cfg(target_arch = "wasm32")]
        cpu: None,
        #[cfg(target_arch = "wasm32")]
        probed: false,
        adapter: None,
        events: InputEvents::default(),
        mods: 0,
        pointer: (0.0, 0.0),
        scale: 1.0,
        batch: Batch::default(),
        rows: InstanceRows::default(),
        access_dirty: true,
        tick: false,
        ime: None,
        #[cfg(not(target_arch = "wasm32"))]
        frame_log: std::env::var_os("SCHEMIFY_FRAME_LOG").is_some_and(|v| v != "0"),
        #[cfg(not(target_arch = "wasm32"))]
        queued: None,
        #[cfg(not(target_arch = "wasm32"))]
        shown: None,
        #[cfg(not(target_arch = "wasm32"))]
        updates: 0,
        #[cfg(not(target_arch = "wasm32"))]
        presented: None,
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut shell = shell;
        event_loop
            .run_app(&mut shell)
            .map_err(|e| RunError::EventLoop(e.to_string()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(shell);
        Ok(())
    }
}

impl<A: App> Shell<A> {
    fn viewport(&self) -> (f32, f32) {
        let size = self
            .window
            .as_ref()
            .map_or(Default::default(), |w| w.inner_size());
        logical(size.width, size.height, self.scale)
    }

    /// One [`App::update`] over the queued input (a tick when there is none); keeps the IME
    /// area in step and marks the accessibility tree stale when the picture changed. Returns
    /// whether the picture changed.
    fn update(&mut self) -> bool {
        let viewport = self.viewport();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((oldest, newest)) = self.queued.take() {
            self.shown = Some((self.shown.map_or(oldest, |s| s.0), newest));
            self.updates += 1;
        }
        let changed = self.app.update(&self.events, viewport);
        self.events.clear();
        if changed {
            self.access_dirty = true;
            let ime = self.app.ime_area();
            if let (Some(a), Some(w)) = (ime, &self.window)
                && self.ime != ime
            {
                w.set_ime_cursor_area(LogicalPosition::new(a.0, a.1), LogicalSize::new(a.2, a.3));
            }
            self.ime = ime;
        }
        changed
    }

    fn render(&mut self) {
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
        // Web: a browser can take every WebGPU call and still show nothing (web spec §7).
        #[cfg(target_arch = "wasm32")]
        if !std::mem::replace(&mut self.probed, true) {
            use winit::platform::web::WindowExtWebSys;
            if let Some(canvas) = window.canvas() {
                let proxy = self.proxy.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if !crate::cpu_view::shows_frames(canvas).await {
                        let why = "WebGPU frames do not reach the page".to_owned();
                        let _ = proxy.send_event(UserEvent::NoGpu(why));
                    }
                });
            }
        }
    }

    fn push_key(&mut self, key: &WKey, text: Option<&str>) {
        let k = match key {
            WKey::Character(s) => s.chars().next().map(|c| Key(c as u32)),
            WKey::Named(n) => named_key(*n),
            _ => None,
        };
        if let Some(k) = k {
            self.events.push(InputEvent::Key {
                chord: Chord::new(Mods(self.mods), k),
            });
        }
        if let Some(t) = text.filter(|t| t.chars().all(|c| !c.is_control())) {
            self.events.push_text(t);
        }
    }
}

/// A `w × h` physical-pixel size in logical pixels at `scale` physical per logical.
pub(crate) fn logical(w: u32, h: u32, scale: f64) -> (f32, f32) {
    debug_assert!(scale > 0.0);
    ((f64::from(w) / scale) as f32, (f64::from(h) / scale) as f32)
}

fn named_key(n: NamedKey) -> Option<Key> {
    Some(match n {
        NamedKey::Escape => Key::ESCAPE,
        NamedKey::Enter => Key::ENTER,
        NamedKey::Tab => Key::TAB,
        NamedKey::Backspace => Key::BACKSPACE,
        NamedKey::Delete => Key::DELETE,
        NamedKey::Insert => Key::INSERT,
        NamedKey::Home => Key::HOME,
        NamedKey::End => Key::END,
        NamedKey::PageUp => Key::PAGE_UP,
        NamedKey::PageDown => Key::PAGE_DOWN,
        NamedKey::ArrowLeft => Key::LEFT,
        NamedKey::ArrowRight => Key::RIGHT,
        NamedKey::ArrowUp => Key::UP,
        NamedKey::ArrowDown => Key::DOWN,
        NamedKey::PrintScreen => Key::PRINT,
        NamedKey::Space => Key(' ' as u32),
        NamedKey::F1 => Key::f(1),
        NamedKey::F2 => Key::f(2),
        NamedKey::F3 => Key::f(3),
        NamedKey::F4 => Key::f(4),
        NamedKey::F5 => Key::f(5),
        NamedKey::F6 => Key::f(6),
        NamedKey::F7 => Key::f(7),
        NamedKey::F8 => Key::f(8),
        NamedKey::F9 => Key::f(9),
        NamedKey::F10 => Key::f(10),
        NamedKey::F11 => Key::f(11),
        NamedKey::F12 => Key::f(12),
        _ => return None,
    })
}

/// What the loop does with what arrived since it last slept (D39 pacing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    /// Nothing pending: sleep until input or the next deadline.
    Sleep,
    /// Input only: ask for a redraw and update when it is granted, so one update answers
    /// every event queued until the compositor lets the frame be drawn.
    Redraw,
    /// A tick is due: update now (queued input rides along); a changed picture then asks for
    /// a redraw.
    Update,
}

/// The pacing rule: a due tick updates at once, input alone waits for the granted redraw,
/// nothing pending sleeps.
fn pace(tick: bool, input: bool) -> Pace {
    match (tick, input) {
        (true, _) => Pace::Update,
        (false, true) => Pace::Redraw,
        (false, false) => Pace::Sleep,
    }
}

fn mods_of(m: ModifiersState) -> u8 {
    let mut out = 0;
    if m.control_key() {
        out |= Mods::CTRL;
    }
    if m.shift_key() {
        out |= Mods::SHIFT;
    }
    if m.alt_key() {
        out |= Mods::ALT;
    }
    if m.super_key() {
        out |= Mods::SUPER;
    }
    out
}

impl<A: App> ApplicationHandler<UserEvent> for Shell<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_visible(false);
        // Natively a 1280×800 window; on the web the page's CSS sizes the canvas to the whole
        // viewport (web/index.html) and winit reports it, and every browser resize, as
        // `Resized` (a fixed size left the editor in a corner of a larger window).
        #[cfg(not(target_arch = "wasm32"))]
        let attrs = attrs.with_inner_size(LogicalSize::new(1280.0, 800.0));
        #[cfg(target_arch = "wasm32")]
        let attrs = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs.with_append(true)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("window: {e}");
                event_loop.exit();
                return;
            }
        };
        // AccessKit must attach before the window is first shown.
        self.adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.proxy.clone(),
        ));
        window.set_visible(true);
        window.set_ime_allowed(true);
        self.scale = window.scale_factor();
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        #[cfg(not(target_arch = "wasm32"))]
        match pollster::block_on(Gpu::new(window.clone(), w, h)) {
            Ok(g) => self.gpu = Some(g),
            Err(e) => eprintln!("gpu: {e}"),
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (pending, proxy, win) = (self.pending.clone(), self.proxy.clone(), window.clone());
            wasm_bindgen_futures::spawn_local(async move {
                match Gpu::new(win, w, h).await {
                    Ok(g) => {
                        let lost = proxy.clone();
                        // `Destroyed` is our own drop of the device after a fallback.
                        g.device.set_device_lost_callback(move |reason, msg| {
                            if reason != wgpu::DeviceLostReason::Destroyed {
                                let why = format!("device lost ({msg})");
                                let _ = lost.send_event(UserEvent::NoGpu(why));
                            }
                        });
                        *pending.borrow_mut() = Some(g);
                        let _ = proxy.send_event(UserEvent::GpuReady);
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::NoGpu(e));
                    }
                }
            });
        }
        self.window = Some(window);
        self.update();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Access(e) => match e.window_event {
                accesskit_winit::WindowEvent::InitialTreeRequested => self.access_dirty = true,
                accesskit_winit::WindowEvent::ActionRequested(req)
                    if req.action == Action::Click =>
                {
                    self.events.push(InputEvent::Activate {
                        node: schemify_ui::access::sym_of(req.target_node),
                    });
                }
                _ => {}
            },
            UserEvent::Wake => self.tick = true,
            UserEvent::GpuReady => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.gpu = self.pending.borrow_mut().take();
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
            UserEvent::NoGpu(why) => {
                #[cfg(target_arch = "wasm32")]
                if let (Some(w), None) = (&self.window, &self.cpu) {
                    // Once per page: the console line the smoke test looks for (web spec §7).
                    web_sys::console::error_1(&format!("gpu: {why}; drawing on the CPU").into());
                    self.gpu = None;
                    self.pending.borrow_mut().take();
                    match crate::cpu_view::CpuView::new(w) {
                        Ok(v) => self.cpu = Some(v),
                        Err(e) => web_sys::console::error_1(&format!("cpu view: {e}").into()),
                    }
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let (Some(a), Some(w)) = (&mut self.adapter, &self.window) {
            a.process_event(w, &event);
        }
        let s = self.scale;
        let at = self.pointer;
        #[cfg(not(target_arch = "wasm32"))]
        let queued = self.events.events.len();
        match event {
            WindowEvent::CloseRequested => {
                self.app.close();
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                self.update();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => self.scale = scale_factor,
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::ModifiersChanged(m) => {
                self.mods = mods_of(m.state());
                self.events.push(InputEvent::Mods {
                    mods: Mods(self.mods),
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ((position.x / s) as f32, (position.y / s) as f32);
                self.events.push(InputEvent::PointerMove {
                    x: self.pointer.0,
                    y: self.pointer.1,
                });
            }
            WindowEvent::CursorLeft { .. } => {
                self.events.push(InputEvent::PointerLeave);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Right,
                    _ => return,
                };
                let (x, y) = at;
                self.events.push(match state {
                    ElementState::Pressed => InputEvent::PointerDown { x, y, button },
                    ElementState::Released => InputEvent::PointerUp { x, y, button },
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    // ~20 px per wheel line on every platform we target.
                    MouseScrollDelta::PixelDelta(p) => ((p.x / 20.0) as f32, (p.y / 20.0) as f32),
                };
                self.events.push(InputEvent::Wheel {
                    x: at.0,
                    y: at.1,
                    dx,
                    dy,
                });
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                self.push_key(&event.logical_key, event.text.as_deref());
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.events.push_preedit(&text);
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.events.push_preedit("");
                self.events.push_text(&text);
            }
            _ => {}
        }
        // Each input stamps the frame that answers it (`SCHEMIFY_FRAME_LOG`).
        #[cfg(not(target_arch = "wasm32"))]
        if self.events.events.len() > queued {
            let now = std::time::Instant::now();
            self.queued = Some((self.queued.map_or(now, |q| q.0), now));
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        self.tick |= matches!(cause, StartCause::ResumeTimeReached { .. });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match pace(std::mem::take(&mut self.tick), !self.events.is_empty()) {
            Pace::Update => {
                if self.update() {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                } else {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.shown = None; // no frame answers this batch
                    }
                }
            }
            Pace::Redraw => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Pace::Sleep => {}
        }
        if self.app.exit_requested() {
            event_loop.exit();
            return;
        }
        if self.access_dirty {
            let (adapter, app) = (&mut self.adapter, &mut self.app);
            if let Some(a) = adapter {
                a.update_if_active(|| app.access());
            }
            self.access_dirty = false;
        }
        let flow = match self.app.wake_ms() {
            Some(ms) => ControlFlow::wait_duration(std::time::Duration::from_millis(u64::from(ms))),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }
}

#[cfg(test)]
mod tests {
    use super::{Pace, pace};

    /// D39: nothing pending sleeps; input alone never updates before the granted redraw, so
    /// events during a frame in flight get one update at the next grant, not one each; a
    /// deadline alone updates once, input or not.
    #[test]
    fn pacing_rule() {
        assert_eq!(pace(false, false), Pace::Sleep);
        assert_eq!(pace(false, true), Pace::Redraw);
        assert_ne!(pace(false, true), Pace::Update);
        assert_eq!(pace(true, false), Pace::Update);
        assert_eq!(pace(true, true), Pace::Update);
    }
}
