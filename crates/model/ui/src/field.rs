//! Text-field editing: a press, an editing key or committed text in, the focused field's
//! editor (parley's `PlainEditor`, in [`UiState`]) and its `input`/`command` requests out.
//!
//! Contents:
//! - `pub(crate) fn flush_typed(tables, state, typed, out)`
//! - `pub(crate) fn focus_field(tables, layout, strings, text, state, r, x, y)`
//! - `pub(crate) fn follow_caret(tables, layout, text, state)`
//! - `pub(crate) fn edit_key(tables, text, state, chord, area, typed, out)`

use parley::{FontFamily, PlainEditor, StyleProperty};

use crate::input::Routed;
use crate::keys::{Chord, Key, Mods};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::{UiLayout, area_scroll_max};
use crate::state::UiState;
use crate::strings::Strings;
use crate::tables::UiTables;
use crate::text::TextSystem;

/// Pushes the focused field's `input` command with its text when an edit changed it since the
/// last push. Called before anything that blurs the field and once at the end of a frame.
pub(crate) fn flush_typed(tables: &UiTables, state: &UiState, typed: &mut bool, out: &mut Routed) {
    if !std::mem::take(typed) {
        return;
    }
    let (Some(n), Some(ed)) = (tables.nodes.find(state.focus), state.editor.as_ref()) else {
        return;
    };
    let cmd = tables.nodes.prop(Prop::Input)[n as usize];
    if !cmd.is_none() {
        out.commands.push(cmd, state.focus, ed.raw_text());
    }
    debug_assert!(!*typed);
}

/// Focuses text field at layout row `r` and places the caret under `(x, y)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn focus_field(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &mut UiState,
    r: usize,
    x: f32,
    y: f32,
) {
    let n = layout.node[r];
    let i = n as usize;
    let rect = layout.rect[r];
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
    d.move_to_point(x - rect.x - pad, y - rect.y - top + scrolled);
    d.refresh_layout();
}

/// Scrolls the focused text area inside itself so the caret stays in view (after an edit or
/// a caret move). Uses the area's last laid-out box.
pub(crate) fn follow_caret(tables: &UiTables, layout: &UiLayout, text: &TextSystem, state: &mut UiState) {
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
    let max = area_scroll_max(tables, layout, text, state, r);
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
    area: bool,
    typed: &mut bool,
    out: &mut Routed,
) {
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
