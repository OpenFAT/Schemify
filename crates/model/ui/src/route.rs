//! Input routing (spine: `ui::route_input`): one frame's event table in, `command_requests`,
//! canvas events and interaction-state changes out.
//!
//! The UI never edits `ui_nodes` in response to input. Clicking a checkbox emits its command;
//! whoever owns the checked state answers with a `set … checked` contribution. Only transient
//! interaction state lives in [`UiState`], keyed by stable node ids so it survives
//! recomposition. Pointer and wheel events are routed in `pointer`, keys and text in
//! `keyboard`; keys on an open popup go on to [`crate::menu`], keys and text on a focused field
//! to [`crate::field`].
//!
//! Contents:
//! - `pub fn route_input(tables, layout, strings, text, state, events, out)`
//! - `pub(crate) fn popup_depth(tables, state, node) -> Option<usize>`
//! - `pub(crate) fn has_popup(tables, node) -> bool`

use crate::field;
use crate::field::Lines;
use crate::input::{InputEvent, InputEvents, Routed};
use crate::kinds::Kind;
use crate::layout::{Shown, UiLayout};
use crate::state::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{NO_NODE, NodeIx, UiTables};
use crate::text::TextSystem;

mod keyboard;
mod pointer;

/// One [`route_input`] call: its read-only inputs, the state and output it writes, and
/// whether the focused field's text changed since its last `input` command.
struct Router<'frame> {
    tables: &'frame UiTables,
    layout: &'frame UiLayout,
    strings: &'frame Strings,
    text: &'frame mut TextSystem,
    state: &'frame mut UiState,
    out: &'frame mut Routed,
    typed: bool,
}

/// The topmost row under the pointer, the node it shows and how.
#[derive(Clone, Copy, Debug)]
struct Hit {
    row: usize,
    node: NodeIx,
    shown: Shown,
}

/// Routes one frame of input: keymap chords, clicks on items and buttons, menu and dropdown
/// popups (pointer and keyboard), hover tracking, split dividers, text-field focus and editing
/// (including IME preedit), and canvas forwarding.
///
/// Reads `tables` and `layout` (which must be the layout of the current `state`); writes only
/// `state` and `out`. A text field's `input` command is pushed with its text before any other
/// command of the same event that follows the edit.
///
/// # Panics
///
/// Panics if an event's text range lies outside `events.text`.
pub fn route_input(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &mut UiState,
    events: &InputEvents,
    out: &mut Routed,
) {
    assert!(
        events.events.len() <= InputEvents::CAPACITY,
        "input table over capacity"
    );
    out.clear();
    let mut r = Router {
        tables,
        layout,
        strings,
        text,
        state,
        out,
        typed: false,
    };
    for &ev in &events.events {
        if let InputEvent::Text { start, end } | InputEvent::Preedit { start, end } = ev {
            assert!(start <= end, "a text range runs forward");
            assert!(
                end as usize <= events.text.len(),
                "a text range inside the payloads"
            );
        }
        match ev {
            InputEvent::PointerMove { x, y } => r.pointer_move(ev, x, y),
            InputEvent::PointerDown { x, y, button } => r.pointer_down(ev, x, y, button),
            InputEvent::PointerUp { .. } => r.pointer_up(ev),
            InputEvent::PointerLeave => {
                if !r.state.hover.is_none() {
                    r.state.hover = Sym::NONE;
                    r.out.redraw = true;
                }
            }
            InputEvent::Wheel { x, y, dy, .. } => r.wheel(ev, x, y, dy),
            InputEvent::Key { chord } => r.key(chord),
            InputEvent::Text { start, end } => {
                r.text_in(&events.text[start as usize..end as usize]);
            }
            InputEvent::Preedit { start, end } => {
                r.preedit(&events.text[start as usize..end as usize]);
            }
            InputEvent::Mods { .. } => {}
            InputEvent::Activate { node } => r.activate(node),
        }
    }
    field::flush_typed(r.tables, r.state, &mut r.typed, r.out);
    debug_assert!(!r.typed);
    debug_assert!(r.state.open.len() <= UiState::MAX_OPEN);
}

/// Depth in the open chain of the popup that contains `node`, if any.
pub(crate) fn popup_depth(tables: &UiTables, state: &UiState, node: NodeIx) -> Option<usize> {
    assert!(
        (node as usize) < tables.nodes.len(),
        "node index out of range"
    );
    let parent = tables.nodes.parent[node as usize];
    if parent == NO_NODE {
        return None;
    }
    debug_assert!(parent < node, "a parent precedes its children");
    let pid = tables.nodes.id[parent as usize];
    state.open.iter().position(|&m| m == pid)
}

/// True for a dropdown with choices (it opens a popup instead of running its command).
pub(crate) fn has_popup(tables: &UiTables, node: NodeIx) -> bool {
    assert!(
        (node as usize) < tables.nodes.len(),
        "node index out of range"
    );
    tables.nodes.kind[node as usize] == Kind::Dropdown
        && tables.nodes.children(node).next().is_some()
}

// ---- private ----

impl Router<'_> {
    /// True while a dialog is open: only its rows can be hit.
    fn modal(&self) -> bool {
        self.tables.nodes.find(self.state.dialog).is_some()
    }

    /// The topmost row under `(x, y)`.
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let l = self.layout;
        let hit = l.hit(x, y, self.modal()).map(|row| Hit {
            row,
            node: l.node[row],
            shown: l.shown[row],
        });
        debug_assert!(hit.is_none_or(|h| (h.node as usize) < self.tables.nodes.len()));
        hit
    }

    /// True when a hit is the canvas shown as itself.
    fn on_canvas(&self, h: Option<Hit>) -> bool {
        h.is_some_and(|h| {
            h.shown == Shown::Normal && self.tables.nodes.kind[h.node as usize] == Kind::Canvas
        })
    }

    /// How the focused node edits: as a text area, or as a single-line field (also when
    /// nothing is focused).
    fn focused_lines(&self) -> Lines {
        let nodes = &self.tables.nodes;
        let area = nodes
            .find(self.state.focus)
            .is_some_and(|n| nodes.kind[n as usize] == Kind::TextArea);
        if area { Lines::Multi } else { Lines::Single }
    }

    /// Ends the focused field's editing: its pending `input` command first.
    fn blur(&mut self) {
        field::flush_typed(self.tables, self.state, &mut self.typed, self.out);
        self.state.blur();
        self.out.redraw = true;
        debug_assert!(!self.typed);
        debug_assert!(self.state.focus.is_none());
    }
}
