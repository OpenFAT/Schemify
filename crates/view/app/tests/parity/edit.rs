//! Edit parity: stretch and kissing moves, clipboard, search and replace, shapes, context
//! menus, wire mode, crosshair, choosers, crash recovery, rotated texts.

use std::path::Path;

use schemify_canvas::LAYER_DEAD;
use schemify_doc::TableId;
use schemify_editor::editor::{Editor, Options, Tool};
use schemify_ui::{InputEvent, InputEvents};

use super::{Lcg, SHIFT};
use crate::common::{self, Session, U, Ui};

/// Property (docs/dev/testing.md): after a kissing move (Shift+M) of any instance subset by any
/// grid offset, every pair of pins (instance pins and ports) connected before is still
/// connected — the kissing wires keep every contact. Stretch alone (Ctrl+M) keeps them too
/// here because every connection of the fixture runs through a wire.
#[test]
fn stretch_and_kissing_moves_keep_pins_connected() {
    let mut rng = Lcg(0x5eed);
    for case in 0..12 {
        let mut s = Session::open("inv", "top.shcomp", &format!("stretch{case}"), Ui::Default);
        let before = super::pin_nets(&s);
        let original = s.text();
        let picks = 1 + rng.below(3);
        let targets = [(10 * U, 0), (150 * U, 0)];
        let mut first = true;
        for (k, &t) in targets.iter().enumerate() {
            if picks & (1 << k) != 0 {
                s.click_mods(t, if first { 0 } else { SHIFT });
                first = false;
            }
        }
        let d = (
            (rng.below(9) as i32 - 4) * 10 * U,
            (rng.below(9) as i32 - 4) * 10 * U,
        );
        let grab = (500 * U, 300 * U);
        s.move_to(grab);
        s.key(if case % 2 == 0 { "Shift+M" } else { "Ctrl+M" });
        s.move_to((grab.0 + d.0, grab.1 + d.1));
        s.click((grab.0 + d.0, grab.1 + d.1));
        let after = super::pin_nets(&s);
        assert_eq!(before.len(), after.len());
        for a in &before {
            for b in &before {
                if a.1 == b.1 {
                    let ca = after.iter().find(|x| x.0 == a.0).unwrap().1;
                    let cb = after.iter().find(|x| x.0 == b.0).unwrap().1;
                    assert_eq!(ca, cb, "case {case} d {d:?}: {} and {} split", a.0, b.0);
                }
            }
        }
        if d != (0, 0) {
            s.key("u");
            assert_eq!(s.text(), original, "case {case}: one undo");
        }
    }
}

#[test]
fn stretch_moves_wire_ends_and_plain_moves_do_not() {
    let mut s = Session::open("inv", "top.shcomp", "stretchends", Ui::Default);
    s.click((150 * U, 0)); // x2: pins (100,0) (180,0) (140,-30) (140,30)
    s.move_to((150 * U, 0));
    s.key("Ctrl+M");
    s.click((150 * U, 100 * U));
    let t = s.text();
    assert!(t.contains("./inv\n  at 140 100 R0\n  name x2"), "{t}");
    assert!(
        t.contains("wire w00001 40 0 100 100\n"),
        "end on x2.IN followed: {t}"
    );
    assert!(t.contains("wire w00005 140 -70 140 70\n"), "{t}");
    s.key("u");
    // Plain move: wires stay; Options → enable stretch (`y`) makes `m` stretch.
    s.key("y");
    assert!(s.ed.opts().stretch);
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((150 * U, 100 * U));
    assert!(s.text().contains("wire w00001 40 0 100 100\n"));
}

/// Copy → paste equals duplicate: the same records at the same offset, ids included (both
/// draw from the same seeded generator).
#[test]
fn clipboard_copy_paste_equals_duplicate() {
    let pick = |s: &mut Session| {
        s.click((150 * U, 0)); // x2, anchor (140, 0)
        s.click_mods((30 * U, 70 * U), SHIFT); // the VSS rail wire, anchor (0, 70)
    };
    // The clipboard holds the records relative to the anchors' snapped centroid (70, 40); a
    // duplicate grabbed there and dropped where the paste lands moves by the same offset.
    let mut a = Session::open("inv", "top.shcomp", "clipa", Ui::Default);
    pick(&mut a);
    a.key("Ctrl+C");
    a.move_to((170 * U, 40 * U));
    a.key("Ctrl+V");
    a.click((170 * U, 40 * U));
    let mut b = Session::open("inv", "top.shcomp", "clipb", Ui::Default);
    pick(&mut b);
    b.move_to((70 * U, 40 * U));
    b.key("c");
    b.move_to((170 * U, 40 * U));
    b.click((170 * U, 40 * U));
    assert_eq!(a.text(), b.text());
    assert_ne!(
        a.text(),
        Session::open("inv", "top.shcomp", "clipc", Ui::Default).text()
    );
    // The fragment text round-trips the clipboard records.
    let recs: Vec<schemify_doc::RecordBuf> = {
        let (h, _) = a.ed.active().unwrap();
        let doc = a.ed.session().project.doc(h).unwrap();
        schemify_editor::edit::selected_records(doc, a.ed.selection())
            .into_iter()
            .map(|r| r.2)
            .collect()
    };
    let text = schemify_editor::clip::encode(&recs);
    let mut back = schemify_editor::clip::decode(&text).unwrap();
    let mut want = recs.clone();
    back.sort_by(|x, y| x.fields.cmp(&y.fields));
    want.sort_by(|x, y| x.fields.cmp(&y.fields));
    assert_eq!(back, want);
}

/// Property: replacing every match with a text that does not contain the value leaves the
/// search empty; one undo restores the file.
#[test]
fn search_replace_all_leaves_nothing_to_find() {
    let mut s = Session::open("inv", "top.shcomp", "search", Ui::Default);
    let original = s.text();
    let (h, _) = s.ed.active().unwrap();
    let queries = [
        ("net", "V", false),
        ("name", "x", false),
        ("*", "mid", true),
        ("cell", "./inv", true),
    ];
    for (field, value, exact) in queries {
        let q = schemify_editor::search::Query {
            field: field.into(),
            value: value.into(),
            exact,
        };
        let found = schemify_editor::search::find(s.ed.session().project.doc(h).unwrap(), &q);
        assert!(!found.is_empty(), "{field} {value}");
        // Through the dialog: field, value, replacement, exact.
        s.key("Ctrl+F");
        assert_eq!(super::dialog(&s), "dialog.props");
        for (i, v) in [field, value, "Q_", if exact { "true" } else { "false" }]
            .iter()
            .enumerate()
        {
            s.click_node(&format!("dialog.props.val.{i}"));
            for _ in 0..8 {
                s.key("BackSpace");
            }
            s.type_text(v);
        }
        s.click_node("dialog.props.ok");
        let left = schemify_editor::search::find(s.ed.session().project.doc(h).unwrap(), &q);
        assert!(left.is_empty(), "{field} {value}: {left:?}\n{}", s.text());
        s.key("u");
        assert_eq!(s.text(), original, "{field} {value}: one undo");
    }
    // An empty replacement selects instead.
    s.key("Ctrl+F");
    s.click_node("dialog.props.val.1");
    s.type_text("VDD");
    s.click_node("dialog.props.ok");
    assert_eq!(s.ed.selection().len(), 1, "{}", s.status());
    assert_eq!(s.ed.selection()[0].table, TableId::Labels);
}

#[test]
fn schematic_shapes_draw_and_undo() {
    let mut s = Session::open("inv", "top.shcomp", "shapes", Ui::Default);
    let original = s.text();
    s.move_to((300 * U, 0));
    s.key("l");
    s.click((340 * U, 0));
    assert!(s.text().contains(" line 300 0 340 0\n"), "{}", s.text());
    s.move_to((300 * U, 50 * U));
    s.key("r");
    s.click((340 * U, 80 * U));
    assert!(s.text().contains(" rect 300 50 340 80\n"));
    // Polygon: vertices, then Enter closes it.
    s.move_to((300 * U, 100 * U));
    s.key("p");
    s.click((340 * U, 100 * U));
    s.click((340 * U, 140 * U));
    s.key("Enter");
    assert!(
        s.text()
            .contains(" poly 300 100 340 100 340 140\n  closed true\n"),
        "{}",
        s.text()
    );
    // Circle: center and a point on it; arc: three points.
    s.move_to((400 * U, 0));
    s.key("Ctrl+Shift+C");
    s.click((420 * U, 0));
    assert!(s.text().contains(" circle 400 0 20\n"), "{}", s.text());
    s.move_to((500 * U, 0));
    s.key("Shift+C");
    s.click((500 * U, 20 * U));
    s.click((480 * U, 0));
    assert!(
        common::lines(&s.text(), "shape ")
            .iter()
            .any(|l| l.contains(" arc "))
    );
    // The current layer (Ctrl+7) colors new shapes.
    s.key("Ctrl+7");
    s.move_to((300 * U, 60 * U));
    s.key("l");
    s.click((340 * U, 60 * U));
    assert!(
        s.text().contains(" line 300 60 340 60\n  layer 7\n"),
        "{}",
        s.text()
    );
    assert_eq!(common::lines(&s.text(), "shape ").len(), 6);
    // Shapes are objects: selectable, deletable.
    s.click((320 * U, 60 * U));
    s.key("Del");
    assert_eq!(common::lines(&s.text(), "shape ").len(), 5);
    for _ in 0..7 {
        s.key("u");
    }
    assert_eq!(s.text(), original);
}

#[test]
fn symbol_view_draws_arcs_circles_and_polygons_as_graphics() {
    let mut s = Session::open("inv", "top.shcomp", "symgfx", Ui::Default);
    s.key("Ctrl+Shift+N");
    s.move_to((0, 0));
    s.key("Ctrl+Shift+C");
    s.click((10 * U, 0));
    s.move_to((0, 40 * U));
    s.key("p");
    s.click((20 * U, 40 * U));
    s.click((20 * U, 60 * U));
    s.click((0, 40 * U)); // the first vertex closes it
    let t = s.text();
    assert!(t.contains("circle 0 0 10\n"), "{t}");
    assert!(t.contains("poly 0 40 20 40 20 60\n  closed true\n"), "{t}");
    assert!(!t.contains("shape "), "a symbol has no rank-3 shapes");
}

#[test]
fn context_menu_variants_run_their_rows() {
    let mut s = Session::open("inv", "top.shcomp", "ctx", Ui::Default);
    super::right_click(&mut s, (200 * U, 40 * U), 0);
    assert_eq!(super::dialog(&s), "dialog.ctx");
    assert!(super::composed(&s, "dialog.ctx.row.13") && !super::composed(&s, "dialog.ctx.row.14"));
    s.click_node("dialog.ctx.row.6"); // Insert wire
    assert_eq!(super::dialog(&s), "");
    assert!(
        matches!(s.ed.tool(), Tool::Wire { .. }),
        "{:?}",
        s.ed.tool()
    );
    s.key("Esc");
    // With a selection: the selection variant; Delete selection deletes.
    s.click((150 * U, 0));
    super::right_click(&mut s, (150 * U, 0), 0);
    assert_eq!(super::dialog(&s), "dialog.ctx");
    s.click_node("dialog.ctx.row.5"); // Delete selection
    assert!(!s.text().contains("name x2"));
    // Esc closes the menu without running anything.
    super::right_click(&mut s, (200 * U, 40 * U), 0);
    s.key("Esc");
    assert_eq!(super::dialog(&s), "");
    // Shift+right click selects the wires connected to the object under the pointer.
    s.key("Esc");
    super::right_click(&mut s, (70 * U, -70 * U), SHIFT);
    assert!(
        s.ed.selection().len() >= 4,
        "the VDD rail: {:?}",
        s.ed.selection()
    );
    assert!(
        s.ed.selection()
            .iter()
            .all(|k| matches!(k.table, TableId::Wires | TableId::Labels))
    );
}

#[test]
fn persistent_wire_mode_chains_segments() {
    let mut s = Session::open("inv", "top.shcomp", "persist", Ui::Default);
    s.run("options.persistent_wire_line_place_command", "");
    s.move_to((300 * U, 0));
    s.key("w");
    s.click((340 * U, 0));
    s.click((340 * U, 40 * U));
    assert!(matches!(s.ed.tool(), Tool::Wire { .. }), "still drawing");
    s.key("Esc");
    let w = common::lines(&s.text(), "wire ").len();
    assert_eq!(w, 9 + 2);
    // Off again: one segment per `w`.
    s.run("options.persistent_wire_line_place_command", "");
    s.key("w");
    s.click((400 * U, 0));
    assert!(matches!(s.ed.tool(), Tool::Idle));
}

#[test]
fn crosshair_snap_cursor_and_axes_draw_in_the_overlay() {
    let mut s = Session::open("inv", "top.shcomp", "cross", Ui::Default);
    s.move_to((33 * U, 12 * U));
    let base = s.ed.overlay().lines.x0.len();
    s.key("Alt+X");
    s.move_to((30 * U, 10 * U));
    let o = s.ed.overlay();
    assert_eq!(o.lines.x0.len(), base + 2);
    assert!(
        o.lines.x0.contains(&(30 * U)),
        "vertical line at the snapped x"
    );
    s.run("view.show_hide.draw_grid_axes", "");
    assert_eq!(s.ed.overlay().lines.x0.len(), base + 4);
    s.key("Alt+Z");
    s.move_to((41 * U, U)); // near x1's OUT pin (40, 0)
    assert_eq!(s.ed.overlay().polygons.end.len(), 1, "the snap diamond");
    s.key("Alt+X");
    s.key("Alt+Z");
    s.run("view.show_hide.draw_grid_axes", "");
    s.move_to((30 * U, 10 * U));
    assert_eq!(s.ed.overlay().lines.x0.len(), base);
}

#[test]
fn chooser_lists_scroll_past_24_rows() {
    let mut s = Session::open("inv", "top.shcomp", "scroll", Ui::Default);
    s.key("Ctrl+O");
    assert!(
        super::composed(&s, "dialog.choose.row.25"),
        "more than 24 rows listed"
    );
    // The wheel over the list scrolls it.
    let sh = &s.ed.shell();
    let list = sh.strings().find("dialog.choose.list").unwrap();
    let n = sh.tables().nodes.find(list).unwrap();
    let r = sh.layout().rect()[sh.layout().row_of(n).unwrap()];
    let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    s.send([
        InputEvent::PointerMove { x, y },
        InputEvent::Wheel {
            x,
            y,
            dx: 0.0,
            dy: -3.0,
        },
    ]);
    assert!(s.ed.shell().ui().scroll_of(list) > 0.0, "scrolled");
}

/// D28 crash recovery asks first: a journal with unsaved edits opens the recover/discard
/// dialog; nothing is applied before the answer; discard leaves nothing to offer next time.
#[test]
fn crash_recovery_asks_to_recover_or_discard() {
    let dir = common::fixture("inv", "recover");
    // An editor on the fixture copy that does not delete the directory when it goes.
    let open = |dir: &Path| {
        let mut s = Session {
            caches: Default::default(),
            ed: Editor::new(Options {
                path: Some(dir.join("top.shcomp")),
                seed: 7,
                ..Default::default()
            })
            .unwrap(),
            now: 1,
            dir: dir.to_path_buf(),
        };
        s.frame(InputEvents::default());
        s
    };
    // A "crash": an edit is journaled, the editor vanishes without saving.
    let crash = |dir: &Path| {
        let mut s = open(dir);
        s.click((150 * U, 0));
        s.key("Del");
        let t = s.text();
        std::mem::forget(s);
        t
    };
    let original = std::fs::read_to_string(dir.join("top.shcomp")).unwrap();
    let edited = crash(&dir);
    assert_ne!(edited, original);
    let mut s = open(&dir);
    assert_eq!(super::dialog(&s), "dialog.recover", "{}", s.status());
    assert_eq!(s.text(), original, "nothing applied before the decision");
    s.click_node("dialog.recover.discard");
    assert_eq!(
        (super::dialog(&s), s.text()),
        (String::new(), original.clone())
    );
    std::mem::forget(s);
    let s = open(&dir);
    assert_eq!(
        super::dialog(&s),
        "",
        "nothing left to recover after a discard"
    );
    std::mem::forget(s);
    assert_eq!(crash(&dir), edited);
    let mut s = open(&dir);
    assert_eq!(super::dialog(&s), "dialog.recover");
    s.click_node("dialog.recover.ok");
    assert_eq!(s.text(), edited, "the unsaved edit is back");
    let (h, _) = s.ed.active().unwrap();
    assert!(s.ed.session().project.is_dirty(h), "recovered, not saved");
    drop(s); // removes the directory
}

#[test]
fn rotated_texts_draw_turned() {
    let mut s = Session::open("inv", "top.shcomp", "rot", Ui::Default);
    s.click((-60 * U, -125 * U)); // the annotation text
    assert_eq!(s.ed.selection()[0].table, TableId::Texts);
    s.key("Alt+R");
    assert!(
        s.text().contains("text t00001 -80 -130 R90 "),
        "{}",
        s.text()
    );
    let p = &s.ed.scene().prims.texts;
    let i = (0..p.x.len())
        .find(|&i| p.x[i] == -80 * U && p.y[i] == -130 * U && p.layer[i] != LAYER_DEAD)
        .expect("the text is drawn at its anchor");
    assert_eq!(p.rot[i], 1, "a quarter turn");
    // Its box turned with it: taller than wide now.
    let o = s.ed.scene().find(s.ed.selection()[0]).unwrap();
    let b = s.ed.scene().objs.bbox(o as usize);
    assert!(b[3] - b[1] > b[2] - b[0], "{b:?}");
}
