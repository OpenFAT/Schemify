//! Tools parity: join/trim, break wires, align to grid.

use crate::common::{self, Session, U, Ui};

#[test]
fn join_trim_merges_cuts_and_is_idempotent() {
    let mut s = Session::open("inv", "top.shcomp", "trim", Ui::Default);
    // Two collinear touching wires and a duplicate.
    for (a, b) in [
        ((300, 0), (340, 0)),
        ((340, 0), (380, 0)),
        ((300, 0), (340, 0)),
    ] {
        s.move_to((a.0 * U, a.1 * U));
        s.key("w");
        s.click((b.0 * U, b.1 * U));
    }
    let before = super::pin_nets(&s);
    s.key("&");
    let w = super::owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 380 0")), "{w:?}");
    assert_eq!(w.len(), 9 + 1);
    assert_eq!(super::pin_nets(&s), before, "connectivity unchanged");
    // Idempotent: a second run changes nothing.
    let t = s.text();
    s.key("&");
    assert_eq!(s.text(), t);
    assert!(s.status().contains("already"));
    // A wire end on another wire's middle cuts it (T junction).
    s.move_to((340 * U, 0));
    s.key("w");
    s.click((340 * U, 40 * U));
    s.key("&");
    let w = super::owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 340 0")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 340 0 380 0")), "{w:?}");
}

#[test]
fn break_wires_at_pins_and_at_the_pointer() {
    let mut s = Session::open("inv", "top.shcomp", "break", Ui::Default);
    // A wire across x2's IN pin (100, 0) from (60, 0) to (120, 0)... w00001 ends there; draw
    // a long one through x1's VSS pin (0, 30): from (-20, 30) to (20, 30).
    s.move_to((-20 * U, 30 * U));
    s.key("w");
    s.click((20 * U, 30 * U));
    s.click((10 * U, 0)); // x1
    s.key("!");
    let w = super::owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" -20 30 0 30")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 0 30 20 30")), "{w:?}");
    // Ctrl+!: pieces running from pin to pin of the instance are removed.
    s.move_to((-40 * U, 0));
    s.key("w");
    s.click((40 * U, 0)); // IN to OUT straight through x1
    s.click((10 * U, -15 * U)); // x1's body, off the new wire
    s.key("Ctrl+!");
    assert!(
        !common::lines(&s.text(), "wire ")
            .iter()
            .any(|l| l.ends_with(" -40 0 40 0"))
    );
    // Alt+right click cuts the wire under the pointer at the snapped point.
    super::right_click(&mut s, (70 * U, 70 * U), schemify_ui::Mods::ALT);
    let w = super::owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 0 70 70 70")), "{w:?}");
    assert!(w.iter().any(|l| l.ends_with(" 70 70 140 70")), "{w:?}");
}

#[test]
fn align_to_grid_snaps_coordinates() {
    let mut s = Session::open("inv", "top.shcomp", "align", Ui::Default);
    s.run("view.set_snap_value", "1");
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((153 * U, 2 * U));
    assert!(
        s.text().contains("  at 143 2 R0\n  name x2"),
        "{}",
        s.text()
    );
    s.run("view.set_snap_value", "10");
    s.key("Alt+U");
    assert!(
        s.text().contains("  at 140 0 R0\n  name x2"),
        "{}",
        s.text()
    );
}
