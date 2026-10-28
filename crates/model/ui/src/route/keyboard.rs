//! Keyboard routing: chords, committed text and IME preedit in, field edits, popup navigation,
//! key bindings and assistive-technology activation out.
//!
//! Contents: `Router::{key, text_in, preedit, activate}` (to `route`)

use super::Router;
use crate::field;
use crate::field::Lines;
use crate::keys::{Chord, Key, Mods};
use crate::kinds::{Kind, Prop};
use crate::menu;
use crate::state::UiState;
use crate::strings::Sym;

impl Router<'_> {
    /// A key chord: the focused field, an open popup, Escape on a dialog, the keymap, F10.
    pub(super) fn key(&mut self, chord: Chord) {
        let modal = self.modal();
        if self.state.editor.is_some() {
            let lines = self.focused_lines();
            let (tables, out) = (self.tables, &mut *self.out);
            field::edit_key(
                tables,
                self.text,
                self.state,
                chord,
                lines,
                &mut self.typed,
                out,
            );
            out.redraw = true;
            // A text area grows and shrinks with its lines.
            out.relayout |= lines == Lines::Multi;
            if lines == Lines::Multi {
                field::follow_caret(tables, self.layout, self.text, self.state);
            }
        } else if !self.state.open.is_empty() {
            // An open popup has the keyboard: no key binding runs under it.
            menu::menu_key(self.tables, self.strings, self.state, chord, self.out);
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
            && menu::open_menubar(self.tables, self.strings, self.state)
        {
            // F10 (when nothing binds it) opened the first menubar menu.
            self.out.relayout = true;
            self.out.redraw = true;
        }
        debug_assert!(self.state.open.len() <= UiState::MAX_OPEN);
    }

    /// Committed text: inserted into the focused field.
    pub(super) fn text_in(&mut self, s: &str) {
        let Some(ed) = self.state.editor.as_mut() else {
            return;
        };
        let mut d = ed.driver(&mut self.text.font_cx, &mut self.text.layout_cx);
        d.clear_compose();
        d.insert_or_replace_selection(s);
        d.refresh_layout();
        self.typed = true;
        self.out.redraw = true;
        let area = self.focused_lines() == Lines::Multi;
        self.out.relayout |= area;
        if area {
            field::follow_caret(self.tables, self.layout, self.text, self.state);
        }
        debug_assert!(
            self.state.editor.is_some(),
            "typing keeps the field focused"
        );
    }

    /// IME preedit: shown in the focused field (an empty one clears it).
    pub(super) fn preedit(&mut self, s: &str) {
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
        debug_assert!(
            self.state.editor.is_some(),
            "a preedit keeps the field focused"
        );
    }

    /// Assistive-technology click: opens a menu or dropdown, else runs the node's command.
    pub(super) fn activate(&mut self, node: Sym) {
        let tables = self.tables;
        let (nodes, state) = (&tables.nodes, &mut *self.state);
        let Some(n) = nodes.find(node) else { return };
        let cmd = nodes.prop(Prop::Command)[n as usize];
        if nodes.kind[n as usize] == Kind::Menu || super::has_popup(self.tables, n) {
            match super::popup_depth(self.tables, state, n) {
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
        debug_assert!(self.state.open.len() <= UiState::MAX_OPEN);
    }
}
