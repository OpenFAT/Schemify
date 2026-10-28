//! Window, event loop and GPU surface (winit + wgpu), native and web from one code
//! path: an [`App`] in, frames and an accessibility tree out. Redraw is event-driven: nothing
//! runs while idle (D39). Input is paced by the compositor ([`pace`]): an event asks for a
//! redraw, and [`App::update`] runs when the `RedrawRequested` is granted (on Wayland, after
//! the previous frame's callback), once over every event queued until then, so a frame shows
//! the newest input and one update serves one frame. An app with background work (a
//! simulation, a child process, a plugin timer) asks for a tick through [`App::wake_ms`]; a
//! tick updates at once, since a hidden window is granted no redraw and the run it polls must
//! go on. The loop then sleeps until that deadline or the next input, never busy-polls. The
//! GPU is opened and drawn in [`gpu`], input translated in [`input`].
//!
//! Contents:
//! - `pub trait App`
//! - `pub struct Waker`, `Waker::wake`
//! - `pub enum RunError`
//! - `pub fn run<A: App + 'static>(title: &str, app: A) -> Result<(), RunError>`
//! - `pub fn block_on<F: Future>(f: F) -> F::Output` (native)
//! - `pub(crate) fn logical(w: u32, h: u32, scale: f64) -> (f32, f32)`

#[cfg(target_arch = "wasm32")]
mod cpu_view;
mod gpu;
mod input;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
use std::mem;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use accesskit::{Action, TreeUpdate};
use peniko::Color;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use schemify_ui::{InputEvent, InputEvents};

use crate::batch::Batch;
use crate::gpu::InstanceRows;
use crate::painter::Painter;
use gpu::Gpu;
#[cfg(target_arch = "wasm32")]
use gpu::TIERS;

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

/// Why [`run`] failed.
#[derive(Debug)]
pub enum RunError {
    /// The windowing system could not start an event loop.
    EventLoop(String),
    /// Native: neither a hardware adapter nor wgpu's software one (WARP on Windows, Mesa's
    /// lavapipe or llvmpipe on Linux) could draw the window, or a lost device could not be
    /// replaced. The window closed after the app's `close`.
    NoGpu(String),
}

enum UserEvent {
    Access(accesskit_winit::Event),
    /// [`Waker::wake`]: update with no input.
    Wake,
    /// Web: a tier's GPU is made and waits in `pending` with its tier.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(dead_code, reason = "natively the GPU opens on the loop's thread")
    )]
    GpuReady,
    /// Web: `tier` is unusable (no adapter or device, the device lost, or presented frames
    /// that never reach the page), for reason `why`; the next tier is tried, and past the
    /// last frames go to the CPU overlay (`cpu_view`). Logged once per tier; a report about a
    /// tier already left is ignored. Native: the device was lost (`tier` 0).
    NoGpu {
        #[cfg_attr(
            not(target_arch = "wasm32"),
            allow(dead_code, reason = "natively there is one tier")
        )]
        tier: usize,
        why: String,
    },
}

/// When the oldest and the newest of a run of inputs arrived (`SCHEMIFY_FRAME_LOG`).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug)]
struct InputSpan {
    oldest: Instant,
    newest: Instant,
}

/// The window, its GPU and the app, as the event loop drives them.
struct Shell<A: App> {
    app: A,
    title: String,
    proxy: EventLoopProxy<UserEvent>,
    /// `Arc`: wgpu's surface takes a `'static` handle to the window it draws into
    /// (`SurfaceTarget::from(Arc<Window>)`) while the shell keeps the window too.
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    /// Web: a GPU made by the tier's future, with its tier, until `GpuReady` takes it. `Rc`:
    /// the spawned future outlives the `start_gpu` call that made it and fills this slot the
    /// shell reads (one thread, so `Rc<RefCell>`).
    #[cfg(target_arch = "wasm32")]
    pending: Rc<RefCell<Option<(usize, Gpu)>>>,
    /// Web: the tier tried now (an index into `TIERS`; `TIERS.len()` is the CPU).
    #[cfg(target_arch = "wasm32")]
    tier: usize,
    /// Web without a GPU: the CPU-rastered overlay (web spec §7).
    #[cfg(target_arch = "wasm32")]
    cpu: Option<cpu_view::CpuView>,
    /// Web: the tier's first GPU frame was presented and `cpu_view::shows_frames` checks it.
    #[cfg(target_arch = "wasm32")]
    probed: bool,
    adapter: Option<accesskit_winit::Adapter>,
    events: InputEvents,
    mods: u8,
    pointer: (f32, f32),
    scale: f64,
    /// The frame's batch and the coverage atlas behind it (kept across frames).
    batch: Batch,
    /// The canvas's instance rows (kept across frames; `Painter::canvas_rows` patches them).
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
    queued: Option<InputSpan>,
    /// The same for the input the updates since the last present consumed: what the next
    /// present shows. The oldest is the lag a log reader sees, the newest what the hand feels.
    #[cfg(not(target_arch = "wasm32"))]
    shown: Option<InputSpan>,
    /// Updates that consumed input since the last present (the frame's coalescing).
    #[cfg(not(target_arch = "wasm32"))]
    updates: u32,
    /// When the last frame was presented (the log's frame period).
    #[cfg(not(target_arch = "wasm32"))]
    presented: Option<Instant>,
    /// Native: why no GPU could draw the window ([`RunError::NoGpu`]).
    #[cfg(not(target_arch = "wasm32"))]
    failed: Option<String>,
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

/// Opens a window and runs `app` until it closes. Native: blocks. Web (wasm32): attaches a
/// canvas to the document body, returns immediately, and the browser drives the loop.
///
/// # Errors
///
/// Fails when no event loop can be created (no display server). Native: a GPU without
/// Vulkan/Metal/DX12 support is reported on stderr and leaves the window blank. Web: when
/// WebGPU is unusable (no adapter, the device lost, or frames that never reach the page) the
/// editor draws through WebGL2, and when that is unusable too, on the CPU; each step is
/// logged once as `gpu: <tier> unusable (<reason>); drawing with WebGL2` or `…; drawing on
/// the CPU` (web spec §7).
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
        tier: 0,
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
        #[cfg(not(target_arch = "wasm32"))]
        failed: None,
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut shell = shell;
        event_loop
            .run_app(&mut shell)
            .map_err(|e| RunError::EventLoop(e.to_string()))?;
        shell.failed.map_or(Ok(()), |why| Err(RunError::NoGpu(why)))
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(shell);
        Ok(())
    }
}

/// Runs `f` to completion on this thread, parking the thread while `f` is pending and waking it
/// when `f`'s waker fires. Native wgpu's futures (adapter, device, buffer mapping) resolve
/// without a reactor, so this stands in for an executor; on the web the browser's event loop
/// runs them (`wasm_bindgen_futures`).
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
    // `Arc`: `Wake::wake` takes the waker by shared ownership (std's waker contract).
    let waker = std::task::Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park(); // a wake before this park makes it return at once
    }
}

/// A `w × h` physical-pixel size in logical pixels at `scale` physical per logical.
pub(crate) fn logical(w: u32, h: u32, scale: f64) -> (f32, f32) {
    assert!(scale > 0.0, "a positive scale, got {scale}");
    assert!(scale.is_finite(), "a finite scale");
    let size = ((f64::from(w) / scale) as f32, (f64::from(h) / scale) as f32);
    debug_assert!(size.0 >= 0.0);
    debug_assert!(size.1 >= 0.0);
    size
}

impl Waker {
    /// Schedules a tick. Wakes coalesce: several before the next frame give one update.
    pub fn wake(&self) {
        // Err only once the event loop is gone, when no frame is wanted anyway.
        let _ = self.0.send_event(UserEvent::Wake);
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::EventLoop(e) => write!(f, "event loop: {e}"),
            RunError::NoGpu(e) => write!(
                f,
                "no GPU can draw the window: {e} (a software adapter needs Mesa's lavapipe \
                 on Linux, WARP on Windows)"
            ),
        }
    }
}

impl std::error::Error for RunError {}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::Access(e)
    }
}

impl<A: App> ApplicationHandler<UserEvent> for Shell<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        assert!(self.gpu.is_none(), "a GPU draws into a window opened first");
        assert!(
            self.adapter.is_none(),
            "AccessKit attaches to the window opened here"
        );
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
        assert!(self.scale > 0.0, "winit reports a positive scale factor");
        #[cfg(not(target_arch = "wasm32"))]
        match self.open_gpu(&window) {
            Ok(g) => self.gpu = Some(g),
            Err(e) => return self.fail(event_loop, e),
        }
        #[cfg(target_arch = "wasm32")]
        self.start_gpu(&window);
        self.window = Some(window);
        self.update();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        debug_assert!(self.events.is_empty(), "the first update took the queue");
    }

    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Access(e) => match e.window_event {
                accesskit_winit::WindowEvent::InitialTreeRequested => self.access_dirty = true,
                accesskit_winit::WindowEvent::ActionRequested(req)
                    if req.action == Action::Click =>
                {
                    // A full queue drops the click, as it drops any input past its capacity.
                    let _ = self.events.push(InputEvent::Activate {
                        node: schemify_ui::access::sym_of(req.target_node),
                    });
                }
                _ => {}
            },
            UserEvent::Wake => self.tick = true,
            UserEvent::GpuReady => {
                #[cfg(target_arch = "wasm32")]
                {
                    let ready = self.pending.borrow_mut().take();
                    if let Some((t, g)) = ready
                        && t == self.tier
                    {
                        assert!(t < TIERS.len(), "a GPU is made on a GPU tier");
                        self.gpu = Some(g);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            #[cfg_attr(
                not(target_arch = "wasm32"),
                allow(unused_variables, reason = "natively there is one tier")
            )]
            UserEvent::NoGpu { tier, why } => {
                // Native: the device was lost. The same ladder opens another, sent every row
                // and mask again; past it the window closes with the reason.
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(w) = self.window.clone() {
                    eprintln!("gpu: {why}; opening another device");
                    self.gpu = None;
                    (self.rows, self.batch) = (InstanceRows::default(), Batch::default());
                    // A new device holds nothing: every row is written again.
                    debug_assert_eq!(self.rows.dirty_bytes(), 0, "no row is packed yet");
                    debug_assert!(self.batch.atlas.is_empty(), "no mask is held yet");
                    match self.open_gpu(&w) {
                        Ok(g) => self.gpu = Some(g),
                        Err(e) => return self.fail(event_loop, format!("{why}; then {e}")),
                    }
                    w.request_redraw();
                }
                #[cfg(target_arch = "wasm32")]
                if let Some(w) = self.window.clone()
                    && tier == self.tier
                    && tier < TIERS.len()
                {
                    let next = tier + 1;
                    assert!(next <= TIERS.len(), "past the last GPU tier is the CPU");
                    let then = TIERS
                        .get(next)
                        .map_or("on the CPU".to_owned(), |t| format!("with {}", t.1));
                    // Once per tier: the console line the smoke test looks for (web spec §7).
                    let line = format!("gpu: {} unusable ({why}); drawing {then}", TIERS[tier].1);
                    web_sys::console::error_1(&line.into());
                    (self.tier, self.gpu, self.probed) = (next, None, false);
                    debug_assert!(self.cpu.is_none(), "the CPU tier is the last one");
                    // The rows and masks the old device holds: the next one is sent all of them.
                    (self.rows, self.batch) = (InstanceRows::default(), Batch::default());
                    self.pending.borrow_mut().take();
                    if next < TIERS.len() {
                        self.start_gpu(&w);
                    } else {
                        match cpu_view::CpuView::new(&w) {
                            Ok(v) => self.cpu = Some(v),
                            Err(e) => web_sys::console::error_1(&format!("cpu view: {e}").into()),
                        }
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                assert!(scale_factor > 0.0, "winit reports a positive scale factor");
                self.scale = scale_factor;
            }
            WindowEvent::RedrawRequested => {
                self.render();
                debug_assert!(self.events.is_empty(), "a granted redraw answers the queue");
            }
            other => self.input(other),
        }
        assert!(
            self.events.events.len() <= InputEvents::CAPACITY,
            "the input queue is bounded"
        );
        // Each input stamps the frame that answers it (`SCHEMIFY_FRAME_LOG`).
        #[cfg(not(target_arch = "wasm32"))]
        if self.events.events.len() > queued {
            let now = Instant::now();
            let oldest = self.queued.map_or(now, |q| q.oldest);
            self.queued = Some(InputSpan {
                oldest,
                newest: now,
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(q) = self.queued {
            debug_assert!(q.oldest <= q.newest, "input arrives in time order");
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        self.tick |= matches!(cause, StartCause::ResumeTimeReached { .. });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match pace(mem::take(&mut self.tick), !self.events.is_empty()) {
            Pace::Update => {
                let changed = self.update();
                debug_assert!(self.events.is_empty(), "an update takes the queue");
                if changed {
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
                debug_assert!(!self.events.is_empty(), "input waits for its redraw");
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Pace::Sleep => debug_assert!(self.events.is_empty(), "nothing pending sleeps"),
        }
        debug_assert!(!self.tick, "a tick is taken once");
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
            Some(ms) => ControlFlow::wait_duration(Duration::from_millis(u64::from(ms))),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }
}

// ---- private ----

impl<A: App> Shell<A> {
    fn viewport(&self) -> (f32, f32) {
        let size = self
            .window
            .as_ref()
            .map_or_else(Default::default, |w| w.inner_size());
        logical(size.width, size.height, self.scale)
    }

    /// One [`App::update`] over the queued input (a tick when there is none); keeps the IME
    /// area in step and marks the accessibility tree stale when the picture changed. Returns
    /// whether the picture changed.
    fn update(&mut self) -> bool {
        let viewport = self.viewport();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(InputSpan { oldest, newest }) = self.queued.take() {
            let oldest = self.shown.map_or(oldest, |s| s.oldest);
            self.shown = Some(InputSpan { oldest, newest });
            self.updates += 1;
        }
        let changed = self.app.update(&self.events, viewport);
        self.events.clear();
        debug_assert!(
            self.events.text.is_empty(),
            "the payloads go with their events"
        );
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
        #[cfg(not(target_arch = "wasm32"))]
        debug_assert!(self.queued.is_none(), "an update consumes the queued input");
        changed
    }
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

#[cfg(test)]
mod tests {
    use super::{Pace, block_on, pace};

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

    /// `block_on` returns a ready value, and one that becomes ready only after another
    /// thread wakes it (a park that a wake must end).
    #[test]
    fn block_on_waits_for_the_wake() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::task::{Poll, Waker};
        assert_eq!(block_on(async { 7 }), 7, "ready at once");
        let done = Arc::new(AtomicBool::new(false));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::default();
        let (d, w) = (done.clone(), waker.clone());
        let polls = std::cell::Cell::new(0);
        let pending = std::future::poll_fn(|cx| {
            polls.set(polls.get() + 1);
            if d.load(Ordering::SeqCst) {
                return Poll::Ready(42);
            }
            *w.lock().expect("lock") = Some(cx.waker().clone());
            Poll::Pending
        });
        let (d2, w2) = (done.clone(), waker.clone());
        let t = std::thread::spawn(move || {
            while w2.lock().expect("lock").is_none() {
                std::thread::yield_now();
            }
            d2.store(true, Ordering::SeqCst);
            w2.lock().expect("lock").take().expect("waker").wake();
        });
        assert_eq!(block_on(pending), 42, "ready after the wake");
        t.join().expect("waker thread");
        assert!(
            polls.get() >= 2,
            "polled again after the wake, not spun to completion"
        );
    }
}
