//! Oracles for the UI tables and their egui view (docs/dev/testing.md): composition (last wins
//! + health), what the view shows, routing (keys, menus, clicks, canvas events), dialogs,
//! split dividers and text fields, all through real egui frames with no window.
//!
//! Contents: `pub(crate) struct Fixture` (`sym`, `node`, `rect`, `route`, `center`, `click`,
//! `commands`), `pub(crate) const VIEW`, `pub(crate) fn fixture(plugins) -> Fixture`,
//! `pub(crate) fn key(f, chord) -> Routed`; the tests live in `compose`, `view` and `fields`.

mod compose;
mod fields;
mod view;

use schemify_ui::{
    Button, Chord, InputEvent, InputEvents, Rect, Routed, Strings, Sym, UiLayout, UiState,
    UiTables, compose, records,
};

/// The default UI composed with test plugins, shown at [`VIEW`], with its interaction state.
pub(crate) struct Fixture {
    pub(crate) strings: Strings,
    pub(crate) tables: UiTables,
    pub(crate) ctx: schemify_ui::egui::Context,
    pub(crate) state: UiState,
    pub(crate) frame: UiLayout,
    now: f64,
}

/// The viewport in logical pixels.
pub(crate) const VIEW: (f32, f32) = (1280.0, 800.0);

pub(crate) fn fixture(plugins: &[&str]) -> Fixture {
    let mut strings = Strings::new();
    let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    assert_eq!(rejects, vec![], "default UI must parse cleanly");
    let parsed: Vec<_> = plugins
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (c, r) = records::parse(p, &mut strings);
            assert_eq!(r, vec![]);
            (strings.intern(&format!("plugin{i}")), c)
        })
        .collect();
    let refs: Vec<_> = parsed.iter().map(|(n, c)| (*n, c)).collect();
    let tables = compose(&strings, &base, &refs);
    let mut f = Fixture {
        strings,
        tables,
        ctx: schemify_ui::context(),
        state: UiState::default(),
        frame: UiLayout::default(),
        now: 0.0,
    };
    // egui hit-tests against the previous frame and flex sizes settle over two.
    for _ in 0..3 {
        f.route(&InputEvents::default());
    }
    f
}

impl Fixture {
    pub(crate) fn sym(&self, s: &str) -> Sym {
        self.strings
            .find(s)
            .unwrap_or_else(|| panic!("{s} not interned"))
    }
    pub(crate) fn node(&self, s: &str) -> u16 {
        self.tables
            .nodes
            .find(self.sym(s))
            .unwrap_or_else(|| panic!("{s} not composed"))
    }
    pub(crate) fn rect(&self, s: &str) -> Rect {
        let r = self
            .frame
            .row_of(self.node(s))
            .unwrap_or_else(|| panic!("{s} not shown"));
        self.frame.rect()[r]
    }
    /// One egui frame over `events` (the clock advances 1 s, so no double clicks).
    pub(crate) fn route(&mut self, events: &InputEvents) -> Routed {
        let mut out = Routed::default();
        let mut raw = events.raw(VIEW);
        self.now += 1.0;
        raw.time = Some(self.now);
        let Fixture {
            strings,
            tables,
            ctx,
            state,
            frame,
            ..
        } = self;
        let mut o = ctx.run_ui(raw, |ui| schemify_ui::show(ui, tables, strings, state, frame, &mut out));
        o.textures_delta.clear(); // no painter in a headless test
        out
    }
    pub(crate) fn center(&self, s: &str) -> (f32, f32) {
        let r = self.rect(s);
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }
    /// Moves to `s`, then presses and releases there (the hover lands a frame first, as a real
    /// pointer's would).
    pub(crate) fn click(&mut self, s: &str) -> Routed {
        let (x, y) = self.center(s);
        self.click_at(x, y)
    }
    pub(crate) fn click_at(&mut self, x: f32, y: f32) -> Routed {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove { x, y });
        self.route(&ev);
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerDown { x, y, button: Button::Left });
        ev.push(InputEvent::PointerUp { x, y, button: Button::Left });
        let out = self.route(&ev);
        self.route(&InputEvents::default());
        out
    }
    pub(crate) fn commands(&self, r: &Routed) -> Vec<String> {
        r.commands
            .command
            .iter()
            .map(|&c| self.strings.get(c).to_owned())
            .collect()
    }
}

pub(crate) fn key(f: &mut Fixture, chord: &str) -> Routed {
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse(chord).expect("the test names a valid chord"),
    });
    f.route(&ev)
}
