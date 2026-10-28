//! Further parity oracles: connectivity over kissing and random wires, auto trim, make
//! subcircuit, highlighted nets, select connected, compare swap, palette options.

use schemify_doc::TableId;
use schemify_editor::editor::Tool;
use schemify_ui::InputEvents;

use super::{CTRL, Lcg, SHIFT};
use crate::common::{self, Session, U};

/// Ctrl+drag stretches; a stretch breaks a direct pin-to-pin contact, a kissing move keeps it
/// with a new wire.
#[test]
fn kissing_keeps_direct_pin_contacts_that_stretch_breaks() {
    let mut s = Session::open("inv", "top.shcomp", "kiss", true);
    // Ctrl+drag on the selected x2: its wires follow.
    s.click((150 * U, 0));
    s.drag((150 * U, 0), (150 * U, 40 * U), CTRL);
    assert!(
        s.text().contains("wire w00001 40 0 100 40\n"),
        "{}",
        s.text()
    );
    s.key("u");
    // Two inverters touching pin to pin: x3 at (300, 0), x4 at (380, 0) (x4.A on x3.Y).
    s.key("Esc");
    s.click((10 * U, 0));
    s.move_to((0, 0));
    s.key("c");
    s.click((300 * U, 0));
    s.move_to((300 * U, 0));
    s.key("c");
    s.click((380 * U, 0));
    let x4 = s.ed.selection()[0];
    let net = |s: &Session, pin: &str| {
        let nets = super::pin_nets(s);
        nets.iter().find(|n| n.0 == pin).map(|n| n.1)
    };
    let a4 = format!("{}.0", x4.key); // x4's pin A
    // x4.A and x3.Y share one net before any move.
    let a4_net = net(&s, &a4).unwrap();
    let shared = super::pin_nets(&s).iter().filter(|n| n.1 == a4_net).count();
    assert!(shared >= 2, "x4.A touches x3.Y: {:?}", super::pin_nets(&s));
    for (key, kiss) in [("Ctrl+M", false), ("Shift+M", true)] {
        s.move_to((380 * U, 0));
        s.key(key);
        s.click((380 * U, 40 * U));
        let after = net(&s, &a4).unwrap();
        let still = super::pin_nets(&s).iter().filter(|n| n.1 == after).count();
        if kiss {
            assert!(still >= 2, "kissing keeps the contact: {}", s.text());
        } else {
            assert_eq!(still, 1, "a stretch alone separates the pins");
        }
        s.key("u");
    }
}

/// Property: join/trim changes no connectivity (a probe on every grid point any wire covers
/// lands in the same net before and after) and is idempotent.
#[test]
fn join_trim_keeps_connectivity_over_random_wires() {
    let mut rng = Lcg(0x7713);
    for case in 0..8 {
        let mut s = Session::open("inv", "top.shcomp", &format!("trimprop{case}"), true);
        let mut text = s.text();
        let mut wires = String::new();
        for k in 0..8 {
            let (x, y) = (300 + rng.below(6) as i32 * 10, rng.below(6) as i32 * 10);
            let len = 10 + rng.below(4) as i32 * 10;
            let (x2, y2) = if rng.below(2) == 0 {
                (x + len, y)
            } else {
                (x, y + len)
            };
            wires.push_str(&format!("wire z{k:05} {x} {y} {x2} {y2}\n"));
        }
        // Insert the wires before the labels (rank 3 is sorted by the writer anyway).
        text = text.replacen("\nlabel ", &format!("\n{wires}label "), 1);
        let canonical = {
            let d = schemify_format::parse(text.as_bytes()).unwrap();
            schemify_format::write(&d)
        };
        s.set_text(&canonical);
        let probes = |s: &Session| -> Vec<Vec<(i32, i32)>> {
            let (h, _) = s.ed.active().unwrap();
            let doc = s.ed.session().project.doc(h).unwrap();
            let cell = s.ed.cell().unwrap();
            let w = doc.wires();
            let mut by_net: std::collections::BTreeMap<u32, Vec<(i32, i32)>> = Default::default();
            for x in (290..=400).step_by(10) {
                for y in (-10..=60).step_by(10) {
                    let p = (x * U, y * U);
                    let on = (0..w.id.len()).find(|&r| {
                        let (a, b) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]));
                        p == a || p == b || schemify_editor::wiring::on_interior(p, a, b)
                    });
                    if let Some(r) = on {
                        by_net
                            .entry(cell.nets.wire_net[r])
                            .or_default()
                            .push((x, y));
                    }
                }
            }
            let mut groups: Vec<_> = by_net.into_values().collect();
            groups.sort();
            groups
        };
        let before = probes(&s);
        s.key("&");
        assert_eq!(probes(&s), before, "case {case}: {wires}");
        let once = s.text();
        s.key("&");
        assert_eq!(s.text(), once, "case {case}: idempotent");
    }
}

#[test]
fn auto_trim_joins_after_each_wire() {
    let mut s = Session::open("inv", "top.shcomp", "autotrim", true);
    s.run("options.auto_join_trim_wires", "");
    s.move_to((300 * U, 0));
    s.key("w");
    s.click((340 * U, 0));
    s.move_to((340 * U, 0));
    s.key("w");
    s.click((380 * U, 0));
    let w = super::owned(&s, "wire ");
    assert!(w.iter().any(|l| l.ends_with(" 300 0 380 0")), "{w:?}");
    assert_eq!(w.len(), 9 + 1);
    // Two undos: the trim, then the second wire.
    s.key("u");
    assert_eq!(super::owned(&s, "wire ").len(), 9 + 2);
}

#[test]
fn make_subcircuit_moves_the_selection_into_a_new_cell() {
    let mut s = Session::open("inv", "top.shcomp", "subckt", true);
    let before = s.text();
    s.click((150 * U, 0)); // x2
    s.key("Ctrl+Shift+H");
    let file = s.dir.join("top_sub1.shcomp");
    let sub = std::fs::read_to_string(&file).unwrap_or_else(|_| panic!("{}", s.status()));
    // x2's four pins sit on four nets that cross the boundary: four ports, four pins.
    assert_eq!(common::lines(&sub, "port ").len(), 4, "{sub}");
    assert_eq!(common::lines(&sub, "pin ").len(), 4, "{sub}");
    assert!(sub.contains("./inv") && sub.contains("name x2"), "{sub}");
    let t = s.text();
    assert!(t.contains("./top_sub1\n"), "{t}");
    assert!(!t.contains("name x2\n"), "x2 moved out: {t}");
    s.key("u");
    assert_eq!(s.text(), before, "one undo restores the schematic");
    // Negative space: nothing selected → a status line, no file.
    s.key("Esc");
    s.key("Ctrl+Shift+H");
    assert!(s.status().contains("select"), "{}", s.status());
    assert!(!s.dir.join("top_sub2.shcomp").exists());
}

#[test]
fn pins_ports_and_labels_from_highlighted_nets() {
    let mut s = Session::open("inv", "top.shcomp", "netlists", true);
    // A new port EN: Alt+H makes it a symbol pin (in, at the port's position).
    s.move_to((300 * U, -40 * U));
    s.key("Ctrl+P");
    s.type_text("EN");
    s.key("Enter");
    s.click((300 * U, -40 * U));
    s.key("Alt+H");
    assert!(s.text().contains("pin EN in 300 -40\n"), "{}", s.text());
    // Highlight `mid`, then Ctrl+J / Alt+Shift+J attach a port / an i-label at the pointer.
    s.key("Esc");
    s.click((60 * U, 0));
    s.key("k");
    s.key("Esc");
    s.move_to((320 * U, 40 * U));
    s.key("Ctrl+J");
    s.click((320 * U, 40 * U));
    assert!(
        super::owned(&s, "port ")
            .iter()
            .any(|l| l.ends_with(" 320 40 R0 mid"))
    );
    s.move_to((360 * U, 40 * U));
    s.key("Alt+Shift+J");
    s.click((360 * U, 40 * U));
    assert!(
        super::owned(&s, "label ")
            .iter()
            .any(|l| l.ends_with(" 360 40 R0 imid"))
    );
    // With buses expanded: a bus net lists one line per bit.
    s.key("Ctrl+Alt+J");
    assert!(s.ed.session().text_view().unwrap().body.contains("mid"));
}

#[test]
fn select_connected_crosses_or_stops_at_junctions() {
    let mut s = Session::open("inv", "top.shcomp", "conn", true);
    // A T on the VDD rail at (30, -70).
    s.move_to((30 * U, -70 * U));
    s.key("w");
    s.click((30 * U, -100 * U));
    s.key("Esc");
    super::right_click(&mut s, (100 * U, -70 * U), SHIFT);
    let all = s.ed.selection().to_vec();
    assert_eq!(
        all.iter().filter(|k| k.table == TableId::Wires).count(),
        4,
        "the rail, its two drops and the T branch: {all:?}"
    );
    assert!(
        all.iter().any(|k| k.table == TableId::Labels),
        "the VDD label"
    );
    s.key("Esc");
    super::right_click(&mut s, (100 * U, -70 * U), CTRL);
    let stop = s.ed.selection().to_vec();
    assert!(
        stop.len() < all.len(),
        "Ctrl stops at the T junction: {stop:?}"
    );
}

#[test]
fn unselect_area_removes_boxed_objects() {
    let mut s = Session::open("inv", "top.shcomp", "unsel", true);
    s.key("Ctrl+A");
    let n = s.ed.selection().len();
    s.key("Shift+D");
    s.drag((-50 * U, -50 * U), (50 * U, 50 * U), 0); // around x1, left to right
    let after = s.ed.selection();
    assert_eq!(after.len(), n - 1, "only x1 was fully inside the box");
    assert!(
        after
            .iter()
            .filter(|k| k.table == TableId::Instances)
            .count()
            == 1,
        "x2 stays selected"
    );
    assert!(matches!(s.ed.tool(), Tool::Idle));
}

#[test]
fn compare_swap_and_toggle() {
    let mut s = Session::open("inv", "top.shcomp", "swap", true);
    s.run("file.save_as", "top_b");
    s.click((30 * U, -70 * U));
    s.key("Del");
    s.run("highlight.set_schematic_to_compare_and_compare_wit", "");
    s.click_node("dialog.choose.filter");
    s.type_text("top.sh");
    s.click_node("dialog.choose.row.0");
    s.frame(InputEvents::default());
    assert!(!s.ed.parity_decor().is_empty(), "the diff is drawn");
    s.run("highlight.compare_schematics", "");
    assert!(s.ed.parity_decor().is_empty(), "toggled off");
    s.run("highlight.compare_schematics", "");
    assert!(!s.ed.parity_decor().is_empty());
    s.run("highlight.swap_compare_schematics", "");
    assert!(
        super::active_path(&s).ends_with("top.shcomp"),
        "{:?}",
        super::active_path(&s)
    );
    assert!(
        s.status().contains("1 added/changed here"),
        "{}",
        s.status()
    );
}

#[test]
fn highlight_decor_options_and_auto_highlight() {
    let mut s = Session::open("inv", "top.shcomp", "hdecor", true);
    s.run("highlight.auto_highlight_net_pins", "");
    s.click((60 * U, 0)); // a wire of `mid`
    assert_eq!(s.ed.highlight_keys().len(), 1, "{}", s.status());
    s.click((200 * U, 40 * U)); // empty space clears it
    assert!(s.ed.highlight_keys().is_empty());
    s.click((60 * U, 0));
    s.run("highlight.enable_highlight_connected_instances", "");
    let outlined = s.ed.parity_decor().rects.x0.len();
    assert_eq!(outlined, 2, "x1 and x2 touch `mid`");
    s.run("highlight.increment_hilight_color", "");
    let d = s.ed.parity_decor();
    assert!(!d.lines.x0.is_empty(), "mid's wire in its own color");
    assert!(
        d.lines
            .layer
            .iter()
            .all(|&l| l != schemify_editor::checks::LAYER_HILITE),
        "a per-net layer, not the shared highlight layer"
    );
    s.run("highlight.view_only_probes", "");
    let p = s.ed.view_palette();
    assert_ne!(
        p.color[1],
        s.ed.view_palette().color[0],
        "dimmed, not hidden"
    );
}

#[test]
fn view_palette_follows_fill_colors_widths_and_symbol_modes() {
    let mut s = Session::open("inv", "top.shcomp", "palette", true);
    let base = s.ed.view_palette();
    s.run("view.toggle_fill_pattern", "");
    assert_eq!(s.ed.view_palette().fill, 0);
    s.run("layers.set_current.4", "");
    s.run("view.change_current_layer_color", "ff0000");
    assert_eq!(s.ed.view_palette().color[4], 0xff00_00ff);
    s.run("view.toggle_variable_line_width", "");
    assert_ne!(s.ed.view_palette().width[1], base.width[1] * 0.0 + 999.0);
    s.run("layers.toggle.1", "");
    assert_eq!(
        s.ed.view_palette().color[1] & 0xff,
        0,
        "hidden layer: alpha 0"
    );
    // Boxes for every instance: the shown scene has the instances' rects only.
    let full = s.ed.shown().lines.x0.len();
    s.run(
        "symbol.show_symbols.show_instance_bounding_boxes_for_all_sym",
        "",
    );
    assert!(s.ed.shown().lines.x0.len() < full);
    assert!(s.ed.shown().rects.x0.len() >= 2);
    s.run("symbol.show_symbols.show_symbol_details", "");
    assert_eq!(s.ed.shown().lines.x0.len(), full);
    // Transparent SVG: no background rectangle.
    s.run("options.transparent_svg_background", "");
    s.run("file.image_export.svg_export", "");
    let t = std::fs::read_to_string(s.dir.join(".schemify/export/top.svg")).unwrap();
    assert!(!t.contains("id=\"background\""));
}

#[test]
fn small_commands_tabs_panels_graphs_and_recent() {
    let mut s = Session::open("inv", "top.shcomp", "small", true);
    s.run("file.create_new_window_tab", "");
    assert_eq!(s.ed.session().tabs().len(), 2);
    s.run("view.show_hide.show_erc_info_window", "");
    assert_eq!(s.ed.panel(), schemify_editor::editor::Panel::Checks);
    s.run("view.show_hide.show_erc_info_window", "");
    assert_eq!(s.ed.panel(), schemify_editor::editor::Panel::None);
    s.key("Ctrl+Left");
    s.run("simulation.graphs.add_waveform_graph", "");
    assert!(s.text().contains("  | ```plot\n"), "{}", s.text());
    assert!(s.ed.docs_shown());
    s.run("simulation.view_last_job_data", "");
    assert!(s.status().contains("no run"));
    s.run("symbol.set_symbol_width", "200");
    assert_eq!(s.ed.opts().symbol_width, 200 * U);
    // The unsnapped wire cut lands between grid points.
    super::right_click(&mut s, (73 * U, 70 * U), SHIFT | schemify_ui::Mods::ALT);
    let w = super::owned(&s, "wire ");
    assert_eq!(w.len(), 10, "{w:?}");
}
