//! Oracles for the UI tables (docs/dev/testing.md): composition (last wins + health), layout
//! geometry, input routing, text fields, menus and the AccessKit snapshot. Re-bless snapshots
//! with `SCHEMIFY_BLESS=1`.
//!
//! Contents: `pub(crate) struct Fixture` (`sym`, `node`, `row`, `relayout`, `route`, `center`,
//! `click`, `commands`), `pub(crate) const VIEW`, `pub(crate) fn fixture(plugins) -> Fixture`,
//! `pub(crate) fn key(f, chord) -> Routed`, `pub(crate) fn first_nav_row(f, menu) -> Sym`; the
//! tests live in `compose`, `layout`, `route`, `field` and `menu`.

mod compose;
mod field;
mod layout;
mod menu;
mod route;

use schemify_ui::{
    Button, Chord, InputEvent, InputEvents, Kind, Routed, Strings, Sym, TextSystem, UiLayout,
    UiState, UiTables, compose, layout, records, route_input,
};

/// The default UI composed with test plugins, laid out at [`VIEW`], with its routing state.
pub(crate) struct Fixture {
    pub(crate) strings: Strings,
    pub(crate) tables: UiTables,
    pub(crate) text: TextSystem,
    pub(crate) state: UiState,
    pub(crate) frame: UiLayout,
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
    let mut text = TextSystem::new();
    let state = UiState::default();
    let frame = layout(&tables, &strings, &mut text, &state, VIEW);
    Fixture {
        strings,
        tables,
        text,
        state,
        frame,
    }
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
    pub(crate) fn row(&self, s: &str) -> usize {
        self.frame
            .row_of(self.node(s))
            .unwrap_or_else(|| panic!("{s} not laid out"))
    }
    pub(crate) fn relayout(&mut self) {
        self.frame = layout(
            &self.tables,
            &self.strings,
            &mut self.text,
            &self.state,
            VIEW,
        );
    }
    pub(crate) fn route(&mut self, events: &InputEvents) -> Routed {
        let mut out = Routed::default();
        route_input(
            &self.tables,
            &self.frame,
            &self.strings,
            &mut self.text,
            &mut self.state,
            events,
            &mut out,
        );
        if out.relayout {
            self.relayout();
        }
        out
    }
    pub(crate) fn center(&self, s: &str) -> (f32, f32) {
        let r = self.frame.rect()[self.row(s)];
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }
    pub(crate) fn click(&mut self, s: &str) -> Routed {
        let (x, y) = self.center(s);
        let mut ev = InputEvents::default();
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
        self.route(&ev)
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

pub(crate) fn first_nav_row(f: &Fixture, menu: &str) -> Sym {
    let n = &f.tables.nodes;
    let c = n
        .children(f.node(menu))
        .find(|&c| matches!(n.kind()[c as usize], Kind::Item | Kind::Menu))
        .expect("the menu has a navigable row");
    n.id()[c as usize]
}
