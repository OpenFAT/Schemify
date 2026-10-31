//! What the view shows and routes: the shell's bars and canvas, keys, menus, clicks, canvas
//! events with drag capture, modal and point-shown dialogs, split dividers.
//!
//! Contents: no `pub` items.

use schemify_ui::{Button, InputEvent, InputEvents, SPLIT_MIN};

use crate::VIEW;

// ---- private ----

#[test]
fn shell_stacks_bars_and_the_canvas_takes_the_rest() {
    let f = crate::fixture(&[]);
    let (bar, canvas) = (f.rect("menubar"), f.rect("canvas"));
    assert!(
        bar.y < 1.0 && bar.w > VIEW.0 - 2.0,
        "the menubar spans the top: {bar:?}"
    );
    assert!(
        canvas.y > bar.y + bar.h - 0.5,
        "the canvas sits below the bars"
    );
    assert!(
        canvas.h > VIEW.1 * 0.6,
        "the canvas grows into the free space: {canvas:?}"
    );
    let status = f.rect("status");
    assert!(
        (status.y + status.h - VIEW.1).abs() < 1.5,
        "the status bar ends at the bottom: {status:?}"
    );
    let canvas_row = f.frame.row_of(f.node("canvas")).unwrap();
    assert!(
        f.frame.slot()[canvas_row].is_some(),
        "the canvas reserves a paint slot"
    );
}

#[test]
fn keys_run_bindings_unless_unbound() {
    let mut f = crate::fixture(&[]);
    let out = crate::key(&mut f, "Ctrl+S");
    assert_eq!(f.commands(&out), ["file.save"]);
    let out = crate::key(&mut f, "Ctrl+Alt+Shift+F12");
    assert!(out.commands.is_empty(), "an unbound chord runs nothing");
    let out = crate::key(&mut f, "w");
    assert_eq!(
        f.commands(&out),
        ["tools.insert_wire"],
        "a printable key arrives as text"
    );
}

#[test]
fn a_menu_opens_runs_an_item_and_closes() {
    let mut f = crate::fixture(&[]);
    let out = f.click("menu.edit");
    assert!(out.commands.is_empty(), "a title only opens its popup");
    assert!(
        f.frame.row_of(f.node("edit.undo")).is_some(),
        "the popup's items are shown"
    );
    let out = crate::key(&mut f, "Ctrl+S");
    assert!(
        out.commands.is_empty(),
        "no binding runs under an open menu"
    );
    let out = f.click("edit.undo");
    assert_eq!(f.commands(&out), ["edit.undo"]);
    assert!(
        f.frame.row_of(f.node("edit.undo")).is_none(),
        "the menu closed"
    );
}

#[test]
fn toolbar_clicks_and_canvas_events_with_capture() {
    let mut f = crate::fixture(&[]);
    let out = f.click("tb.wire");
    assert_eq!(f.commands(&out), ["tools.insert_wire"]);
    let (cx, cy) = f.center("canvas");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x: cx, y: cy });
    f.route(&ev);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: cx,
        y: cy,
        button: Button::Middle,
    });
    ev.push(InputEvent::PointerMove { x: 5.0, y: 5.0 });
    ev.push(InputEvent::PointerUp {
        x: 5.0,
        y: 5.0,
        button: Button::Middle,
    });
    let out = f.route(&ev);
    let kinds: Vec<_> = out
        .canvas
        .iter()
        .filter(|e| !matches!(e, InputEvent::Mods { .. }))
        .collect();
    assert_eq!(
        kinds.len(),
        3,
        "a drag started on the canvas is captured past its edge: {kinds:?}"
    );
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x: 5.0, y: 5.0 });
    ev.push(InputEvent::Wheel {
        x: 5.0,
        y: 5.0,
        dx: 0.0,
        dy: 1.0,
    });
    let out = f.route(&ev);
    assert!(
        out.canvas.is_empty(),
        "nothing over the menubar reaches the canvas: {:?}",
        out.canvas
    );
}

#[test]
fn a_dialog_is_modal_and_escape_closes_it() {
    let mut f = crate::fixture(&[]);
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.route(&InputEvents::default());
    f.route(&InputEvents::default());
    let d = f.rect("dialog.enter_text");
    assert!(d.x > 0.0 && d.x + d.w < VIEW.0, "centered: {d:?}");
    assert!(
        f.click("tb.wire").commands.is_empty(),
        "the window under a dialog takes no clicks"
    );
    crate::key(&mut f, "Escape");
    assert!(f.state.dialog().is_none());
}

#[test]
fn a_context_menu_closes_on_a_click_outside() {
    let ctx = "node dialog.tctx dialog\nnode ctx.a button\n  parent dialog.tctx\n  label A\n  command a.run\n";
    let mut f = crate::fixture(&[ctx]);
    f.state
        .set_dialog_at(f.sym("dialog.tctx"), Some((300.0, 300.0)));
    f.route(&InputEvents::default());
    f.route(&InputEvents::default());
    let out = f.click("ctx.a");
    assert_eq!(f.commands(&out), ["a.run"]);
    assert_eq!(f.state.dialog(), f.sym("dialog.tctx"), "inside keeps it");
    let out = f.click_at(1000.0, 700.0);
    assert!(f.state.dialog().is_none() && out.commands.is_empty());
}

#[test]
fn the_split_divider_drags_the_second_pane() {
    let mut f = crate::fixture(&[]);
    let docs = f.rect("docs.pane");
    let canvas = f.rect("canvas");
    let x = docs.x - (docs.x - (canvas.x + canvas.w)) / 2.0;
    let y = docs.y + docs.h / 2.0;
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    f.route(&ev);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x,
        y,
        button: Button::Left,
    });
    f.route(&ev);
    for k in 1..=4 {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerMove {
            x: x - 25.0 * k as f32,
            y,
        });
        f.route(&ev);
    }
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerUp {
        x: x - 100.0,
        y,
        button: Button::Left,
    });
    f.route(&ev);
    f.route(&InputEvents::default());
    let wider = f.rect("docs.pane");
    assert!(
        (wider.w - docs.w - 100.0).abs() < 2.0,
        "{docs:?} → {wider:?}"
    );
    let work = f.sym("work");
    assert!(f.state.split_of(work).is_some_and(|w| w >= SPLIT_MIN));
}
