//! `text_input` and `text_area`: egui text edits over the node's `value`. While focused, a field
//! edits its own buffer in [`crate::UiState`] (the owner may not echo every keystroke); each
//! change runs its `input` command with the text, Enter (Ctrl+Enter in a text area) runs its
//! `command`. Escape drops the buffer.
//!
//! Contents: crate-only `field(v, ui, n)`

use egui::{Margin, TextEdit, Ui, vec2};

use super::{View, color, font};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::AREA_EM_MAX;
use crate::tables::NodeIx;

/// Draws field `n` and routes its edits.
pub(crate) fn field(v: &mut View, ui: &mut Ui, n: NodeIx) {
    let id = v.id(n);
    let multi = v.kind(n) == Kind::TextArea;
    let editing = v.st.edit.as_ref().is_some_and(|e| e.0 == id);
    let mut buf = match v.st.edit.take() {
        Some((e, b)) if e == id => b,
        other => {
            v.st.edit = other;
            v.prop(n, Prop::Value).to_owned()
        }
    };
    let px = v.font(n);
    let margin = Margin::symmetric(
        v.style(n, StyleProp::Pad).min(127) as i8,
        v.style(n, StyleProp::PadY).min(127) as i8,
    );
    let width = match v.px(n, StyleProp::Width) {
        w if w > 0.0 => w,
        _ => ui.available_width(),
    };
    let edit = if multi { TextEdit::multiline(&mut buf) } else { TextEdit::singleline(&mut buf) };
    let edit = edit
        .id(v.egui_id(n))
        .font(font(px))
        .text_color(color(v.style(n, StyleProp::Fg)))
        .background_color(color(v.style(n, StyleProp::Bg)))
        .margin(margin)
        .desired_width(width)
        .interactive(!v.t.nodes.is_disabled(v.s, n));
    let r = if multi {
        // A fixed height is the area's; else it grows with its lines up to AREA_EM_MAX.
        let fixed = v.px(n, StyleProp::Height);
        let (max_h, min_h) = if fixed > 0.0 { (fixed, fixed) } else { (px * AREA_EM_MAX, 0.0) };
        egui::ScrollArea::vertical()
            .id_salt(v.egui_id(n).with("area"))
            .max_height(max_h)
            .min_scrolled_height(min_h)
            .show(ui, |ui| ui.add(edit))
            .inner
    } else {
        ui.add_sized(vec2(width, ui.spacing().interact_size.y.max(px + 8.0)), edit)
    };
    if v.st.focus_next == id {
        r.request_focus();
        v.st.focus_next = crate::strings::Sym::NONE;
    }
    if r.changed() {
        let text = buf.clone();
        v.run(n, Prop::Input, &text);
    }
    let (enter, ctrl) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.ctrl));
    if r.has_focus() {
        if multi && enter && ctrl {
            let text = buf.clone();
            v.run(n, Prop::Command, &text);
        }
        v.st.edit = Some((id, buf));
    } else if r.lost_focus() && enter && !multi {
        v.run(n, Prop::Command, &buf);
    } else if editing {
        debug_assert!(v.st.edit.is_none(), "the buffer was taken above");
    }
    v.record(n, r.rect, None);
}
