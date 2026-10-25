//! `window`: the editor in a window. The spine of one window frame is
//! `Editor::update(input)` → [`paint::frame`] (`Editor::picture` → pixels →
//! `Editor::painted`) → [`paint::serve_raster`] (a PNG export the frame asked for); the
//! window sleeps until input or `Editor::wake_ms` (D39 idle CPU 0%).

use schemify_editor::editor::{Editor, Options};
use schemify_editor::platform;
use schemify_render::{App, Painter};
use schemify_ui::InputEvents;

use crate::paint::{self, Caches};

/// The window shell's view of an [`Editor`] (`render::run` drives it): the editor, its
/// paint caches and the frame clock.
pub struct Window {
    ed: Editor,
    caches: Caches,
    #[cfg(not(target_arch = "wasm32"))]
    start: std::time::Instant,
}

impl Window {
    /// Wraps an editor; the frame clock (`Editor::update`'s `now_ms`) starts at this call.
    pub fn new(ed: Editor) -> Window {
        Window {
            ed,
            caches: Caches::default(),
            #[cfg(not(target_arch = "wasm32"))]
            start: std::time::Instant::now(),
        }
    }

    /// The wrapped editor: the `--rpc` server (`crate::serve`) queues commands into it
    /// between frames.
    pub fn editor(&mut self) -> &mut Editor {
        &mut self.ed
    }

    fn now_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.start.elapsed().as_millis() as u64
        }
        // Web: `Instant` panics in a browser; the page's clock (web spec §1). A frozen clock
        // made every second click a double click and kept plugin timers from firing.
        #[cfg(target_arch = "wasm32")]
        {
            platform::unix_ms()
        }
    }
}

impl App for Window {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        let now = self.now_ms();
        let changed = self.ed.update(events, viewport, now);
        paint::serve_raster(&mut self.ed);
        changed
    }

    fn exit_requested(&self) -> bool {
        // Journals are flushed after every edit, so nothing is lost by leaving now.
        self.ed.wants_quit()
    }

    fn paint<P: Painter>(&mut self, p: &mut P, _viewport: (f32, f32)) {
        paint::frame(&mut self.ed, &mut self.caches, p);
    }

    fn wake_ms(&self) -> Option<u32> {
        self.ed.wake_ms()
    }

    fn ime_area(&self) -> Option<(f32, f32, f32, f32)> {
        self.ed.ime_area()
    }

    fn access(&mut self) -> schemify_render::accesskit::TreeUpdate {
        self.ed.access_tree()
    }
}

/// Opens the editor window and runs until it closes (native: blocks).
///
/// # Errors
///
/// The project or file cannot be opened, or no window can be created.
pub fn run(opts: Options) -> Result<(), String> {
    let ed = open(opts)?;
    schemify_render::run("Schemify", Window::new(ed)).map_err(|e| e.to_string())
}

/// Opens the editor a window shows: the platform's state directory unless `opts` names
/// one, and the system clipboard.
///
/// # Errors
///
/// The project or file cannot be opened.
pub fn open(mut opts: Options) -> Result<Editor, String> {
    if opts.state_dir.is_none() {
        opts.state_dir = platform::state_dir();
    }
    opts.system_clipboard = true;
    Editor::new(opts)
}
