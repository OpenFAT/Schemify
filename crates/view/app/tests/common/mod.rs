//! Headless editor harness: a copy of the fixture project in a temp directory, scripted
//! input batches through the real frame spine, and CPU rendering.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use schemify_editor::editor::{Editor, Options};
use schemify_editor::geometry::Pt;
use schemify_render::CpuPainter;
use schemify_render::peniko::Color;
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
    std::fs::create_dir_all(dst).expect("the test's temp dir is writable");
    for e in std::fs::read_dir(src).expect("the fixture directory lists") {
        let p = e.expect("the fixture directory lists").path();
        let to = dst.join(p.file_name().expect("a listed entry has a file name"));
        if p.file_name().is_some_and(|n| n == ".schemify") {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &to);
        } else {
            std::fs::copy(&p, &to).expect("the fixture copies into the temp dir");
        }
    }
}

/// A scripted session: the editor plus a fake clock.
pub struct Session {
    pub ed: Editor,
    pub now: u64,
    pub dir: PathBuf,
    /// The window's paint caches (canvas raster layer, diff, indexes) across renders.
    pub caches: schemify_app::paint::Caches,
}

impl Session {
    /// Opens `file` of fixture `name` (a copy), with or without the default UI plugin. The
    /// user config only pins the simulator (the default, espice), so no frame depends on
    /// which simulators this machine has installed (detection, run spec §5): the status bar
    /// names it in every golden.
    pub fn open(name: &str, file: &str, tag: &str, default_ui: bool) -> Session {
        let dir = fixture(name, tag);
        let init = dir.join("init.lua");
        std::fs::write(&init, "schemify.opt.simulator = \"espice\"\n").expect("init.lua");
        Session::open_with(dir, file, default_ui, schemify_config::Startup::File(init))
    }

    /// As [`Session::open`] with no user config at all (`--clean`): the simulator is then
    /// detected from this machine's `PATH`.
    pub fn open_clean(name: &str, file: &str, tag: &str, default_ui: bool) -> Session {
        let dir = fixture(name, tag);
        Session::open_with(dir, file, default_ui, schemify_config::Startup::Clean)
    }

    fn open_with(
        dir: PathBuf,
        file: &str,
        default_ui: bool,
        config: schemify_config::Startup,
    ) -> Session {
        let ed = Editor::new(Options {
            no_default_ui: !default_ui,
            path: Some(dir.join(file)),
            seed: 7,
            config,
            ..Default::default()
        })
        .expect("fixture opens");
        let mut s = Session {
            ed,
            now: 1,
            dir,
            caches: Default::default(),
        };
        s.frame(InputEvents::default());
        s.frame(InputEvents::default());
        s
    }

    /// Runs one frame with `events`; the clock advances 1 s per frame (no double clicks
    /// unless a test sets `now`).
    pub fn frame(&mut self, events: InputEvents) -> bool {
        self.now += 1000;
        let changed = self.ed.update(&events, VIEW, self.now);
        schemify_app::paint::serve_raster(&mut self.ed);
        changed
    }

    /// Runs one frame with `events`, in order.
    pub fn send(&mut self, events: impl IntoIterator<Item = InputEvent>) -> bool {
        let mut ev = InputEvents::default();
        for e in events {
            ev.push(e);
        }
        self.frame(ev)
    }

    /// Presses a chord (`"w"`, `"Shift+R"`, `"Ctrl+Z"`).
    pub fn key(&mut self, chord: &str) {
        let chord = Chord::parse(chord).expect("chord parses");
        let mods = chord.mods();
        self.send([
            InputEvent::Mods { mods },
            InputEvent::Key { chord },
            InputEvent::Mods {
                mods: Mods::default(),
            },
        ]);
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
        self.send([InputEvent::PointerMove { x, y }]);
    }

    /// Moves the pointer to screen point `(x, y)`.
    pub fn move_screen(&mut self, x: f32, y: f32) {
        self.send([InputEvent::PointerMove { x, y }]);
    }

    fn button(&mut self, p: Pt, down: bool, button: Button, mods: u8) {
        let (x, y) = self.ed.to_screen(p);
        self.send([
            InputEvent::Mods { mods: Mods(mods) },
            InputEvent::PointerMove { x, y },
            if down {
                InputEvent::PointerDown { x, y, button }
            } else {
                InputEvent::PointerUp { x, y, button }
            },
        ]);
    }

    /// Left click (press + release) at world point `p` with modifier bits `mods`.
    pub fn click_mods(&mut self, p: Pt, mods: u8) {
        self.button(p, true, Button::Left, mods);
        self.button(p, false, Button::Left, mods);
        self.send([InputEvent::Mods {
            mods: Mods::default(),
        }]);
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
        self.send([
            InputEvent::PointerMove { x, y },
            InputEvent::PointerDown {
                x,
                y,
                button: Button::Left,
            },
            InputEvent::PointerUp {
                x,
                y,
                button: Button::Left,
            },
        ]);
    }

    /// Runs command `name` with `arg`, then one frame.
    pub fn run(&mut self, name: &str, arg: &str) {
        self.ed.command(name, arg);
        self.frame(InputEvents::default());
    }

    /// The status line.
    pub fn status(&self) -> &str {
        self.ed.session().status()
    }

    /// Replaces the active document's text through Properties → Edit file (one undo).
    pub fn set_text(&mut self, text: &str) {
        self.key("Alt+Q");
        self.run("dialog.edit_file.ok", text);
        assert_eq!(self.text(), text, "{}", self.status());
    }

    /// The label of composed UI node `id`.
    pub fn label(&self, id: &str) -> String {
        let sh = &self.ed.shell();
        let n = sh
            .strings()
            .find(id)
            .and_then(|i| sh.tables().nodes.find(i))
            .unwrap_or_else(|| panic!("{id} composed"));
        sh.strings()
            .get(sh.tables().nodes.prop(schemify_ui::Prop::Label)[n as usize])
            .to_owned()
    }

    /// Canonical text of the active document.
    pub fn text(&self) -> String {
        let (h, _) = self.ed.active().expect("a tab");
        self.ed.session().project.doc(h).expect("open").canonical()
    }

    /// Renders the window on the CPU.
    pub fn render(&mut self) -> schemify_render::Pixmap {
        let mut p = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
        schemify_app::paint::frame(&mut self.ed, &mut self.caches, &mut p);
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
        std::fs::create_dir_all(dir).expect("the test's temp dir is writable");
    }
    std::fs::write(path, png).expect("the test's temp dir is writable");
}

/// Compares `pixmap` with `tests/golden/<name>.png`: equal size, and at most 0.1% of pixels
/// off by more than 2 in a channel. `SCHEMIFY_BLESS=1` writes the golden first; a failure
/// writes the actual image beside the test binary's temp dir.
pub fn check_golden(name: &str, pixmap: schemify_render::Pixmap) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
    let path = format!("{dir}/{name}.png");
    let png = pixmap.clone().into_png().expect("encode png");
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::create_dir_all(dir).expect("the test's temp dir is writable");
        std::fs::write(&path, &png).expect("the test's temp dir is writable");
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{path} missing; run with SCHEMIFY_BLESS=1"));
    let want =
        schemify_render::Pixmap::from_png(std::io::Cursor::new(want)).expect("decode golden");
    assert_eq!(
        (want.width(), want.height()),
        (pixmap.width(), pixmap.height())
    );
    let bad = want
        .data()
        .iter()
        .zip(pixmap.data())
        .filter(|(a, b)| {
            let (a, b) = ([a.r, a.g, a.b, a.a], [b.r, b.g, b.b, b.a]);
            a.iter().zip(b).any(|(x, y)| x.abs_diff(y) > 2)
        })
        .count();
    let limit = want.data().len() / 1000;
    if bad > limit {
        let out = format!("{}/{name}.actual.png", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&out, &png).expect("the test's temp dir is writable");
        panic!("{name}: {bad} pixels differ (limit {limit}); actual written to {out}");
    }
}

/// Lines of `text` starting with `prefix`.
pub fn lines<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.starts_with(prefix)).collect()
}
