//! Text-field editing: a press, an editing key or committed text in, the focused field's
//! editor (parley's `PlainEditor`, in [`UiState`]) and its `input`/`command` requests out.
//!
//! Contents:
//! - `pub(crate) struct FieldPress`, `pub(crate) enum Lines`
//! - `pub(crate) fn flush_typed(tables, state, typed, out)`
//! - `pub(crate) fn focus_field(tables, layout, strings, text, state, press)`
//! - `pub(crate) fn follow_caret(tables, layout, text, state)`
//! - `pub(crate) fn edit_key(tables, text, state, chord, lines, typed, out)`

use std::mem;

use parley::{FontFamily, PlainEditor, StyleProperty};

use crate::input::Routed;
use crate::keys::{Chord, Key, Mods};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout;
use crate::layout::UiLayout;
use crate::state::UiState;
use crate::strings::Strings;
use crate::tables::UiTables;
use crate::text::TextSystem;

/// A press that focuses a text field: the field's layout row and the pointer, in logical px.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FieldPress {
    pub(crate) row: usize,
    pub(crate) x: f32,
    pub(crate) y: f32,
}

/// What Enter and the vertical arrows do in the focused field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lines {
    /// A `text_input`: Enter submits.
    Single,
    /// A `text_area`: Enter breaks the line, Ctrl+Enter submits, Up and Down move by lines.
    Multi,
}

/// Pushes the focused field's `input` command with its text when an edit changed it since the
/// last push. Called before anything that blurs the field and once at the end of a frame.
pub(crate) fn flush_typed(tables: &UiTables, state: &UiState, typed: &mut bool, out: &mut Routed) {
    if !mem::take(typed) {
        return;
    }
    debug_assert!(!*typed);
    let (Some(n), Some(ed)) = (tables.nodes.find(state.focus), state.editor.as_ref()) else {
        return;
    };
    let before = out.commands.len();
    let cmd = tables.nodes.prop(Prop::Input)[n as usize];
    if !cmd.is_none() {
        out.commands.push(cmd, state.focus, ed.raw_text());
    }
    debug_assert!(
        out.commands.len() <= before + 1,
        "one input command per flush"
    );
}

/// Focuses the text field at the press's layout row and places the caret under the pointer.
pub(crate) fn focus_field(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &mut UiState,
    press: FieldPress,
) {
    assert!(press.row < layout.len(), "a press on a laid-out row");
    let n = layout.node[press.row];
    let i = n as usize;
    debug_assert!(
        matches!(tables.nodes.kind[i], Kind::TextInput | Kind::TextArea),
        "only text fields take focus"
    );
    let rect = layout.rect[press.row];
    let style = |p| layout.style(tables, n, p);
    let (pad, font) = (style(StyleProp::Pad), style(StyleProp::Font).max(8.0));
    let area = tables.nodes.kind[i] == Kind::TextArea;
    let top = if area { style(StyleProp::PadY) } else { 0.0 };
    let id = tables.nodes.id[i];
    if state.focus != id || state.editor.is_none() {
        let mut ed = PlainEditor::new(font);
        let family = FontFamily::from(text.family());
        ed.edit_styles()
            .insert(StyleProperty::FontFamily(family.into_owned()));
        if area {
            ed.set_width(Some((rect.w - 2.0 * pad).max(font)));
        }
        ed.set_text(strings.get(tables.nodes.prop(Prop::Value)[i]));
        state.editor = Some(ed);
        state.focus = id;
    }
    // A text area may be scrolled inside itself: the point is in its text's coordinates.
    let scrolled = if area { state.scroll_of(id) } else { 0.0 };
    let ed = state.editor.as_mut().expect("editor just created");
    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
    d.move_to_point(press.x - rect.x - pad, press.y - rect.y - top + scrolled);
    d.refresh_layout();
    debug_assert_eq!(state.focus, id);
}

/// Scrolls the focused text area inside itself so the caret stays in view (after an edit or
/// a caret move). Uses the area's last laid-out box.
pub(crate) fn follow_caret(
    tables: &UiTables,
    layout: &UiLayout,
    text: &TextSystem,
    state: &mut UiState,
) {
    let Some(n) = tables.nodes.find(state.focus) else {
        return;
    };
    debug_assert_eq!(tables.nodes.kind[n as usize], Kind::TextArea);
    let (Some(r), Some(caret)) = (
        layout.row_of(n),
        state.editor.as_ref().and_then(|e| e.cursor_geometry(1.0)),
    ) else {
        return;
    };
    let pad_y = layout.style(tables, n, StyleProp::PadY);
    let view = layout.rect[r].h - 2.0 * pad_y;
    let max = layout::area_scroll_max(tables, layout, text, state, r);
    let now = state.scroll_of(state.focus);
    let (top, bottom) = (caret.y0 as f32, caret.y1 as f32);
    let want = if bottom > now + view {
        bottom - view
    } else if top < now {
        top
    } else {
        now
    };
    state.set_scroll(state.focus, want, max);
    debug_assert!(state.scroll_of(state.focus) <= max.max(0.0));
}

/// Applies an editing key to the focused field; Enter submits (Ctrl+Enter in a `text_area`,
/// where Enter breaks the line), Escape cancels. Sets `typed` when the key changed the text;
/// flushes the field's `input` command before the field loses focus.
pub(crate) fn edit_key(
    tables: &UiTables,
    text: &mut TextSystem,
    state: &mut UiState,
    chord: Chord,
    lines: Lines,
    typed: &mut bool,
    out: &mut Routed,
) {
    assert!(
        state.editor.is_some(),
        "keys edit the focused field's editor"
    );
    let area = lines == Lines::Multi;
    let mods = chord.mods().0;
    let shift = mods & Mods::SHIFT != 0;
    let word = mods & Mods::CTRL != 0;
    let key = chord.key();
    if area && key == Key::ENTER && !word {
        let ed = state.editor.as_mut().expect("caller checked editor");
        let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
        d.insert_or_replace_selection("\n");
        d.refresh_layout();
        *typed = true;
        return;
    }
    if area && (key == Key::UP || key == Key::DOWN) {
        let ed = state.editor.as_mut().expect("caller checked editor");
        let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
        match (key == Key::UP, shift) {
            (true, false) => d.move_up(),
            (true, true) => d.select_up(),
            (false, false) => d.move_down(),
            (false, true) => d.select_down(),
        }
        d.refresh_layout();
        return;
    }
    if key == Key::ENTER || key == Key::ESCAPE {
        flush_typed(tables, state, typed, out);
        if key == Key::ENTER
            && let (Some(n), Some(ed)) = (tables.nodes.find(state.focus), state.editor.as_ref())
        {
            let cmd = tables.nodes.prop(Prop::Command)[n as usize];
            if !cmd.is_none() {
                out.commands.push(cmd, state.focus, ed.raw_text());
            }
        }
        state.blur();
        debug_assert!(state.editor.is_none(), "submit and cancel end the editing");
        return;
    }
    let ed = state.editor.as_mut().expect("caller checked editor");
    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
    match key {
        Key::LEFT if shift => d.select_left(),
        Key::LEFT if word => d.move_word_left(),
        Key::LEFT => d.move_left(),
        Key::RIGHT if shift => d.select_right(),
        Key::RIGHT if word => d.move_word_right(),
        Key::RIGHT => d.move_right(),
        Key::HOME if shift => d.select_to_line_start(),
        Key::HOME => d.move_to_line_start(),
        Key::END if shift => d.select_to_line_end(),
        Key::END => d.move_to_line_end(),
        Key::BACKSPACE if word => d.backdelete_word(),
        Key::BACKSPACE => d.backdelete(),
        Key::DELETE if word => d.delete_word(),
        Key::DELETE => d.delete(),
        k if k == Key('a' as u32) && word => d.select_all(),
        _ => {} // printable keys arrive as Text events
    }
    d.refresh_layout();
    *typed |= matches!(key, Key::BACKSPACE | Key::DELETE);
}
