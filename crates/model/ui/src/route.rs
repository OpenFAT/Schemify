//! Input routing (spine: `ui::route_input`): one frame's event table in, `command_requests`,
//! canvas events and interaction-state changes out.
//!
//! The UI never edits `ui_nodes` in response to input. Clicking a checkbox emits its command;
//! whoever owns the checked state answers with a `set … checked` contribution. Only transient
//! interaction state lives in [`UiState`], keyed by stable node ids so it survives
//! recomposition. Pointer and wheel events are routed here; keys on an open popup go to
//! [`crate::menu`], keys and text on a focused field to [`crate::field`].
//!
//! Contents:
//! - `pub fn route_input(tables, layout, strings, text, state, events, out)`
//! - `pub(crate) fn popup_depth(tables, state, node) -> Option<usize>`
//! - `pub(crate) fn has_popup(tables, node) -> bool`

use crate::field::{edit_key, flush_typed, focus_field, follow_caret};
use crate::input::{Button, InputEvent, InputEvents, Routed};
use crate::keys::{Chord, Key, Mods};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::{SPLIT_GAP, SPLIT_MIN, Shown, UiLayout, area_scroll_max};
use crate::menu::{menu_key, open_menubar};
use crate::state::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{NO_NODE, NodeIx, UiTables};
use crate::text::TextSystem;

/// One [`route_input`] call: its read-only inputs, the state and output it writes, and
/// whether the focused field's text changed since its last `input` command.
struct Router<'a> {
    tables: &'a UiTables,
    layout: &'a UiLayout,
    strings: &'a Strings,
    text: &'a mut TextSystem,
    state: &'a mut UiState,
    out: &'a mut Routed,
    typed: bool,
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
    flush_typed(r.tables, r.state, &mut r.typed, r.out);
    debug_assert!(!r.typed);
    debug_assert!(r.state.open.len() <= UiState::MAX_OPEN);
}

/// Depth in the open chain of the popup that contains `node`, if any.
pub(crate) fn popup_depth(tables: &UiTables, state: &UiState, node: NodeIx) -> Option<usize> {
    let parent = tables.nodes.parent[node as usize];
    if parent == NO_NODE {
        return None;
    }
    let pid = tables.nodes.id[parent as usize];
    state.open.iter().position(|&m| m == pid)
}

/// True for a dropdown with choices (it opens a popup instead of running its command).
pub(crate) fn has_popup(tables: &UiTables, node: NodeIx) -> bool {
    tables.nodes.kind[node as usize] == Kind::Dropdown
        && tables.nodes.children(node).next().is_some()
}

// ---- private ----

impl Router<'_> {
    /// True while a dialog is open: only its rows can be hit.
    fn modal(&self) -> bool {
        self.tables.nodes.find(self.state.dialog).is_some()
    }

    /// The topmost row under `(x, y)`: (row, node, how it is shown).
    fn hit(&self, x: f32, y: f32) -> Option<(usize, NodeIx, Shown)> {
        let l = self.layout;
        l.hit(x, y, self.modal())
            .map(|r| (r, l.node[r], l.shown[r]))
    }

    /// True when a hit is the canvas shown as itself.
    fn on_canvas(&self, h: Option<(usize, NodeIx, Shown)>) -> bool {
        matches!(h, Some((_, n, Shown::Normal)) if self.tables.nodes.kind[n as usize] == Kind::Canvas)
    }

    /// True when the focused node is a text area.
    fn focused_area(&self) -> bool {
        let nodes = &self.tables.nodes;
        nodes
            .find(self.state.focus)
            .is_some_and(|n| nodes.kind[n as usize] == Kind::TextArea)
    }

    /// Ends the focused field's editing: its pending `input` command first.
    fn blur(&mut self) {
        flush_typed(self.tables, self.state, &mut self.typed, self.out);
        self.state.blur();
        self.out.redraw = true;
    }

    fn pointer_move(&mut self, ev: InputEvent, x: f32, y: f32) {
        let tables = self.tables;
        let (nodes, state) = (&tables.nodes, &mut *self.state);
        if let Some((gx, gy)) = state.dialog_grab {
            state.dialog_pos = Some((x - gx, y - gy));
            self.out.relayout = true;
            return;
        }
        if !state.split_drag.is_none() {
            drag_split(self.tables, self.layout, state, x, y);
            self.out.relayout = true;
            return;
        }
        if state.capture {
            self.out.canvas.push(ev);
            return;
        }
        let h = self.hit(x, y);
        let (ids, state) = (nodes.id(), &mut *self.state);
        let hover = match h {
            Some((_, n, s)) if interactive(nodes.kind[n as usize], s) => ids[n as usize],
            _ => Sym::NONE,
        };
        if hover != state.hover {
            state.hover = hover;
            self.out.redraw = true;
        }
        match h {
            // Menu tracking: once a menubar menu is open, hovering moves the open chain (an
            // open dropdown does not follow the pointer onto the menubar).
            Some((_, n, Shown::Title))
                if !state.open.is_empty()
                    && state.open[0] != ids[n as usize]
                    && nodes
                        .find(state.open[0])
                        .is_some_and(|m| nodes.kind[m as usize] == Kind::Menu) =>
            {
                state.open.clear();
                state.open.push(ids[n as usize]);
                self.out.relayout = true;
            }
            Some((_, n, s))
                if !state.open.is_empty() && matches!(s, Shown::Cascade | Shown::Normal) =>
            {
                if let Some(d) = popup_depth(self.tables, state, n) {
                    let before = state.open.len();
                    state.open.truncate(d + 1);
                    if s == Shown::Cascade && state.open.len() < UiState::MAX_OPEN {
                        state.open.push(ids[n as usize]);
                    }
                    self.out.relayout |= state.open.len() != before || s == Shown::Cascade;
                }
            }
            Some((_, n, Shown::Normal)) if nodes.kind[n as usize] == Kind::Canvas => {
                self.out.canvas.push(ev)
            }
            _ => {}
        }
    }

    fn pointer_down(&mut self, ev: InputEvent, x: f32, y: f32, button: Button) {
        let tables = self.tables;
        let nodes = &tables.nodes;
        let h = self.hit(x, y);
        let on_canvas = self.on_canvas(h);
        let modal = self.modal();
        if !self.state.open.is_empty() && (h.is_none() || on_canvas || button != Button::Left) {
            self.state.open.clear(); // click outside closes menus and is consumed (Tk)
            self.out.relayout = true;
            return;
        }
        if modal && h.is_none() && self.state.dialog_at.is_some() {
            // A dialog at a point (the context menu) closes like a menu.
            flush_typed(self.tables, self.state, &mut self.typed, self.out);
            self.state.set_dialog(Sym::NONE);
            self.out.relayout = true;
            return;
        }
        if on_canvas {
            self.state.capture = true;
            if !self.state.focus.is_none() {
                self.blur();
            }
            self.out.canvas.push(ev);
            return;
        }
        let Some((r, n, shown)) = h else { return };
        if button != Button::Left {
            return;
        }
        let i = n as usize;
        let id = nodes.id[i];
        let field = matches!(nodes.kind[i], Kind::TextInput | Kind::TextArea);
        if !(field && self.state.focus == id) && !self.state.focus.is_none() {
            self.blur();
            self.out.relayout = true; // a text_area shrinks back to its value
        }
        // A press on a centered dialog's own surface (its frame, a panel or a plain label, not
        // a control) grabs the dialog: dragging moves it.
        let surface = match nodes.kind[i] {
            Kind::Dialog | Kind::Panel => true,
            Kind::Label => nodes.prop(Prop::Command)[i].is_none(),
            _ => false,
        };
        if modal && surface && self.state.dialog_at.is_none() {
            let d = self.layout.rect[self.layout.floats().end];
            debug_assert_eq!(
                self.layout.node[self.layout.floats().end],
                nodes.find(self.state.dialog).unwrap_or(NO_NODE)
            );
            self.state.dialog_grab = Some((x - d.x, y - d.y));
            return;
        }
        self.press(r, n, shown, x, y);
    }

    /// A left press on row `r` (node `n` shown as `shown`) that no gesture above took.
    fn press(&mut self, r: usize, n: NodeIx, shown: Shown, x: f32, y: f32) {
        let tables = self.tables;
        let (nodes, state, out) = (&tables.nodes, &mut *self.state, &mut *self.out);
        let i = n as usize;
        let id = nodes.id[i];
        match (nodes.kind[i], shown) {
            (_, Shown::Title) => {
                let reopen = state.open.first() != Some(&id);
                state.open.clear();
                if reopen {
                    state.open.push(id);
                }
                out.relayout = true;
            }
            (_, Shown::Cascade) => {
                if let Some(d) = popup_depth(self.tables, state, n) {
                    state.open.truncate(d + 1);
                    state.open.push(id);
                    out.relayout = true;
                }
            }
            (Kind::Dropdown, Shown::Normal) if has_popup(self.tables, n) => {
                toggle_popup(self.tables, state, n);
                out.relayout = true;
            }
            (Kind::Split, Shown::Normal) => {
                // Inside a split only its divider is not covered by a pane.
                state.split_drag = id;
                out.redraw = true;
            }
            (Kind::TextInput | Kind::TextArea, _) => {
                focus_field(
                    self.tables,
                    self.layout,
                    self.strings,
                    self.text,
                    state,
                    r,
                    x,
                    y,
                );
                out.redraw = true;
            }
            (
                Kind::Item
                | Kind::Button
                | Kind::Tab
                | Kind::Checkbox
                | Kind::Dropdown
                | Kind::Prose
                | Kind::Label,
                _,
            ) => {
                let cmd = nodes.prop(Prop::Command)[i];
                if !cmd.is_none() && !nodes.is_disabled(self.strings, n) {
                    out.commands.push(cmd, id, "");
                    out.relayout |= !state.open.is_empty();
                    state.open.clear();
                }
            }
            _ => {}
        }
    }

    fn pointer_up(&mut self, ev: InputEvent) {
        let state = &mut *self.state;
        if state.dialog_grab.take().is_some() {
            self.out.redraw = true;
        } else if !state.split_drag.is_none() {
            state.split_drag = Sym::NONE;
            self.out.redraw = true;
        } else if state.capture {
            state.capture = false;
            self.out.canvas.push(ev);
        }
    }

    fn wheel(&mut self, ev: InputEvent, x: f32, y: f32, dy: f32) {
        let tables = self.tables;
        let nodes = &tables.nodes;
        let h = self.hit(x, y);
        if self.on_canvas(h) && self.state.open.is_empty() {
            self.out.canvas.push(ev);
            return;
        }
        // A text area taller than its box scrolls inside itself (not the pane).
        if let Some((r, n, Shown::Normal)) = h
            && nodes.kind[n as usize] == Kind::TextArea
        {
            let max = area_scroll_max(self.tables, self.layout, self.text, self.state, r);
            if max > 0.0 {
                let id = nodes.id[n as usize];
                let now = self.state.scroll_of(id);
                self.state.set_scroll(id, now - dy * UiState::WHEEL_PX, max);
                self.out.redraw |= self.state.scroll_of(id) != now;
                return;
            }
        }
        // Over a scroll pane (or anything inside one): scroll it. Wheel away from the user
        // (positive `dy`) moves the content down, like every text view.
        let pane = h.map(|(r, n, _)| {
            if nodes.kind[n as usize] == Kind::Scroll {
                n
            } else {
                self.layout.scroller(r)
            }
        });
        if let Some(s) = pane.filter(|&s| s != NO_NODE)
            && let Some(max) = self.layout.scroll_max(s)
        {
            let id = nodes.id[s as usize];
            let now = self.state.scroll_of(id);
            self.state.set_scroll(id, now - dy * UiState::WHEEL_PX, max);
            self.out.relayout |= self.state.scroll_of(id) != now;
        }
    }

    fn key(&mut self, chord: Chord) {
        let modal = self.modal();
        if self.state.editor.is_some() {
            let area = self.focused_area();
            let (tables, out) = (self.tables, &mut *self.out);
            edit_key(
                tables,
                self.text,
                self.state,
                chord,
                area,
                &mut self.typed,
                out,
            );
            out.redraw = true;
            out.relayout |= area; // the field grows and shrinks with its lines
            if area {
                follow_caret(tables, self.layout, self.text, self.state);
            }
        } else if !self.state.open.is_empty() {
            // An open popup has the keyboard: no key binding runs under it.
            menu_key(self.tables, self.strings, self.state, chord, self.out);
            self.out.relayout = true;
            self.out.redraw = true;
        } else if chord == Chord::new(Mods::default(), Key::ESCAPE) && modal {
            self.state.dialog = Sym::NONE;
            self.state.dialog_at = None;
            self.out.relayout = true;
        } else if let Some(cmd) = self.tables.keymap.lookup(chord) {
            self.out.commands.push(cmd, Sym::NONE, "");
        } else if chord == Chord::new(Mods::default(), Key::f(10))
            && !modal
            && open_menubar(self.tables, self.strings, self.state)
        {
            // F10 (when nothing binds it) opened the first menubar menu.
            self.out.relayout = true;
            self.out.redraw = true;
        }
    }

    /// Committed text: inserted into the focused field.
    fn text_in(&mut self, s: &str) {
        let Some(ed) = self.state.editor.as_mut() else {
            return;
        };
        let mut d = ed.driver(&mut self.text.font_cx, &mut self.text.layout_cx);
        d.clear_compose();
        d.insert_or_replace_selection(s);
        d.refresh_layout();
        self.typed = true;
        self.out.redraw = true;
        let area = self.focused_area();
        self.out.relayout |= area;
        if area {
            follow_caret(self.tables, self.layout, self.text, self.state);
        }
    }

    /// IME preedit: shown in the focused field (an empty one clears it).
    fn preedit(&mut self, s: &str) {
        let Some(ed) = self.state.editor.as_mut() else {
            return;
        };
        let mut d = ed.driver(&mut self.text.font_cx, &mut self.text.layout_cx);
        if s.is_empty() {
            d.clear_compose();
        } else {
            d.set_compose(s, None);
        }
        d.refresh_layout();
        self.out.redraw = true;
    }

    /// Assistive-technology click: opens a menu or dropdown, else runs the node's command.
    fn activate(&mut self, node: Sym) {
        let tables = self.tables;
        let (nodes, state) = (&tables.nodes, &mut *self.state);
        let Some(n) = nodes.find(node) else { return };
        let cmd = nodes.prop(Prop::Command)[n as usize];
        if nodes.kind[n as usize] == Kind::Menu || has_popup(self.tables, n) {
            match popup_depth(self.tables, state, n) {
                Some(d) => state.open.truncate(d + 1),
                None => state.open.clear(),
            }
            if state.open.len() < UiState::MAX_OPEN {
                state.open.push(node);
            }
            self.out.relayout = true;
        } else if !cmd.is_none() && !nodes.is_disabled(self.strings, n) {
            self.out.commands.push(cmd, node, "");
        }
    }
}

/// True when a row shown as `shown` of `kind` reacts to the pointer (and so hovers).
fn interactive(kind: Kind, shown: Shown) -> bool {
    shown != Shown::Normal
        || matches!(
            kind,
            Kind::Item
                | Kind::Button
                | Kind::Tab
                | Kind::Checkbox
                | Kind::Dropdown
                | Kind::TextInput
                | Kind::TextArea
        )
}

/// Moves the divider of the dragged `split` to the pointer: the second pane takes the space
/// from the pointer to its own far edge (later panes keep their size), leaving each pane at
/// least [`SPLIT_MIN`].
fn drag_split(tables: &UiTables, layout: &UiLayout, state: &mut UiState, x: f32, y: f32) {
    let id = state.split_drag;
    let Some(r) = tables.nodes.find(id).and_then(|n| layout.row_of(n)) else {
        state.split_drag = Sym::NONE; // the split went away mid-drag
        return;
    };
    let n = layout.node[r];
    let style = |p| layout.style(tables, n, p);
    let gap = style(StyleProp::Gap).max(SPLIT_GAP);
    let rect = layout.rect[r];
    let horiz = style(StyleProp::Dir) != 0.0;
    let (start, end, pos, pad) = if horiz {
        (rect.x, rect.x + rect.w, x, style(StyleProp::Pad))
    } else {
        (rect.y, rect.y + rect.h, y, style(StyleProp::PadY))
    };
    // The second pane ends at its own far edge: panes after it keep their size.
    let second = tables
        .nodes
        .children(n)
        .nth(1)
        .and_then(|c| layout.row_of(c))
        .map(|s| layout.rect[s]);
    let far = match second {
        Some(s) if horiz => s.x + s.w,
        Some(s) => s.y + s.h,
        None => end - pad,
    };
    let total = (far - (start + pad) - gap).max(0.0);
    let min = SPLIT_MIN.min(total / 2.0);
    let px = (far - pos - gap / 2.0).clamp(min, total - min);
    state.set_split(id, px);
    debug_assert!(px >= 0.0 && px <= total);
}

/// Opens or closes dropdown `n`'s popup: a dropdown inside an open popup keeps the chain above
/// it, anywhere else it starts a new chain.
fn toggle_popup(tables: &UiTables, state: &mut UiState, n: NodeIx) {
    let id = tables.nodes.id[n as usize];
    let reopen = state.open.last() != Some(&id);
    match popup_depth(tables, state, n) {
        Some(d) => state.open.truncate(d + 1),
        None => state.open.clear(),
    }
    if reopen && state.open.len() < UiState::MAX_OPEN {
        state.open.push(id);
    }
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}
