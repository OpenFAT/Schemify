//! Menus and dropdowns by pointer and keyboard.
//!
//! Contents: no `pub` items.

use schemify_ui::{Button, InputEvent, InputEvents, Layer, Shown};

use crate::{first_nav_row, fixture, key};

// ---- private ----

/// Menus by keyboard: F10 opens the menubar, arrows walk rows (wrapping, skipping
/// separators), cascades and the bar, Enter and mnemonics activate, Escape closes one level,
/// and no key binding runs under an open menu.
#[test]
fn menus_by_keyboard_arrows_enter_escape_and_mnemonics() {
    let mut f = fixture(&[]);
    key(&mut f, "F10");
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    assert_eq!(f.state.hover(), f.sym("file.clear_schematic"));
    key(&mut f, "Down");
    assert_eq!(f.state.hover(), f.sym("file.clear_symbol"));
    key(&mut f, "Up");
    key(&mut f, "Up");
    assert_eq!(f.state.hover(), f.sym("file.quit"), "wraps to the last row");
    assert!(key(&mut f, "Ctrl+S").commands.is_empty(), "bindings wait");
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    key(&mut f, "o");
    assert_eq!(
        f.state.hover(),
        f.sym("file.open"),
        "first of several O rows"
    );
    key(&mut f, "o");
    assert_eq!(
        f.state.hover(),
        f.sym("file.open_most_recent"),
        "the next one"
    );
    // "i" lands on the cascade (first of several I rows); Right opens it on its first row.
    key(&mut f, "i");
    assert_eq!(f.state.hover(), f.sym("menu.file.image_export"));
    key(&mut f, "Right");
    assert_eq!(
        f.state.open(),
        [f.sym("menu.file"), f.sym("menu.file.image_export")]
    );
    assert_eq!(f.state.hover(), first_nav_row(&f, "menu.file.image_export"));
    key(&mut f, "Left");
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    assert_eq!(f.state.hover(), f.sym("menu.file.image_export"));
    for _ in 0..3 {
        key(&mut f, "Down");
    }
    assert_eq!(f.state.hover(), f.sym("file.import_project.folder"));
    key(&mut f, "Down");
    assert_eq!(
        f.state.hover(),
        f.sym("file.close_schematic"),
        "separator skipped"
    );
    key(&mut f, "Right");
    assert_eq!(
        f.state.open(),
        [f.sym("menu.edit")],
        "the next menubar menu"
    );
    assert_eq!(f.state.hover(), first_nav_row(&f, "menu.edit"));
    key(&mut f, "Left");
    assert_eq!(f.state.open(), [f.sym("menu.file")]);
    let out = key(&mut f, "Enter");
    assert_eq!(f.commands(&out), ["file.clear_schematic"]);
    assert!(f.state.open().is_empty());
    key(&mut f, "F10");
    let out = key(&mut f, "m");
    assert_eq!(
        f.commands(&out),
        ["file.merge"],
        "a unique mnemonic runs its item"
    );
    assert!(f.state.open().is_empty());
    key(&mut f, "F10");
    key(&mut f, "i");
    key(&mut f, "Right");
    key(&mut f, "Escape");
    assert_eq!(
        f.state.open(),
        [f.sym("menu.file")],
        "Escape closes one level"
    );
    key(&mut f, "Escape");
    assert!(f.state.open().is_empty() && f.state.hover().is_none());
    // With every menu closed, bindings run again.
    let out = key(&mut f, "Ctrl+S");
    assert_eq!(f.commands(&out), ["file.save"]);
}

/// Dropdowns: a click opens the choices below the box, a choice runs its command and closes
/// the popup, a second click or a click elsewhere closes it, keys walk it like a menu, it works
/// inside a dialog (popup above the dialog), and a dropdown without choices runs its command.
#[test]
fn dropdowns_open_choose_close_and_work_in_dialogs() {
    let mut f = fixture(&[concat!(
        "node t.dd dropdown\n  parent toolbar\n  order -2\n  label Ngspice\n  command t.dd.plain\n",
        "node t.dd.a item\n  parent t.dd\n  label Ngspice\n  command t.pick.ngspice\n",
        "node t.dd.b item\n  parent t.dd\n  label Xyce\n  command t.pick.xyce\n",
        "node t.flat dropdown\n  parent toolbar\n  order -1\n  label Flat\n  command t.flat.run\n",
        "node t.dlg dialog\n  label Pick\n",
        "node t.dlg.dd dropdown\n  parent t.dlg\n  label A\n",
        "node t.dlg.dd.x item\n  parent t.dlg.dd\n  label X\n  command t.x\n",
    )]);
    let dd = f.frame.rect()[f.row("t.dd")];
    assert!(f.click("t.dd").commands.is_empty());
    assert_eq!(f.state.open(), [f.sym("t.dd")]);
    let popup = f
        .frame
        .rows(Layer::Overlay)
        .find(|&r| f.frame.shown()[r] == Shown::Popup)
        .unwrap();
    let p = f.frame.rect()[popup];
    assert_eq!((p.x, p.y), (dd.x, dd.y + dd.h), "drops below the box");
    let b = f
        .frame
        .rows(Layer::Overlay)
        .find(|&r| f.frame.node()[r] == f.node("t.dd.b"))
        .unwrap();
    let rb = f.frame.rect()[b];
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: rb.x + 4.0,
        y: rb.y + 4.0,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["t.pick.xyce"]);
    assert!(f.state.open().is_empty());
    f.click("t.dd");
    assert!(f.click("t.dd").commands.is_empty(), "a second click closes");
    assert!(f.state.open().is_empty());
    f.click("t.dd");
    key(&mut f, "Down");
    key(&mut f, "Down");
    let out = key(&mut f, "Enter");
    assert_eq!(f.commands(&out), ["t.pick.xyce"]);
    f.click("t.dd");
    let (cx, cy) = f.center("canvas");
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: cx,
        y: cy,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert!(
        f.state.open().is_empty() && out.canvas.is_empty(),
        "closed, click consumed"
    );
    let out = f.click("t.flat");
    assert_eq!(f.commands(&out), ["t.flat.run"], "no choices: a command");
    // Inside a dialog.
    f.state.set_dialog(f.sym("t.dlg"));
    f.relayout();
    f.click("t.dlg.dd");
    assert_eq!(f.state.open(), [f.sym("t.dlg.dd")]);
    let overlay: Vec<usize> = f.frame.rows(Layer::Overlay).collect();
    let dialog_row = overlay
        .iter()
        .position(|&r| f.frame.node()[r] == f.node("t.dlg"))
        .unwrap();
    let popup_row = overlay
        .iter()
        .position(|&r| f.frame.shown()[r] == Shown::Popup)
        .unwrap();
    assert!(
        popup_row > dialog_row,
        "the popup paints and hits above the dialog"
    );
    let x = overlay
        .iter()
        .copied()
        .find(|&r| f.frame.node()[r] == f.node("t.dlg.dd.x"))
        .unwrap();
    let rx = f.frame.rect()[x];
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerDown {
        x: rx.x + 4.0,
        y: rx.y + 4.0,
        button: Button::Left,
    });
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["t.x"]);
    assert_eq!(f.state.dialog(), f.sym("t.dlg"), "the dialog stays");
}
