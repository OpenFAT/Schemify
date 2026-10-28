//! Input routing: keys, menus, clicks and canvas events, modal and point-shown dialogs, split
//! dividers, dialog dragging.
//!
//! Contents: no `pub` items.

use schemify_ui::{Button, Chord, InputEvent, InputEvents, Layer, SPLIT_GAP, SPLIT_MIN};

use crate::{Fixture, VIEW, fixture};

// ---- private ----

#[test]
fn routing_keys_menus_clicks_and_canvas() {
    let mut f = fixture(&[]);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+S").unwrap(),
    });
    ev.push(InputEvent::Key {
        chord: Chord::parse("Ctrl+Alt+Shift+F12").unwrap(),
    });
    let out = f.route(&ev);
    assert_eq!(
        f.commands(&out),
        ["file.save"],
        "bound chord runs, unbound is ignored"
    );

    let out = f.click("menu.file");
    assert!(out.commands.is_empty());
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    // Hover tracking moves the open menu along the bar.
    let (x, y) = f.center("menu.edit");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    f.route(&ev);
    assert_eq!(f.state.open(), [f.sym("menu.edit")]);
    // Clicking an item runs it and closes the menu.
    let undo_row = f
        .frame
        .rows(Layer::Overlay)
        .find(|&r| f.frame.node()[r] == f.node("edit.undo"))
        .unwrap();
    let rr = f.frame.rect()[undo_row];
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: rr.x + 5.0,
        y: rr.y + 5.0,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["edit.undo"]);
    assert!(f.state.open().is_empty());
    // Escape closes an open menu without running a command.
    f.click("menu.view");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Esc").unwrap(),
    });
    let out = f.route(&ev);
    assert!(out.commands.is_empty() && f.state.open().is_empty());
    // Toolbar button and canvas forwarding (with drag capture outside the canvas).
    let out = f.click("tb.wire");
    assert_eq!(f.commands(&out), ["tools.insert_wire"]);
    let (cx, cy) = f.center("canvas");
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
    ev.push(InputEvent::Wheel {
        x: cx,
        y: cy,
        dx: 0.0,
        dy: 1.0,
    });
    ev.push(InputEvent::Wheel {
        x: 5.0,
        y: 5.0,
        dx: 0.0,
        dy: 1.0,
    });
    let out = f.route(&ev);
    assert_eq!(
        out.canvas.len(),
        4,
        "drag captured; wheel only over the canvas"
    );
}

#[test]
fn dialog_is_modal_and_escape_closes_it() {
    let mut f = fixture(&[]);
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.relayout();
    let d = f.frame.rect()[f.frame.floats().end];
    assert!(d.x > 0.0 && d.w > 300.0, "centered dialog");
    assert!(
        f.click("switch.symbol").commands.is_empty(),
        "a float over the canvas is dead under a modal dialog"
    );
    assert!(
        f.click("tb.wire").commands.is_empty(),
        "main window is blocked"
    );
    let out = f.click("dialog.enter_text.cancel");
    assert_eq!(f.commands(&out), ["dialog.close"]);
    let mut ev = InputEvents::default();
    ev.push(InputEvent::Key {
        chord: Chord::parse("Escape").unwrap(),
    });
    f.route(&ev);
    assert!(f.state.dialog().is_none());
}

/// Split dividers: a drag on the gap sizes the second pane by the pointer travel (the first
/// takes the rest, nothing else sees the drag), clamped to `SPLIT_MIN`; presses elsewhere do
/// not drag.
#[test]
fn split_divider_drags_the_second_pane() {
    let mut f = fixture(&[]);
    let r = |f: &Fixture, s: &str| f.frame.rect()[f.row(s)];
    let (canvas, docs) = (r(&f, "canvas"), r(&f, "docs.pane"));
    let (gx, y) = (canvas.x + canvas.w + SPLIT_GAP / 2.0, canvas.y + 40.0);
    let drag = |f: &mut Fixture, from: f32, to: f32| {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerDown {
            x: from,
            y,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerMove { x: to, y });
        ev.push(InputEvent::PointerUp {
            x: to,
            y,
            button: Button::Left,
        });
        f.route(&ev)
    };
    let out = drag(&mut f, gx, gx - 200.0);
    assert!(
        out.commands.is_empty() && out.canvas.is_empty(),
        "the divider eats the drag"
    );
    let (c2, d2) = (r(&f, "canvas"), r(&f, "docs.pane"));
    assert!(
        (d2.w - (docs.w + 200.0)).abs() <= 1.0,
        "{} vs {}",
        d2.w,
        docs.w
    );
    let cell = r(&f, "cell.pane").w;
    assert!(
        (c2.w + SPLIT_GAP + d2.w + SPLIT_GAP + cell - VIEW.0).abs() <= 1.0,
        "the first pane takes the rest; a third pane keeps its width"
    );
    // After the release a move drags nothing.
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x: 10.0, y });
    f.route(&ev);
    assert_eq!(r(&f, "docs.pane").w, d2.w);
    // Clamped: the first pane keeps SPLIT_MIN.
    let gx = c2.x + c2.w + SPLIT_GAP / 2.0;
    drag(&mut f, gx, 0.0);
    assert!((r(&f, "canvas").w - SPLIT_MIN).abs() <= 1.0);
    // A press on the canvas is the canvas's.
    let (cx, _) = f.center("canvas");
    let out = drag(&mut f, cx, cx + 5.0);
    assert_eq!(out.canvas.len(), 3);
    assert!((r(&f, "canvas").w - SPLIT_MIN).abs() <= 1.0);
}

/// A dialog shown at a point (the context menu) closes on a click outside it, running
/// nothing; a click inside keeps it; a centered dialog stays modal.
#[test]
fn context_menu_closes_on_a_click_outside() {
    let mut f = fixture(&[]);
    f.state
        .set_dialog_at(f.sym("dialog.ctx"), Some((300.0, 200.0)));
    f.relayout();
    let d = f.frame.rect()[f.frame.floats().end];
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: d.x + d.w / 2.0,
        y: d.y + d.h / 2.0,
        button: Button::Left,
    });
    f.route(&ev);
    assert_eq!(f.state.dialog(), f.sym("dialog.ctx"), "inside keeps it");
    let (x, y) = f.center("tb.wire");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x,
        y,
        button: Button::Right,
    });
    let out = f.route(&ev);
    assert!(f.state.dialog().is_none() && out.commands.is_empty() && out.canvas.is_empty());
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.relayout();
    assert!(f.click("tb.wire").commands.is_empty());
    assert_eq!(
        f.state.dialog(),
        f.sym("dialog.enter_text"),
        "centered: modal"
    );
}

/// A centered dialog moves by dragging its own surface (its title, frame or a plain label),
/// not its controls; it stays inside the viewport, stays modal (a click outside neither
/// closes it nor reaches the UI below), and the next dialog opens centered again (#46).
#[test]
fn a_dialog_drags_by_its_surface_and_stays_modal() {
    let mut f = fixture(&[]);
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.relayout();
    let at = |f: &Fixture| f.frame.rect()[f.frame.floats().end];
    let d0 = at(&f);
    let drag = |f: &mut Fixture, (x, y): (f32, f32), (dx, dy): (f32, f32)| {
        let mut ev = InputEvents::default();
        ev.push(InputEvent::PointerDown {
            x,
            y,
            button: Button::Left,
        });
        ev.push(InputEvent::PointerMove {
            x: x + dx,
            y: y + dy,
        });
        ev.push(InputEvent::PointerUp {
            x: x + dx,
            y: y + dy,
            button: Button::Left,
        });
        f.route(&ev)
    };
    let c = f.center("dialog.enter_text.title");
    let out = drag(&mut f, c, (-100.0, 60.0));
    assert!(out.commands.is_empty());
    let d1 = at(&f);
    assert!(
        (d1.x - (d0.x - 100.0)).abs() < 0.5 && (d1.y - (d0.y + 60.0)).abs() < 0.5,
        "{d0:?} → {d1:?}"
    );
    // Modal: a click outside closes nothing and runs nothing.
    assert!(f.click("tb.wire").commands.is_empty());
    assert_eq!(f.state.dialog(), f.sym("dialog.enter_text"));
    // A press on a control does not move the dialog.
    let c = f.center("dialog.enter_text.input");
    drag(&mut f, c, (200.0, 200.0));
    assert_eq!(at(&f), d1);
    // Far off-screen: kept inside the viewport.
    let c = f.center("dialog.enter_text.title");
    drag(&mut f, c, (-5000.0, -5000.0));
    let d2 = at(&f);
    assert_eq!((d2.x, d2.y), (0.0, 0.0));
    // The next dialog opens centered.
    f.state.set_dialog(schemify_ui::Sym::NONE);
    f.state.set_dialog(f.sym("dialog.enter_text"));
    f.relayout();
    assert_eq!(at(&f), d0);
}
