//! Menu parity: highlight, compare, symbol, view, properties, simulation, help and waves
//! commands reached through the default UI's menus.

use std::path::Path;

use schemify_doc::TableId;
use schemify_ui::InputEvents;

use crate::common::{self, Session, U, Ui};

#[test]
fn highlight_menu_duplicates_overlaps_and_selection() {
    let mut s = Session::open("inv", "top.shcomp", "hilite", Ui::Default);
    // Rename x2 to x1: a duplicate.
    s.run("file.save", "");
    s.click((150 * U, 0));
    s.key("q");
    s.click_node("dialog.props.val.0");
    s.key("End");
    s.key("BackSpace");
    s.type_text("1");
    s.click_node("dialog.props.ok");
    s.key("Esc");
    s.key("#");
    assert_eq!(s.ed.selection().len(), 2, "{}", s.status());
    s.key("Ctrl+#");
    let t = s.text();
    assert!(t.contains("name x1\n") && t.contains("name x2\n"), "{t}");
    s.key("Ctrl+#");
    assert!(s.status().contains("no duplicate"));
    // An exact copy on top of x1: select overlapped finds the copy only.
    s.key("Esc");
    s.click((10 * U, 0));
    s.move_to((0, 0));
    s.key("c");
    s.click((0, 0));
    s.key("Esc");
    s.run("highlight.select_overlapped_instances", "");
    assert_eq!(s.ed.selection().len(), 1);
    s.key("Del");
    // Highlight the VDD net, then select what is highlighted.
    s.click((70 * U, -70 * U));
    s.key("k");
    s.key("Esc");
    s.key("Alt+K");
    assert!(s.ed.selection().len() >= 3, "{:?}", s.ed.selection());
    // Increment highlight color draws the net in its own layer.
    s.run("highlight.increment_hilight_color", "");
    s.frame(InputEvents::default());
    // Only probes dims everything but the highlight (palette, no edit).
    s.run("highlight.view_only_probes", "");
    assert!(s.ed.opts().only_probes);
}

#[test]
fn compare_schematics_outlines_the_differences() {
    let mut s = Session::open("inv", "top.shcomp", "compare", Ui::Default);
    s.run("file.save_as", "top_b");
    // Change the copy: delete the VDD rail wire, move the VSS rail wire (clicks off the
    // labels' anchors: a press on a connection point starts a wire).
    s.click((30 * U, -70 * U));
    s.key("Del");
    s.click((100 * U, 70 * U));
    s.move_to((100 * U, 70 * U));
    s.key("m");
    s.click((100 * U, 80 * U));
    assert!(
        s.text().contains("wire w00009 0 80 140 80\n"),
        "{}",
        s.text()
    );
    assert!(!s.text().contains("w00006"));
    s.run("highlight.set_schematic_to_compare_and_compare_wit", "");
    s.click_node("dialog.choose.filter");
    s.type_text("top.sh"); // top.shcomp only (not top_b.shcomp)
    assert!(!super::composed(&s, "dialog.choose.row.1"));
    s.click_node("dialog.choose.row.0");
    s.frame(InputEvents::default());
    assert!(s.status().contains("compare:"), "{}", s.status());
    assert!(
        s.status().contains("1 added/changed here"),
        "{}\n{}\n{:?}",
        s.status(),
        s.text(),
        super::active_path(&s)
    );
    assert!(
        s.status().contains("1 only in the other cell"),
        "{}",
        s.status()
    );
    s.run("highlight.compare_schematics", "");
    assert!(!s.ed.opts().only_probes);
}

#[test]
fn symbol_menu_makes_symbols_schematics_labels_and_pins() {
    let mut s = Session::open("inv", "top.shcomp", "symbol", Ui::Default);
    // Make symbol from schematic: IN left, OUT right, a box and the name.
    s.key("a");
    let t = s.text();
    assert!(t.contains("pin IN in -75 "), "{t}");
    assert!(t.contains("pin OUT out 75 "), "{t}");
    assert!(common::lines(&t, "rect ").len() == 1 && t.contains("{name}"));
    s.key("u");
    // Make schematic from symbol: a port for a pin that has none.
    // The OUT port: just above its anchor (a press on the anchor itself starts a wire).
    s.click((240 * U, -3 * U));
    assert_eq!(s.ed.selection()[0].table, TableId::Ports);
    s.key("Del");
    s.move_to((300 * U, 0));
    s.key("Ctrl+L");
    assert!(
        common::lines(&s.text(), "port ")
            .iter()
            .any(|l| l.ends_with(" 300 0 R180 OUT")),
        "{}",
        s.text()
    );
    // Create symbol pins from selected ports: none missing → status.
    s.click((300 * U, 0));
    s.key("Alt+H");
    assert!(s.status().contains("without a pin"), "{}", s.status());
    // Attach net labels: x2's unconnected pins after a plain move away.
    s.key("Esc");
    s.click((150 * U, 0));
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((400 * U, 0)); // x2 at (390, 0): pins (350, 0) (430, 0) (390, ±30), all free
    s.key("Shift+H");
    let labels = super::owned(&s, "label ");
    for (pin, at) in [
        ("A", "350 0"),
        ("Y", "430 0"),
        ("VDD", "390 -30"),
        ("VSS", "390 30"),
    ] {
        assert!(
            labels
                .iter()
                .any(|l| l.ends_with(&format!(" {at} R0 {pin}"))),
            "{labels:?}"
        );
    }
    // Negative space: no new labels a second time (every pin now touches its label).
    s.key("Shift+H");
    assert_eq!(super::owned(&s, "label ").len(), labels.len());
    // List of nets: the highlight in a text tab; labels from it attach to the pointer.
    s.key("Esc");
    s.click((70 * U, 0));
    s.key("k");
    s.key("J");
    let v = s.ed.session().text_view().expect("a text tab");
    assert!(v.body.contains("mid"), "{}", v.body);
    s.key("Ctrl+Left");
    while s.ed.session().text_view().is_some() {
        s.key("Ctrl+Left");
    }
    s.move_to((400 * U, 0));
    s.key("Alt+J");
    s.click((400 * U, 0));
    assert!(
        common::lines(&s.text(), "label ")
            .iter()
            .any(|l| l.ends_with(" 400 0 R0 mid"))
    );
}

#[test]
fn view_options_shape_the_palette_and_the_ui() {
    let mut s = Session::open("inv", "top.shcomp", "view", Ui::Default);
    assert!(super::composed(&s, "menubar") && super::composed(&s, "toolbar"));
    s.key("\\");
    assert!(
        !super::composed(&s, "menubar") && !super::composed(&s, "status"),
        "canvas only"
    );
    s.key("\\");
    s.run("view.show_hide.show_toolbar", "");
    assert!(super::composed(&s, "menubar") && !super::composed(&s, "toolbar"));
    // Layers: hide layer 1 (wires), only the current layer, all again.
    s.run("layers.toggle.1", "");
    assert_eq!(s.ed.opts().hidden, 1 << 1);
    s.key("Ctrl+5");
    assert_eq!(s.ed.opts().layer, 5);
    s.key(">");
    assert_eq!(s.ed.opts().hidden & (1 << 5), 0);
    assert_ne!(s.ed.opts().hidden & (1 << 4), 0);
    s.key("<");
    assert_eq!(s.ed.opts().hidden, 0);
    // The visible-layers dialog: one checkable row per layer; a click toggles it.
    s.run("view.show_hide.visible_layers", "");
    assert_eq!(super::dialog(&s), "dialog.layers");
    assert!(super::composed(&s, "dialog.layers.row.21"));
    s.click_node("dialog.layers.row.3");
    assert_eq!(s.ed.opts().hidden, 1 << 3);
    s.click_node("dialog.layers.close");
    // Widths, dim, colors, reset.
    s.key("Ctrl++");
    assert_eq!(s.ed.opts().width_tenths, 15);
    s.run("view.set_line_width", "2");
    assert_eq!(s.ed.opts().width_tenths, 20);
    s.run("view.dim_colors", "0.5");
    assert_eq!(s.ed.opts().dim, 50);
    s.run("view.change_current_layer_color", "ff0000");
    s.run("view.change_current_layer_color", "zz");
    assert!(s.status().contains("not an rrggbb"));
    s.run("view.reset_all_colors_to_default", "");
    assert_eq!(
        (
            s.ed.opts().dim,
            s.ed.opts().hidden,
            s.ed.opts().width_tenths
        ),
        (0, 0, 10)
    );
    // Symbols as boxes, symbol texts off: the displayed scene differs, the doc does not.
    let t = s.text();
    s.key("Alt+B");
    assert_eq!(s.ed.opts().show_symbols, 1);
    s.run("view.show_hide.symbol_text", "");
    assert!(!s.ed.opts().symbol_text);
    assert_eq!(s.text(), t);
    // Snap / grid / crosshair size dialogs and their arguments.
    s.run("view.set_grid_spacing", "5");
    assert_eq!(s.ed.opts().grid, 5 * U);
    s.key("Ctrl+G");
    assert_eq!(super::dialog(&s), "dialog.enter_text");
    for _ in 0..4 {
        s.key("BackSpace");
    }
    s.type_text("2.5");
    s.key("Enter");
    assert_eq!(s.ed.opts().snap, 2500);
    s.run("options.crosshair.crosshair_size", "12");
    assert_eq!(s.ed.opts().crosshair_px, 12);
    s.run("options.crosshair.crosshair_size", "-3");
    assert!(s.status().contains("out of range"));
}

#[test]
fn properties_menu_ignore_header_and_edit_file() {
    let mut s = Session::open("inv", "top.shcomp", "props", Ui::Default);
    let original = s.text();
    s.click((150 * U, 0));
    s.key("Shift+T");
    assert!(
        s.text().contains("name x2\n  ignore true\n"),
        "{}",
        s.text()
    );
    s.key("Shift+T");
    assert_eq!(s.text(), original);
    // Header: the doc line.
    s.key("Shift+B");
    assert_eq!(super::dialog(&s), "dialog.props");
    s.click_node("dialog.props.val.1");
    s.key("End");
    s.key("BackSpace");
    s.type_text("y");
    s.click_node("dialog.props.ok");
    assert!(s.text().contains("\n  prefix y\n"), "{}", s.text());
    s.key("u");
    // Edit file: the text, changed and applied as one edit.
    s.key("Alt+Q");
    assert_eq!(super::dialog(&s), "dialog.edit_file");
    let typed = original.replace("label n00001 70 0 R0 mid", "label n00001 70 0 R0 middle");
    s.key("Ctrl+A");
    s.run("dialog.edit_file.ok", &typed);
    assert!(s.text().contains("R0 middle\n"), "{}", s.text());
    assert_eq!(s.text(), typed);
    s.key("u");
    assert_eq!(s.text(), original, "one undo");
    // A text that does not parse is refused with its line.
    s.key("Alt+Q");
    s.run("dialog.edit_file.ok", "schemify 1 shcomp\nwire x 0 0\n");
    assert!(s.status().starts_with("edit file: line"), "{}", s.status());
    assert_eq!(s.text(), original);
}

#[test]
fn simulation_help_and_waves_commands() {
    let mut s = Session::open("inv", "top.shcomp", "simhelp", Ui::Default);
    s.run("simulation.edit_netlist", "");
    let title = s.ed.session().text_view().map(|v| v.title.clone());
    assert_eq!(title.as_deref(), Some("top.sp (netlist)"), "{}", s.status());
    s.run("simulation.changelog_from_current_hierarchy", "");
    let v = s.ed.session().text_view().unwrap();
    assert!(
        v.body.contains("top.shcomp") && v.body.contains("inv.shcomp"),
        "{}",
        v.body
    );
    s.run("simulation.list_running_sub_processes", "");
    assert!(s.ed.session().text_view().unwrap().body.contains("no run"));
    s.run("help.about", "");
    assert!(
        s.ed.session()
            .text_view()
            .unwrap()
            .body
            .contains("Schemify")
    );
    s.run("help.help", "");
    assert!(
        s.ed.session()
            .text_view()
            .unwrap()
            .body
            .contains("Keys are xschem's")
    );
    s.run("waves.load.tran", "");
    assert_eq!(super::dialog(&s), "dialog.choose");
    s.key("Esc");
    s.key("Esc");
    s.run("waves.clear", "");
    assert!(!s.ed.waves_open());
    // From the default, espice, in the dialects' order.
    assert_eq!(s.ed.dialect(), schemify_elab::Dialect::Espice);
    s.run("options.cycle_netlist_format", "");
    assert_eq!(s.ed.dialect(), schemify_elab::Dialect::Xyce);
    s.run("options.cycle_netlist_format", "");
    assert_eq!(s.ed.dialect(), schemify_elab::Dialect::Ltspice);
    s.run("options.netlist_format_symbol_mode.flat_netlist", "");
    assert!(s.ed.opts().flat);
}

/// The default-UI menus reach the new commands: File → Image export → SVG, by clicks.
#[test]
fn menus_reach_parity_commands() {
    let mut s = Session::open("inv", "top.shcomp", "menus", Ui::Default);
    s.click_node("menu.file");
    s.click_node("menu.file.image_export");
    s.click_node("file.image_export.svg_export");
    assert!(
        Path::new(&s.dir.join(".schemify/export/top.svg")).exists(),
        "{}",
        s.status()
    );
}
