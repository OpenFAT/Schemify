//! The editing loop end to end, headless: scripted input batches through the real frame
//! spine (route_input → dispatch → doc::apply_edits → compose → scene), checked against
//! the resulting `doc` records. Oracle: the canonical record text of the edited document,
//! and undo restoring it byte for byte (one undo per gesture).

mod common;
use common::{Session, U, lines};

use schemify_app::editor::Tool;
use schemify_doc::TableId;

const SHIFT: u8 = schemify_ui::Mods::SHIFT;

fn wires(s: &Session) -> Vec<String> {
    lines(&s.text(), "wire ")
        .into_iter()
        .map(|l| l.split_whitespace().skip(2).collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn draws_a_wire_and_an_l_wire_one_undo_per_gesture() {
    let mut s = Session::open("inv", "top.shcomp", "wire", true);
    let before = s.text();
    s.move_to((300 * U, 100 * U));
    s.key("w");
    assert!(matches!(s.ed.tool, Tool::Wire { .. }));
    s.move_to((400 * U, 120 * U));
    s.click((400 * U, 120 * U));
    assert!(
        wires(&s).contains(&"300 100 400 120".to_owned()),
        "{:?}",
        wires(&s)
    );
    // Space cycles Manhattan mode: horizontal first, stored as two segments (xschem).
    s.move_to((300 * U, -60 * U));
    s.key("w");
    s.key("Space");
    s.click((400 * U, -20 * U));
    let w = wires(&s);
    assert!(w.contains(&"300 -60 400 -60".to_owned()), "{w:?}");
    assert!(w.contains(&"400 -60 400 -20".to_owned()), "{w:?}");
    assert_eq!(w.len(), 9 + 3);
    let after = s.text();
    s.key("u");
    assert_eq!(wires(&s).len(), 9 + 1, "the L wire is one transaction");
    s.key("u");
    assert_eq!(s.text(), before, "undo restores the file exactly");
    s.key("Shift+U");
    s.key("Shift+U");
    assert_eq!(s.text(), after, "redo replays both gestures");
    // Esc aborts a wire in progress: nothing is written.
    s.key("w");
    s.key("Esc");
    s.click((450 * U, 40 * U));
    assert_eq!(s.text(), after);
}

#[test]
fn places_moves_rotates_and_flips_an_instance() {
    let mut s = Session::open("inv", "top.shcomp", "place", true);
    s.move_to((300 * U, -100 * U));
    s.key("Ins");
    s.click_node("dialog.choose.filter");
    s.type_text("nmos");
    s.click_node("dialog.choose.row.0");
    assert!(matches!(s.ed.tool, Tool::Place { .. }), "{:?}", s.ed.tool);
    s.move_to((300 * U, -100 * U));
    s.click((300 * U, -100 * U));
    let t = s.text();
    assert!(t.contains("dev:nmos\n  at 300 -100 R0\n  name M1\n"), "{t}");
    assert_eq!(s.ed.selection().len(), 1, "the placed instance is selected");
    // m: grab at the pointer, click to drop (one transaction).
    s.key("m");
    s.move_to((340 * U, -60 * U));
    s.click((340 * U, -60 * U));
    assert!(
        s.text().contains("dev:nmos\n  at 340 -60 R0\n"),
        "{}",
        s.text()
    );
    // Shift+R rotates the selection 90° clockwise about its (snapped) center.
    s.key("Shift+R");
    assert!(
        s.text().contains("dev:nmos\n  at 340 -60 R90\n"),
        "{}",
        s.text()
    );
    // Shift+F flips it horizontally.
    s.key("Shift+F");
    assert!(
        s.text().contains("dev:nmos\n  at 340 -60 MR270\n"),
        "{}",
        s.text()
    );
    // Four gestures, four undos back to the original file.
    for _ in 0..4 {
        s.key("u");
    }
    assert!(!s.text().contains("dev:nmos"));
}

#[test]
fn drag_moves_and_shift_drag_copies() {
    let mut s = Session::open("inv", "top.shcomp", "drag", true);
    // x2 sits at (140, 0); its body spans the triangle right of the origin.
    s.drag((130 * U, 10 * U), (130 * U, 110 * U), 0);
    let t = s.text();
    assert!(t.contains("./inv\n  at 140 100 R0\n  name x2\n"), "{t}");
    // Shift-drag on the (selected) instance copies it with a fresh name.
    s.drag((130 * U, 110 * U), (230 * U, 110 * U), SHIFT);
    let t = s.text();
    assert!(t.contains("  at 140 100 R0\n  name x2\n"), "{t}");
    assert!(t.contains("  at 240 100 R0\n  name x3\n"), "{t}");
    assert_eq!(lines(&t, "inst ").len(), 3);
}

#[test]
fn box_select_delete_and_undo() {
    let mut s = Session::open("inv", "top.shcomp", "box", true);
    let before = s.text();
    // Left-to-right box around the VSS rail and its label: fully-inside objects only.
    s.drag((-20 * U, 50 * U), (160 * U, 95 * U), 0);
    let sel = s.ed.selection().to_vec();
    assert!(!sel.is_empty());
    assert!(
        sel.iter()
            .all(|k| matches!(k.table, TableId::Wires | TableId::Labels)),
        "{sel:?}"
    );
    s.key("Del");
    let t = s.text();
    assert!(!t.contains("label n00003"), "{t}");
    assert!(!t.contains("wire w00009"), "{t}");
    assert!(t.contains("wire w00004"), "objects outside the box stay");
    s.key("u");
    assert_eq!(s.text(), before);
}

#[test]
fn duplicate_cut_and_paste() {
    let mut s = Session::open("inv", "top.shcomp", "dup", true);
    s.click((10 * U, 0)); // x1
    assert_eq!(s.ed.selection().len(), 1);
    s.move_to((0, 0));
    s.key("c");
    s.move_to((0, 120 * U));
    s.click((0, 120 * U));
    let t = s.text();
    assert!(
        t.contains("  at 0 120 R0\n  name x3\n  wp 4\n"),
        "copies keep overrides: {t}"
    );
    // Ctrl+X then Ctrl+V: the clipboard follows the pointer, a click drops it.
    s.key("Ctrl+X");
    assert!(!s.text().contains("name x3"));
    s.move_to((300 * U, 100 * U));
    s.key("Ctrl+V");
    s.click((300 * U, 100 * U));
    assert!(s.text().contains("name x3\n  wp 4\n"), "{}", s.text());
}

#[test]
fn descend_edit_return_keeps_per_document_undo() {
    let mut s = Session::open("inv", "top.shcomp", "hier", true);
    let top = s.text();
    s.click((10 * U, 0)); // x1
    s.key("e");
    let (child, _) = s.ed.active().unwrap();
    assert!(s.ed.session.name(child).starts_with("inv"));
    assert_eq!(s.ed.session.hier_path(), ".x1.");
    let inv = s.text();
    assert!(inv.contains("dev:pmos"));
    // Delete the "CMOS inverter" text in the child.
    s.click((-50 * U, -145 * U));
    s.key("Del");
    assert!(lines(&s.text(), "text ").is_empty(), "{}", s.text());
    s.key("Ctrl+E");
    assert_eq!(s.ed.session.hier_path(), ".");
    assert_eq!(s.text(), top, "the parent is untouched");
    // Back down: the child's undo stack survived the navigation (D28).
    s.click((10 * U, 0));
    s.key("e");
    s.key("u");
    assert_eq!(s.text(), inv);
    // i opens the symbol side of the instance's cell.
    s.key("Ctrl+E");
    s.click((10 * U, 0));
    s.key("i");
    let (_, view) = s.ed.active().unwrap();
    assert_eq!(view, schemify_app::session::View::Symbol);
    assert!(
        s.ed.scene()
            .objs
            .key
            .iter()
            .any(|k| k.table == TableId::Pins)
    );
}

#[test]
fn properties_dialog_writes_only_overrides() {
    let mut s = Session::open("inv", "top.shcomp", "props", true);
    s.click((150 * U, 0)); // x2: no overrides
    s.key("q");
    // Rows: name, wp (default 2), wn (default 1).
    s.click_node("dialog.props.val.1");
    s.type_text("6");
    s.click_node("dialog.props.ok");
    let t = s.text();
    assert!(t.contains("  name x2\n  wp 6\n"), "{t}");
    assert!(
        !t.contains("wn "),
        "untouched params stay defaults (D40): {t}"
    );
    // Clearing an override removes it; renaming edits `name`.
    s.click((10 * U, 0)); // x1: wp 4
    s.key("q");
    s.click_node("dialog.props.val.1");
    for _ in 0..4 {
        s.key("BackSpace");
    }
    s.click_node("dialog.props.val.0");
    s.key("End");
    s.type_text("a");
    s.click_node("dialog.props.ok");
    let t = s.text();
    assert!(
        t.contains("  at 0 0 R0\n  name x1a\n\n") || t.contains("  name x1a\ninst"),
        "{t}"
    );
    assert!(!t.contains("wp 4"), "{t}");
    s.key("u");
    assert!(
        s.text().contains("  name x1\n  wp 4\n"),
        "one undo per dialog OK"
    );
}

#[test]
fn escape_cancels_dialogs_and_ok_keeps_unsubmitted_typing() {
    let mut s = Session::open("inv", "top.shcomp", "esc", true);
    let before = s.text();
    s.click((150 * U, 0));
    s.key("q");
    s.type_text("zzz");
    s.key("Esc"); // blurs the focused field
    s.key("Esc"); // closes the dialog: cancel
    assert!(s.ed.shell.ui.dialog().is_none());
    assert_eq!(s.text(), before, "cancel writes nothing");
    s.move_to((300 * U, 100 * U));
    s.key("w");
    assert!(
        matches!(s.ed.tool, Tool::Wire { .. }),
        "editing keys work again"
    );
    s.key("Esc");
    // Typing then clicking OK (no Enter) still uses the typed text.
    s.key("t");
    s.type_text("note");
    s.click_node("dialog.enter_text.ok");
    s.click((300 * U, 60 * U));
    assert!(s.text().contains(" 300 60 R0 note\n"), "{}", s.text());
}

#[test]
fn labels_ports_and_texts_from_the_text_dialog() {
    let mut s = Session::open("inv", "top.shcomp", "label", true);
    s.move_to((300 * U, -40 * U));
    s.key("Alt+L");
    s.type_text("net7");
    s.key("Enter");
    s.click((300 * U, -40 * U));
    assert!(s.text().contains(" 300 -40 R0 net7\n"), "{}", s.text());
    s.key("Ctrl+P");
    s.type_text("EN");
    s.key("Enter");
    s.click((300 * U, -80 * U));
    assert!(
        lines(&s.text(), "port ")
            .iter()
            .any(|l| l.ends_with(" 300 -80 R0 EN"))
    );
    s.key("t");
    s.type_text("hello world");
    s.key("Enter");
    s.click((300 * U, -120 * U));
    assert!(
        s.text().contains(" 300 -120 R0 \"hello world\"\n"),
        "{}",
        s.text()
    );
}

#[test]
fn save_writes_the_canonical_file_and_clears_dirty() {
    let mut s = Session::open("inv", "top.shcomp", "save", true);
    s.click((150 * U, 0));
    s.key("Del");
    let (h, _) = s.ed.active().unwrap();
    assert!(s.ed.session.project.is_dirty(h));
    s.key("Ctrl+S");
    assert!(!s.ed.session.project.is_dirty(h));
    let on_disk = std::fs::read_to_string(s.dir.join("top.shcomp")).unwrap();
    assert_eq!(on_disk, s.text());
    assert!(!on_disk.contains("name x2"));
    let journal = s.dir.join(".schemify/journal/top.shcomp.journal");
    assert!(journal.exists(), "edits are journaled (D28)");
}

#[test]
fn zoom_pan_and_snap_keys() {
    let mut s = Session::open("inv", "top.shcomp", "view", true);
    let c0 = s.ed.camera();
    s.key("Shift+Z");
    assert!(s.ed.camera().scale > c0.scale);
    s.key("Ctrl+Z");
    s.key("Ctrl+Z");
    assert!(s.ed.camera().scale < c0.scale);
    s.key("f");
    assert!(
        (s.ed.camera().scale - c0.scale).abs() < 1e-9,
        "f fits again"
    );
    s.key("Left");
    assert!(s.ed.camera().cx < c0.cx);
    assert_eq!(s.ed.opts.snap, 10 * U);
    s.key("g");
    assert_eq!(s.ed.opts.snap, 5 * U);
    s.key("Shift+G");
    s.key("Shift+G");
    assert_eq!(s.ed.opts.snap, 20 * U);
    s.key("%");
    assert!(!s.ed.opts.show_grid);
}

#[test]
fn tabs_open_new_and_switch() {
    let mut s = Session::open("inv", "top.shcomp", "tabs", true);
    s.key("Ctrl+N");
    assert_eq!(s.ed.session.tabs.len(), 2);
    assert_eq!(s.ed.session.active, 1);
    s.key("Ctrl+Left");
    assert_eq!(s.ed.session.active, 0);
    s.click_node("tab.1");
    assert_eq!(s.ed.session.active, 1);
    // Ctrl+O lists cells; opening one adds a tab.
    s.key("Ctrl+O");
    s.click_node("dialog.choose.filter");
    s.type_text("pmos");
    s.click_node("dialog.choose.row.0");
    assert_eq!(s.ed.session.tabs.len(), 3);
    let (_, view) = s.ed.active().unwrap();
    assert_eq!(
        view,
        schemify_app::session::View::Symbol,
        "a .shprim opens on its symbol"
    );
}
