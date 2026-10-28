//! Pointer routing: moves, presses, releases and the wheel in, hover, the open popup chain,
//! drags of dialogs and split dividers, field focus, commands and canvas events out.
//!
//! Contents: `Router::{pointer_move, pointer_down, pointer_up, wheel}` (to `route`)

use super::{Hit, Router};
use crate::field;
use crate::field::FieldPress;
use crate::input::{Button, InputEvent};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout;
use crate::layout::{SPLIT_GAP, SPLIT_MIN, Shown, UiLayout};
use crate::state::UiState;
use crate::strings::Sym;
use crate::tables::{NO_NODE, NodeIx, UiTables};

impl Router<'_> {
    /// Pointer motion: a dialog or divider drag, canvas capture, hover and menu tracking.
    pub(super) fn pointer_move(&mut self, ev: InputEvent, x: f32, y: f32) {
        debug_assert!(matches!(ev, InputEvent::PointerMove { .. }));
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
            Some(h) if interactive(nodes.kind[h.node as usize], h.shown) => ids[h.node as usize],
            _ => Sym::NONE,
        };
        if hover != state.hover {
            state.hover = hover;
            self.out.redraw = true;
        }
        match h {
            // Menu tracking: once a menubar menu is open, hovering moves the open chain (an
            // open dropdown does not follow the pointer onto the menubar).
            Some(Hit {
                node: n,
                shown: Shown::Title,
                ..
            }) if !state.open.is_empty()
                && state.open[0] != ids[n as usize]
                && nodes
                    .find(state.open[0])
                    .is_some_and(|m| nodes.kind[m as usize] == Kind::Menu) =>
            {
                state.open.clear();
                state.open.push(ids[n as usize]);
                self.out.relayout = true;
            }
            Some(Hit {
                node: n, shown: s, ..
            }) if !state.open.is_empty() && matches!(s, Shown::Cascade | Shown::Normal) => {
                if let Some(d) = super::popup_depth(self.tables, state, n) {
                    let before = state.open.len();
                    state.open.truncate(d + 1);
                    if s == Shown::Cascade && state.open.len() < UiState::MAX_OPEN {
                        state.open.push(ids[n as usize]);
                    }
                    self.out.relayout |= state.open.len() != before || s == Shown::Cascade;
                }
            }
            Some(Hit {
                node: n,
                shown: Shown::Normal,
                ..
            }) if nodes.kind[n as usize] == Kind::Canvas => self.out.canvas.push(ev),
            _ => {}
        }
        debug_assert!(self.state.open.len() <= UiState::MAX_OPEN);
    }

    /// A button press: closes menus or a point dialog, starts a canvas capture, a dialog
    /// drag, or a [`Router::press`].
    pub(super) fn pointer_down(&mut self, ev: InputEvent, x: f32, y: f32, button: Button) {
        debug_assert!(matches!(ev, InputEvent::PointerDown { .. }));
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
            field::flush_typed(self.tables, self.state, &mut self.typed, self.out);
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
        let Some(hit) = h else { return };
        if button != Button::Left {
            return;
        }
        let n = hit.node;
        let i = n as usize;
        let id = nodes.id[i];
        let field = matches!(nodes.kind[i], Kind::TextInput | Kind::TextArea);
        let refocus = field && self.state.focus == id;
        if !refocus && !self.state.focus.is_none() {
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
        self.press(hit, x, y);
    }

    /// A button release: ends a dialog or divider drag, or a canvas capture.
    pub(super) fn pointer_up(&mut self, ev: InputEvent) {
        debug_assert!(matches!(ev, InputEvent::PointerUp { .. }));
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
        debug_assert!(self.state.dialog_grab.is_none());
    }

    /// The wheel: the canvas, a text area taller than its box, or the scroll pane under it.
    pub(super) fn wheel(&mut self, ev: InputEvent, x: f32, y: f32, dy: f32) {
        debug_assert!(matches!(ev, InputEvent::Wheel { .. }));
        let tables = self.tables;
        let nodes = &tables.nodes;
        let h = self.hit(x, y);
        if self.on_canvas(h) && self.state.open.is_empty() {
            self.out.canvas.push(ev);
            return;
        }
        // A text area taller than its box scrolls inside itself (not the pane).
        if let Some(Hit {
            row: r,
            node: n,
            shown: Shown::Normal,
        }) = h
            && nodes.kind[n as usize] == Kind::TextArea
        {
            let max = layout::area_scroll_max(self.tables, self.layout, self.text, self.state, r);
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
        let pane = h.map(|h| {
            if nodes.kind[h.node as usize] == Kind::Scroll {
                h.node
            } else {
                self.layout.scroller(h.row)
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
}

// ---- private ----

impl Router<'_> {
    /// A left press on a hit row that no gesture above took.
    fn press(&mut self, hit: Hit, x: f32, y: f32) {
        debug_assert!(hit.row < self.layout.len());
        debug_assert_eq!(
            self.layout.node[hit.row], hit.node,
            "the hit reads its own row"
        );
        let tables = self.tables;
        let (nodes, state, out) = (&tables.nodes, &mut *self.state, &mut *self.out);
        let n = hit.node;
        let i = n as usize;
        let id = nodes.id[i];
        match (nodes.kind[i], hit.shown) {
            (_, Shown::Title) => {
                let reopen = state.open.first() != Some(&id);
                state.open.clear();
                if reopen {
                    state.open.push(id);
                }
                out.relayout = true;
            }
            (_, Shown::Cascade) => {
                if let Some(d) = super::popup_depth(self.tables, state, n) {
                    state.open.truncate(d + 1);
                    state.open.push(id);
                    out.relayout = true;
                }
            }
            (Kind::Dropdown, Shown::Normal) if super::has_popup(self.tables, n) => {
                toggle_popup(self.tables, state, n);
                out.relayout = true;
            }
            (Kind::Split, Shown::Normal) => {
                // Inside a split only its divider is not covered by a pane.
                state.split_drag = id;
                out.redraw = true;
            }
            (Kind::TextInput | Kind::TextArea, _) => {
                let press = FieldPress { row: hit.row, x, y };
                field::focus_field(
                    self.tables,
                    self.layout,
                    self.strings,
                    self.text,
                    state,
                    press,
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
    assert!(!id.is_none(), "a divider drag is in progress");
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
    debug_assert!(min <= total - min, "the clamp range is not empty");
    let px = (far - pos - gap / 2.0).clamp(min, total - min);
    state.set_split(id, px);
    debug_assert!(px >= 0.0);
    debug_assert!(px <= total);
}

/// Opens or closes dropdown `n`'s popup: a dropdown inside an open popup keeps the chain above
/// it, anywhere else it starts a new chain.
fn toggle_popup(tables: &UiTables, state: &mut UiState, n: NodeIx) {
    debug_assert!(
        super::has_popup(tables, n),
        "only a dropdown with choices toggles a popup"
    );
    let id = tables.nodes.id[n as usize];
    let reopen = state.open.last() != Some(&id);
    match super::popup_depth(tables, state, n) {
        Some(d) => state.open.truncate(d + 1),
        None => state.open.clear(),
    }
    if reopen && state.open.len() < UiState::MAX_OPEN {
        state.open.push(id);
    }
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}
