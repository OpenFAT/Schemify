//! Headless editor harness: a copy of the fixture project in a temp directory, scripted
//! input batches through the real frame spine, and CPU rendering.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use schemify_app::editor::{Editor, Options};
use schemify_app::geometry::Pt;
use schemify_render::CpuPainter;
use schemify_render::vello::peniko::Color;
use schemify_ui::{Button, Chord, InputEvent, InputEvents, Mods};

/// Logical window size of every scripted session.
pub const VIEW: (f32, f32) = (1280.0, 800.0);

/// One xschem unit in milli-units.
pub const U: i32 = 1000;

/// Copies `tests/fixtures/<name>` into a fresh temp directory and returns its path. Tests
/// never open a fixture in place: a GUI session on a fixture leaves `.schemify/` state (a
/// journal replayed as crash recovery, netlists, runs) that must not reach a test.
pub fn fixture(name: &str, tag: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dst = std::env::temp_dir().join(format!(
        "schemify-editor-{tag}-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dst);
    copy_dir(&src, &dst);
    dst
}

/// Copies the tree `src` to `dst`, leaving out every `.schemify/` directory (derived editor
/// state: journals, netlists, runs, exports).
pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        let to = dst.join(p.file_name().unwrap());
        if p.file_name().is_some_and(|n| n == ".schemify") {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &to);
        } else {
            std::fs::copy(&p, &to).unwrap();
        }
    }
}

/// A scripted session: the editor plus a fake clock.
pub struct Session {
    pub ed: Editor,
    pub now: u64,
    pub dir: PathBuf,
}

impl Session {
    /// Opens `file` of fixture `name` (a copy), with or without the default UI plugin.
    pub fn open(name: &str, file: &str, tag: &str, default_ui: bool) -> Session {
        let dir = fixture(name, tag);
        let ed = Editor::new(Options {
            no_default_ui: !default_ui,
            path: Some(dir.join(file)),
            seed: 7,
            ..Default::default()
        })
        .expect("fixture opens");
        let mut s = Session { ed, now: 1, dir };
        s.frame(InputEvents::default());
        s.frame(InputEvents::default());
        s
    }

    /// Runs one frame with `events`; the clock advances 1 s per frame (no double clicks
    /// unless a test sets `now`).
    pub fn frame(&mut self, events: InputEvents) -> bool {
        self.now += 1000;
        self.ed.update(&events, VIEW, self.now)
    }

    /// Presses a chord (`"w"`, `"Shift+R"`, `"Ctrl+Z"`).
    pub fn key(&mut self, chord: &str) {
        let chord = Chord::parse(chord).expect("chord parses");
        let mods = chord.mods();
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Mods { mods });
        ev.push(InputEvent::Key { chord });
        ev.push(InputEvent::Mods {
            mods: Mods::default(),
        });
        self.frame(ev);
    }

    /// Types text into the focused field.
    pub fn type_text(&mut self, s: &str) {
        let mut ev = InputEvents::default();
        ev.push_text(s);
        self.frame(ev);
    }

    /// Moves the pointer to world point `p`.
    pub fn move_to(&mut self, p: Pt) {
        let (x, y) = self.ed.to_screen(p);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove { x, y });
        self.frame(ev);
    }

    /// Moves the pointer to screen point `(x, y)`.
    pub fn move_screen(&mut self, x: f32, y: f32) {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove { x, y });
        self.frame(ev);
    }

    fn button(&mut self, p: Pt, down: bool, button: Button, mods: u8) {
        let (x, y) = self.ed.to_screen(p);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Mods { mods: Mods(mods) });
        ev.push(InputEvent::PointerMove { x, y });
        ev.push(if down {
            InputEvent::PointerDown { x, y, button }
        } else {
            InputEvent::PointerUp { x, y, button }
        });
        self.frame(ev);
    }

    /// Left click (press + release) at world point `p` with modifier bits `mods`.
    pub fn click_mods(&mut self, p: Pt, mods: u8) {
        self.button(p, true, Button::Left, mods);
        self.button(p, false, Button::Left, mods);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Mods {
            mods: Mods::default(),
        });
        self.frame(ev);
    }

    /// Left click at world point `p`.
    pub fn click(&mut self, p: Pt) {
        self.click_mods(p, 0);
    }

    /// Left drag from `a` to `b` (world), in steps.
    pub fn drag(&mut self, a: Pt, b: Pt, mods: u8) {
        self.button(a, true, Button::Left, mods);
        for k in 1..=4 {
            let p = (a.0 + (b.0 - a.0) * k / 4, a.1 + (b.1 - a.1) * k / 4);
            self.move_to(p);
        }
        self.button(b, false, Button::Left, mods);
    }

    /// Clicks the UI node `id` (menus, dialog buttons, list rows).
    pub fn click_node(&mut self, id: &str) {
        let s = &self.ed.shell();
        let sym = s
            .strings()
            .find(id)
            .unwrap_or_else(|| panic!("{id} interned"));
        let n = s
            .tables()
            .nodes
            .find(sym)
            .unwrap_or_else(|| panic!("{id} composed"));
        let r = s
            .layout()
            .row_of(n)
            .unwrap_or_else(|| panic!("{id} laid out"));
        let rect = s.layout().rect()[r];
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove { x, y });
        ev.push(InputEvent::PointerDown {
            x,
            y,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerUp {
            x,
            y,
            button: Button::Left,
        });
        self.frame(ev);
    }

    /// Canonical text of the active document.
    pub fn text(&self) -> String {
        let (h, _) = self.ed.active().expect("a tab");
        self.ed.session().project.doc(h).expect("open").canonical()
    }

    /// Renders the window on the CPU.
    pub fn render(&mut self) -> schemify_render::Pixmap {
        let mut p = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
        let drawn = self.ed.paint(&mut p);
        self.ed.painted(drawn);
        p.finish(Color::BLACK)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Writes `px` as a PNG to `path` (screenshots).
pub fn save_png(px: &schemify_render::Pixmap, path: &Path) {
    let png = px.clone().into_png().expect("encode png");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, png).unwrap();
}

/// Lines of `text` starting with `prefix`.
pub fn lines<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.starts_with(prefix)).collect()
}
