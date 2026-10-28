//! Keyboard navigation of the open popup chain (menus and dropdowns): a key chord in, the open
//! chain, the highlighted row ([`UiState::hover`]) and any activated command out.
//!
//! Contents:
//! - `pub(crate) fn open_menubar(tables, strings, state) -> bool`
//! - `pub(crate) fn menu_key(tables, strings, state, chord, out)`

use crate::input::Routed;
use crate::keys::{Chord, Key, Mods};
use crate::kinds::{Kind, Prop};
use crate::route;
use crate::state::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{NO_NODE, NodeIx, UiTables};

/// Which way Left and Right move along a menubar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Next,
    Previous,
}

/// Opens the first menu of the first menubar with the keyboard highlight on its first row.
/// Returns false when there is no menubar menu.
pub(crate) fn open_menubar(tables: &UiTables, strings: &Strings, state: &mut UiState) -> bool {
    let n = &tables.nodes;
    let Some(bar) = (0..n.len()).find(|&i| n.kind[i] == Kind::Menubar) else {
        return false;
    };
    let Some(first) = n
        .children(bar as NodeIx)
        .find(|&c| n.kind[c as usize] == Kind::Menu)
    else {
        return false;
    };
    state.open.clear();
    state.open.push(n.id[first as usize]);
    state.hover = first_row(tables, strings, first);
    debug_assert_eq!(state.open.len(), 1);
    debug_assert!(
        state.hover.is_none()
            || n.find(state.hover)
                .is_some_and(|h| n.parent[h as usize] == first),
        "the highlight is a row of the opened menu"
    );
    true
}

/// Keyboard navigation of the open popup chain; the deepest popup has the keyboard and
/// [`UiState::hover`] is its highlighted row. Up/Down move the highlight (wrapping), Right
/// opens a cascade or moves to the next menubar menu, Left closes a cascade or moves to the
/// previous menubar menu, Enter or Space activates, Escape closes one level, and a letter
/// (mnemonic) jumps to the next row whose label starts with it, activating it when it is the
/// only one.
pub(crate) fn menu_key(
    tables: &UiTables,
    strings: &Strings,
    state: &mut UiState,
    chord: Chord,
    out: &mut Routed,
) {
    assert!(!state.open.is_empty(), "a popup is open");
    let n = &tables.nodes;
    let Some(menu) = state.open.last().and_then(|&m| n.find(m)) else {
        state.open.clear(); // the open popup's node went away
        return;
    };
    let rows = nav_rows(tables, strings, menu);
    let cur = rows.iter().position(|&c| n.id[c as usize] == state.hover);
    let key = chord.key();
    let plain = chord.mods().0 & (Mods::CTRL | Mods::ALT | Mods::SUPER) == 0;
    match key {
        Key::ESCAPE => {
            let closed = state.open.pop().expect("checked non-empty");
            state.hover = if state.open.is_empty() {
                Sym::NONE
            } else {
                closed
            };
        }
        Key::DOWN | Key::UP if !rows.is_empty() => {
            let len = rows.len();
            let next = match (cur, key == Key::DOWN) {
                (None, true) => 0,
                (None, false) => len - 1,
                (Some(i), true) => (i + 1) % len,
                (Some(i), false) => (i + len - 1) % len,
            };
            state.hover = n.id[rows[next] as usize];
        }
        Key::RIGHT => match cur {
            Some(i) if n.kind[rows[i] as usize] == Kind::Menu => {
                activate(tables, strings, state, rows[i], out);
            }
            _ => step_menubar(tables, strings, state, Step::Next),
        },
        Key::LEFT => {
            if state.open.len() > 1 {
                state.hover = state.open.pop().expect("more than one open");
            } else {
                step_menubar(tables, strings, state, Step::Previous);
            }
        }
        Key::ENTER => {
            if let Some(i) = cur {
                activate(tables, strings, state, rows[i], out);
            }
        }
        k if k == Key(' ' as u32) && plain => {
            if let Some(i) = cur {
                activate(tables, strings, state, rows[i], out);
            }
        }
        k if plain && k.0 < Key::NAMED => {
            let Some(c) = char::from_u32(k.0) else { return };
            let starts = |row: NodeIx| {
                let label = strings.get(n.prop(Prop::Label)[row as usize]);
                label
                    .chars()
                    .next()
                    .is_some_and(|f| f.to_lowercase().eq(c.to_lowercase()))
            };
            let hits: Vec<usize> = (0..rows.len()).filter(|&j| starts(rows[j])).collect();
            match hits.as_slice() {
                [] => {}
                [only] => activate(tables, strings, state, rows[*only], out),
                many => {
                    // The next match after the highlight, wrapping.
                    let after = cur.map_or(0, |i| i + 1);
                    let j = many
                        .iter()
                        .copied()
                        .find(|&j| j >= after)
                        .unwrap_or(many[0]);
                    state.hover = n.id[rows[j] as usize];
                }
            }
        }
        _ => {}
    }
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}

// ---- private ----

/// Rows of popup `menu` the keyboard can land on: enabled items, cascades and dropdowns.
fn nav_rows(tables: &UiTables, strings: &Strings, menu: NodeIx) -> Vec<NodeIx> {
    let n = &tables.nodes;
    let rows: Vec<NodeIx> = n
        .children(menu)
        .filter(|&c| {
            matches!(n.kind[c as usize], Kind::Item | Kind::Menu | Kind::Dropdown)
                && !n.is_disabled(strings, c)
        })
        .collect();
    debug_assert!(rows.iter().all(|&c| n.parent[c as usize] == menu));
    debug_assert!(rows.is_sorted(), "children come in sibling order");
    rows
}

/// Id of the first keyboard row of `menu` (`Sym::NONE` when it has none).
fn first_row(tables: &UiTables, strings: &Strings, menu: NodeIx) -> Sym {
    let rows = nav_rows(tables, strings, menu);
    let id = rows
        .first()
        .map_or(Sym::NONE, |&c| tables.nodes.id[c as usize]);
    debug_assert_eq!(id.is_none(), rows.is_empty(), "node ids are never empty");
    id
}

/// Runs a popup row from the keyboard: an item runs its command and closes the chain; a
/// cascade or dropdown opens its popup with the highlight on its first row.
fn activate(
    tables: &UiTables,
    strings: &Strings,
    state: &mut UiState,
    row: NodeIx,
    out: &mut Routed,
) {
    let n = &tables.nodes;
    let i = row as usize;
    assert!(i < n.len(), "node index out of range");
    let opens = n.kind[i] == Kind::Menu || route::has_popup(tables, row);
    if opens {
        if state.open.len() < UiState::MAX_OPEN {
            state.open.push(n.id[i]);
            state.hover = first_row(tables, strings, row);
        }
        debug_assert!(state.open.len() <= UiState::MAX_OPEN);
        return;
    }
    let cmd = n.prop(Prop::Command)[i];
    if !cmd.is_none() && !n.is_disabled(strings, row) {
        out.commands.push(cmd, n.id[i], "");
        state.open.clear();
        state.hover = Sym::NONE;
    }
}

/// Moves the open chain to the next or previous menu of the menubar holding the
/// open root menu, highlighting its first row. Does nothing for a popup not opened from a
/// menubar (a dropdown).
fn step_menubar(tables: &UiTables, strings: &Strings, state: &mut UiState, step: Step) {
    let n = &tables.nodes;
    let Some(root) = state.open.first().and_then(|&m| n.find(m)) else {
        return;
    };
    let bar = n.parent[root as usize];
    if bar == NO_NODE || n.kind[bar as usize] != Kind::Menubar {
        return;
    }
    let menus: Vec<NodeIx> = n
        .children(bar)
        .filter(|&c| n.kind[c as usize] == Kind::Menu)
        .collect();
    let Some(at) = menus.iter().position(|&m| m == root) else {
        return;
    };
    let len = menus.len();
    debug_assert!(at < len);
    let next = menus[match step {
        Step::Next => (at + 1) % len,
        Step::Previous => (at + len - 1) % len,
    }];
    state.open.clear();
    state.open.push(n.id[next as usize]);
    state.hover = first_row(tables, strings, next);
    debug_assert_eq!(state.open.len(), 1);
}
