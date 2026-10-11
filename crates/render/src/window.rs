//! Window, event loop and GPU surface (winit + wgpu via vello), native and web from one code
//! path. Redraw is event-driven: nothing runs while idle (D39). An app with background work
//! (a simulation, a child process, a plugin timer) asks for a tick through [`App::wake_ms`];
//! the loop then sleeps until that deadline or the next input, never busy-polls.

use std::sync::Arc;

use accesskit::{Action, TreeUpdate};
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::util::{RenderContext, RenderSurface};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use schemify_ui::{Button, Chord, InputEvent, InputEvents, Key, Mods};

use crate::layers::Layers;
use crate::painter::{Painter, ScenePainter};

/// What the application provides to [`run`]. Static dispatch: `run` is generic over it.
pub trait App {
    /// Consumes one batch of input (possibly empty, e.g. after a resize) for a viewport in
    /// logical pixels; returns true when the picture changed and a frame must be drawn.
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
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    GpuReady,
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::Access(e)
    }
}

struct Gpu {
    cx: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
}

impl Gpu {
    async fn new(window: Arc<Window>, w: u32, h: u32) -> Result<Gpu, String> {
        let mut cx = RenderContext::new();
        let surface = cx
            .create_surface(window, w, h, wgpu::PresentMode::AutoVsync)
            .await
            .map_err(|e| e.to_string())?;
        let device = &cx.devices[surface.dev_id].device;
        let options = RendererOptions {
            antialiasing_support: AaSupport::area_only(),
            ..RendererOptions::default()
        };
        let renderer = Renderer::new(device, options).map_err(|e| e.to_string())?;
        Ok(Gpu {
            cx,
            surface,
            renderer,
        })
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
    adapter: Option<accesskit_winit::Adapter>,
    events: InputEvents,
    mods: u8,
    pointer: (f32, f32),
    scale: f64,
    scene: Scene,
    /// Raster layers the app's cached canvases record into (kept across frames).
    layers: Layers,
    access_dirty: bool,
    /// The deadline [`App::wake_ms`] asked for has passed: update with no input.
    tick: bool,
}

/// Opens a window and runs `app` until it closes. Native: blocks. Web (wasm32): attaches a
/// canvas to the document body, returns immediately, and the browser drives the loop.
///
/// # Errors
///
/// Fails when no event loop can be created (no display server). A GPU without WebGPU/Vulkan/
/// Metal/DX12 support is reported on stderr and leaves the window blank.
pub fn run<A: App + 'static>(title: &str, app: A) -> Result<(), RunError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| RunError::EventLoop(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let shell = Shell {
        app,
        title: title.to_owned(),
        proxy: event_loop.create_proxy(),
        window: None,
        gpu: None,
        #[cfg(target_arch = "wasm32")]
        pending: Default::default(),
        adapter: None,
        events: InputEvents::default(),
        mods: 0,
        pointer: (0.0, 0.0),
        scale: 1.0,
        scene: Scene::new(),
        layers: Layers::default(),
        access_dirty: true,
        tick: false,
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
        (
            (f64::from(size.width) / self.scale) as f32,
            (f64::from(size.height) / self.scale) as f32,
        )
    }

    fn render(&mut self) {
        let (Some(window), Some(gpu)) = (&self.window, &mut self.gpu) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if gpu.surface.config.width != size.width || gpu.surface.config.height != size.height {
            gpu.cx
                .resize_surface(&mut gpu.surface, size.width, size.height);
        }
        let viewport = (
            (f64::from(size.width) / self.scale) as f32,
            (f64::from(size.height) / self.scale) as f32,
        );
        self.scene.reset();
        let base = Affine::scale(self.scale);
        self.layers.begin_frame(base);
        let mut painter = ScenePainter {
            scene: &mut self.scene,
            base,
            layers: Some(&mut self.layers),
        };
        self.app.paint(&mut painter, viewport);
        let dev = &gpu.cx.devices[gpu.surface.dev_id];
        if let Err(e) = self
            .layers
            .raster(&dev.device, &dev.queue, &mut gpu.renderer)
        {
            eprintln!("render layer: {e}");
        }
        let frame = match gpu.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.cx.configure_surface(&gpu.surface);
                window.request_redraw();
                return;
            }
            _ => return,
        };
        let dev = &gpu.cx.devices[gpu.surface.dev_id];
        // D39: a frame that only moved its cached layers (a pan, a zoom stand-in) skips vello:
        // the target still holds the last vello frame, and the layers are quads over it.
        let reuse = self.layers.reusable(size.width, size.height);
        if !reuse {
            let params = RenderParams {
                base_color: self.app.background(),
                width: size.width,
                height: size.height,
                antialiasing_method: AaConfig::Area,
            };
            if let Err(e) = gpu.renderer.render_to_texture(
                &dev.device,
                &dev.queue,
                &self.scene,
                &gpu.surface.target_view,
                &params,
            ) {
                eprintln!("render: {e}");
                return;
            }
            self.layers.rendered(size.width, size.height);
        }
        let mut encoder = dev
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit"),
            });
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        gpu.surface
            .blitter
            .copy(&dev.device, &mut encoder, &gpu.surface.target_view, &target);
        if reuse {
            self.layers.compose(
                &dev.device,
                &dev.queue,
                &mut encoder,
                &target,
                gpu.surface.format,
                (size.width, size.height),
            );
        }
        dev.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        dev.queue.present(frame);
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
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_visible(false);
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
                        *pending.borrow_mut() = Some(g);
                        let _ = proxy.send_event(UserEvent::GpuReady);
                    }
                    Err(e) => web_sys::console::error_1(&format!("gpu: {e}").into()),
                }
            });
        }
        self.window = Some(window);
        let viewport = self.viewport();
        self.app.update(&InputEvents::default(), viewport);
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
            UserEvent::GpuReady => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.gpu = self.pending.borrow_mut().take();
                }
                if let Some(w) = &self.window {
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
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => {
                let viewport = self.viewport();
                self.app.update(&InputEvents::default(), viewport);
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
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        self.tick |= matches!(cause, StartCause::ResumeTimeReached { .. });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.events.is_empty() || std::mem::take(&mut self.tick) {
            let viewport = self.viewport();
            if self.app.update(&self.events, viewport) {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                self.access_dirty = true;
            }
            self.events.clear();
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
