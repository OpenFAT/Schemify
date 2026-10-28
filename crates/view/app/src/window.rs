//! `window`: an [`Editor`] → a window frame loop (`schemify_render::run`; D39). One frame
//! is `Editor::update(input)` → [`paint::frame`] (`Editor::picture` → pixels →
//! `Editor::painted`) → [`paint::serve_raster`] (a PNG export the frame asked for); the
//! window sleeps until input or `Editor::wake_ms` (idle CPU 0%).
//!
//! Contents:
//! - `pub struct Window`, `Window::new(editor: Editor) -> Window`,
//!   `Window::editor_mut(&mut self) -> &mut Editor`
//! - `pub fn run(options: Options) -> Result<(), String>`
//! - `pub(crate) fn open(options: Options) -> Result<Editor, String>`

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use schemify_editor::editor::{Editor, Options};
use schemify_editor::platform;
use schemify_render::accesskit::TreeUpdate;
use schemify_render::{App, Painter};
use schemify_ui::InputEvents;

use crate::paint::{self, Caches};

/// The window shell's view of an [`Editor`] (`schemify_render::run` drives it): the
/// editor, its paint caches and the frame clock.
pub struct Window {
    editor: Editor,
    caches: Caches,
    #[cfg(not(target_arch = "wasm32"))]
    start: Instant,
}

impl Window {
    /// Wraps an editor; the frame clock (`Editor::update`'s `now_ms`) starts at this call.
    #[must_use]
    pub fn new(editor: Editor) -> Window {
        Window {
            editor,
            caches: Caches::default(),
            #[cfg(not(target_arch = "wasm32"))]
            start: Instant::now(),
        }
    }

    /// The wrapped editor: the `--rpc` server (`crate::serve`) queues commands into it
    /// between frames.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }
}

impl App for Window {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        assert!(viewport.0 >= 0.0, "viewport width {}", viewport.0);
        assert!(viewport.1 >= 0.0, "viewport height {}", viewport.1);
        let now = self.now_ms();
        let changed = self.editor.update(events, viewport, now);
        paint::serve_raster(&mut self.editor);
        changed
    }

    fn exit_requested(&self) -> bool {
        // The quit command settled every journal (`Editor::settle`) before setting this.
        self.editor.wants_quit()
    }

    fn close(&mut self) {
        self.editor.settle();
    }

    fn paint<P: Painter>(&mut self, painter: &mut P, _viewport: (f32, f32)) {
        paint::frame(&mut self.editor, &mut self.caches, painter);
    }

    fn wake_ms(&self) -> Option<u32> {
        self.editor.wake_ms()
    }

    fn ime_area(&self) -> Option<(f32, f32, f32, f32)> {
        self.editor.ime_area()
    }

    fn access(&mut self) -> TreeUpdate {
        self.editor.access_tree()
    }
}

/// Opens the editor window and runs until it closes (native: blocks; web: returns once the
/// window is attached).
///
/// # Errors
///
/// The project or file cannot be opened, or no window can be created.
pub fn run(options: Options) -> Result<(), String> {
    let editor = open(options)?;
    schemify_render::run("Schemify", Window::new(editor)).map_err(|error| error.to_string())
}

/// Opens the editor a window shows: the platform's state directory unless `options` names
/// one, and the system clipboard.
pub(crate) fn open(mut options: Options) -> Result<Editor, String> {
    if options.state_dir.is_none() {
        options.state_dir = platform::state_dir();
    }
    options.system_clipboard = true;
    Editor::new(options)
}

// ---- private ----

impl Window {
    /// Milliseconds since [`Window::new`] natively; the page's clock on the web, where
    /// `Instant` panics (web spec §1); a frozen clock made every second click a double
    /// click and kept plugin timers from firing.
    fn now_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
        #[cfg(target_arch = "wasm32")]
        {
            platform::unix_ms()
        }
    }
}
