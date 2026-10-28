//! Text fields and areas: editing with IME, `input` commands as typed, scrolling inside a
//! text area, the IME candidate area.
//!
//! Contents: no `pub` items.

use schemify_ui::{Button, Chord, InputEvent, InputEvents, Shown, StyleProp, UiState};

use crate::{fixture, key};

// ---- private ----

#[test]
fn text_field_editing_with_ime_submits_on_enter() {
    let mut f = fixture(&[]);
    f.click("status.snap");
    assert_eq!(f.state.focus(), f.sym("status.snap"));
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("End").unwrap(),
    });
    ev.push(InputEvent::Key {
        chord: Chord::parse("BackSpace").unwrap(),
    });
    ev.push_preedit("ｇ");
    ev.push_preedit("");
    ev.push_text("5");
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+S").unwrap(),
    }); // swallowed while editing
    ev.push(InputEvent::Key {
        chord: Chord::parse("Enter").unwrap(),
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["view.set_snap"]);
    assert_eq!(out.commands.arg(0), "15");
    assert!(f.state.focus().is_none() && f.state.editor().is_none());
}

/// A text area grows with its lines up to `AREA_EM_MAX` font sizes, then scrolls inside itself:
/// the wheel over it scrolls it, a click lands on the scrolled line, and caret moves keep the
/// caret in view. One that fits leaves the wheel alone.
#[test]
fn text_area_scrolls_inside_itself() {
    let long: Vec<String> = (0..60).map(|k| format!("line {k}")).collect();
    let src = format!(
        "node area.box text_area\n  parent window\n  value \"{}\"\nnode area.short text_area\n  parent window\n  value \"one\\ntwo\"\n",
        long.join("\\n")
    );
    let mut f = fixture(&[src.as_str()]);
    let (node, r) = (f.node("area.box"), f.row("area.box"));
    let style = |p| {
        let v = schemify_ui::row_style(&f.tables, f.frame.title_style(), node, Shown::Normal, p);
        v as f32
    };
    let (font, pad_y) = (style(StyleProp::Font).max(8.0), style(StyleProp::PadY));
    let rect = f.frame.rect()[r];
    assert!(
        rect.h <= font * schemify_ui::AREA_EM_MAX + 2.0 * pad_y + 0.5,
        "capped: {rect:?}"
    );
    let max = schemify_ui::area_scroll_max(&f.tables, &f.frame, &f.text, &f.state, r);
    assert!(max > 100.0, "60 lines overflow the box: {max}");
    let id = f.sym("area.box");
    let wheel = |x: f32, y: f32| {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::Wheel {
            x,
            y,
            dx: 0.0,
            dy: -2.0,
        });
        ev
    };
    let (cx, cy) = f.center("area.box");
    f.route(&wheel(cx, cy));
    assert_eq!(f.state.scroll_of(id), 2.0 * UiState::WHEEL_PX);
    // A click lands on the line under the pointer in the scrolled text.
    f.click("area.box");
    let caret = f.state.editor().unwrap().cursor_geometry(1.0).unwrap();
    let view = rect.h - 2.0 * pad_y;
    let off = f.state.scroll_of(id);
    assert!(caret.y0 as f32 >= off - 1.0 && caret.y1 as f32 <= off + view + 1.0);
    assert!(
        caret.y0 as f32 > 2.0 * UiState::WHEEL_PX,
        "below the scrolled-out lines"
    );
    let mut ev = InputEvents::default();
    for _ in 0..40 {
        ev.push(InputEvent::Key {
            chord: Chord::parse("Down").unwrap(),
        });
    }
    f.route(&ev);
    let caret = f.state.editor().unwrap().cursor_geometry(1.0).unwrap();
    let off = f.state.scroll_of(id);
    assert!(
        off > 2.0 * UiState::WHEEL_PX,
        "followed the caret down: {off}"
    );
    assert!(caret.y1 as f32 <= off + view + 1.0 && caret.y0 as f32 >= off - 1.0);
    // Negative space: an area that fits does not scroll.
    let short = f.row("area.short");
    assert_eq!(
        schemify_ui::area_scroll_max(&f.tables, &f.frame, &f.text, &f.state, short),
        0.0
    );
    let (sx, sy) = f.center("area.short");
    f.route(&wheel(sx, sy));
    assert_eq!(f.state.scroll_of(f.sym("area.short")), 0.0);
}

/// `input`: a field sends its text as typed, once per frame and only when the text changed,
/// before its Enter command and before it loses focus; a field without `input` sends nothing
/// until Enter (`text_field_editing_with_ime_submits_on_enter`).
#[test]
fn text_fields_send_their_input_command_as_typed() {
    let mut f = fixture(&[
        "node t.field text_input\n  parent status\n  value ab\n  command t.ok\n  input t.typed\n",
    ]);
    assert!(
        f.click("t.field").commands.is_empty(),
        "focusing sends nothing"
    );
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("End").unwrap(),
    });
    ev.push_text("c");
    ev.push_text("d");
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["t.typed"], "once per frame");
    assert_eq!(out.commands.arg(0), "abcd");
    assert_eq!(out.commands.node[0], f.sym("t.field"));
    assert!(
        key(&mut f, "Left").commands.is_empty(),
        "no change, no command"
    );
    let out = key(&mut f, "BackSpace");
    assert_eq!(f.commands(&out), ["t.typed"]);
    assert_eq!(out.commands.arg(0), "abd");
    let mut ev = InputEvents::default();
    ev.push_text("x");
    ev.push(InputEvent::Key {
        chord: Chord::parse("Enter").unwrap(),
    });
    let out = f.route(&ev);
    assert_eq!(
        f.commands(&out),
        ["t.typed", "t.ok"],
        "typing reaches the owner first"
    );
    assert_eq!((out.commands.arg(0), out.commands.arg(1)), ("abxd", "abxd"));
    assert!(f.state.focus().is_none());
    // Typing then clicking elsewhere in one batch: the text is sent before the blur.
    f.click("t.field");
    let (x, y) = f.center("tb.wire");
    let mut ev = InputEvents::default();
    ev.push_text("z");
    ev.push(InputEvent::PointerDown {
        x,
        y,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["t.typed", "tools.insert_wire"]);
    assert!(out.commands.arg(0).contains('z'));
}

/// The IME candidate area follows the focused field's caret and is gone without focus.
#[test]
fn ime_area_follows_the_caret() {
    let mut f = fixture(&[]);
    assert!(f.state.ime_area(&f.tables, &f.frame).is_none());
    f.click("status.snap");
    key(&mut f, "End");
    let field = f.frame.rect()[f.row("status.snap")];
    let a = f.state.ime_area(&f.tables, &f.frame).unwrap();
    assert!(a.w > 0.0 && a.h > 0.0);
    assert!(
        field.contains(a.x, a.y + a.h / 2.0),
        "starts in the field: {a:?} in {field:?}"
    );
    let mut ev = InputEvents::default();
    ev.push_text("1234");
    f.route(&ev);
    let b = f.state.ime_area(&f.tables, &f.frame).unwrap();
    assert!(
        b.x + b.w > a.x + a.w,
        "moves right with the caret: {a:?} -> {b:?}"
    );
    key(&mut f, "Escape");
    assert!(f.state.ime_area(&f.tables, &f.frame).is_none());
}
