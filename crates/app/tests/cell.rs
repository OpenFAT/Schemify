//! The cell settings panel (`view.cell`, `crates/app/src/cell.rs`): every header, pin,
//! param and impl field the netlister reads is editable without the raw Edit-file dialog.
//! Oracle: the document's canonical text after each edit (format spec ranks 0, 1, 5), one
//! undo step per submitted field, and the document left untouched by a rejected value.

mod common;
use common::Session;

use schemify_ui::{InputEvents, Prop};

/// Runs a command as a script would (RPC, init.lua): no clicked node.
fn run(s: &mut Session, name: &str, arg: &str) {
    s.ed.command(name, arg);
    s.frame(InputEvents::default());
}

/// `cell.set` from a script: `<field id> <value>`.
fn set(s: &mut Session, field: &str, value: &str) {
    run(s, "cell.set", &format!("{field} {value}"));
}

fn has_node(s: &Session, id: &str) -> bool {
    let sh = &s.ed.shell;
    sh.strings
        .find(id)
        .and_then(|i| sh.tables.nodes.find(i))
        .is_some()
}

fn value(s: &Session, id: &str) -> String {
    let sh = &s.ed.shell;
    let n = sh
        .strings
        .find(id)
        .and_then(|i| sh.tables.nodes.find(i))
        .unwrap_or_else(|| panic!("{id} composed"));
    sh.strings
        .get(sh.tables.nodes.prop(Prop::Value)[n as usize])
        .to_owned()
}

#[test]
fn panel_projects_the_interface_by_file_kind() {
    let mut s = Session::open("rc", "res.shprim", "cell-show", true);
    assert!(!has_node(&s, "cell.pane"), "hidden until asked");
    run(&mut s, "view.cell", "");
    assert!(has_node(&s, "cell.pane"));
    assert_eq!(value(&s, "cell.hdr.prefix"), "R");
    assert_eq!(value(&s, "cell.hdr.doc"), "Resistor (run fixture)");
    assert_eq!(value(&s, "cell.pin.0.name"), "A");
    assert_eq!(value(&s, "cell.pin.1.name"), "B");
    assert_eq!(value(&s, "cell.param.0.name"), "R");
    assert_eq!(value(&s, "cell.param.0.default"), "1k");
    assert_eq!(value(&s, "cell.param.0.unit"), "Ohm");
    assert_eq!(value(&s, "cell.impl.0.format.spice"), "{name} {A} {B} {R}");
    assert_eq!(value(&s, "cell.impl.0.format.spectre"), "");
    // A primitive can gain any leaf impl but no schematic.
    assert!(has_node(&s, "cell.add_impl.veriloga") && has_node(&s, "cell.add_impl.spice"));
    assert!(!has_node(&s, "cell.add_impl.schematic"));
    assert!(!has_node(&s, "cell.hdr.corner"), "a bench-only attribute");
    run(&mut s, "view.cell", "");
    assert!(!has_node(&s, "cell.pane"));

    // A testbench: header only (no interface), with its bench attributes.
    let mut b = Session::open("rc", "tb.shbench", "cell-bench", true);
    run(&mut b, "view.cell", "");
    assert_eq!(value(&b, "cell.hdr.corner"), "tt");
    assert!(has_node(&b, "cell.hdr.config"));
    assert!(!has_node(&b, "cell.s.pins") && !has_node(&b, "cell.add_param"));
    assert!(!has_node(&b, "cell.hdr.prefix"));
}

#[test]
fn every_netlist_field_edits_through_the_panel() {
    let mut s = Session::open("rc", "res.shprim", "cell-edit", true);
    let has = |s: &Session, line: &str| s.text().lines().any(|l| l == line);

    // Header.
    set(&mut s, "cell.hdr.prefix", "X");
    assert!(has(&s, "  prefix X"), "{}", s.text());
    set(&mut s, "cell.hdr.category", "passive");
    assert!(has(&s, "  category passive"));
    set(&mut s, "cell.hdr.category", "");
    assert!(
        !s.text().contains("category"),
        "an empty value removes the attribute"
    );

    // Pins: dir and domain from dropdown choices, rename, port order.
    run(&mut s, "cell.set", "cell.pin.0.dir=in");
    assert!(has(&s, "pin A in 0 -30"), "{}", s.text());
    run(&mut s, "cell.set", "cell.pin.1.domain=digital");
    assert!(has(&s, "  domain digital"));
    run(&mut s, "cell.set", "cell.pin.1.domain=analog");
    assert!(
        !s.text().contains("domain"),
        "analog is the default: no attribute"
    );
    set(&mut s, "cell.pin.1.name", "N");
    assert!(has(&s, "pin N inout 0 30"));
    run(&mut s, "cell.pin_down", "cell.pin.0.down");
    let t = s.text();
    assert!(t.find("pin N").unwrap() < t.find("pin A").unwrap(), "{t}");
    run(&mut s, "cell.pin_down", "cell.pin.1.down");
    assert_eq!(s.text(), t, "the last pin cannot move later");

    // Params: fields, attributes, kind, add and delete.
    set(&mut s, "cell.param.0.default", "2k");
    assert!(has(&s, "param R 2k"));
    set(&mut s, "cell.param.0.min", "1");
    assert!(has(&s, "  min 1"));
    run(&mut s, "cell.set", "cell.param.0.kind=choice");
    set(&mut s, "cell.param.0.choices", "1k 2k 10k");
    assert!(
        has(&s, "  kind choice") && has(&s, "  choices 1k 2k 10k"),
        "{}",
        s.text()
    );
    run(&mut s, "cell.set", "cell.param.0.kind=number");
    assert!(
        !s.text().contains("kind"),
        "number is the default: no attribute"
    );
    run(&mut s, "cell.add_param", "");
    assert!(has(&s, "param p1 0"));
    run(&mut s, "cell.delete", "cell.param.1.del");
    assert!(!s.text().contains("param p1"));

    // Impls: model, per-dialect formats, a Verilog-A impl with its body, delete.
    set(&mut s, "cell.impl.0.model", "rpoly");
    assert!(has(&s, "  model rpoly"));
    set(
        &mut s,
        "cell.impl.0.format.spectre",
        "{name} ({A} {N}) resistor r={R}",
    );
    assert!(
        s.text()
            .contains("format spectre \"{name} ({A} {N}) resistor r={R}\""),
        "{}",
        s.text()
    );
    set(&mut s, "cell.impl.0.format.spice", "");
    assert!(!s.text().contains("format spice"));
    run(&mut s, "cell.add_impl", "veriloga");
    assert!(
        has(&s, "impl va veriloga") && has(&s, "  default spice"),
        "{}",
        s.text()
    );
    set(&mut s, "cell.impl.1.body", "module r(A, N);\nendmodule");
    assert!(s.text().contains("body va"), "{}", s.text());
    assert!(s.text().contains("module r(A, N);") && s.text().contains("endmodule"));
    run(&mut s, "cell.delete", "cell.impl.1.del");
    assert!(!s.text().contains("impl va") && !s.text().contains("body va"));
    assert!(!s.text().contains("default"), "one impl left: no default");

    // One undo per submitted field.
    let before = s.text();
    set(&mut s, "cell.param.0.unit", "V");
    assert!(has(&s, "  unit V"));
    s.key("u");
    assert_eq!(s.text(), before);
}

#[test]
fn rejected_values_leave_the_document_untouched() {
    let mut s = Session::open("rc", "res.shprim", "cell-reject", true);
    let before = s.text();
    let cases = [
        ("cell.pin.0.dir=sideways", "dir must be one of"),
        ("cell.param.0.default ", "needs a default"),
        ("cell.hdr.prefix AB", "one letter"),
        ("cell.hdr.corner tt", "header attribute must be one of"),
        ("cell.pin.0.name two words", "one word"),
        ("cell.hdr.default nope", "no impl named nope"),
        ("cell.impl.0.format.vhdl x", "dialect must be one of"),
        ("cell.pin.9.name Z", "no such pin"),
    ];
    for (arg, why) in cases {
        run(&mut s, "cell.set", arg);
        assert!(
            s.ed.session.status.contains(why),
            "{arg}: {}",
            s.ed.session.status
        );
        assert_eq!(s.text(), before, "{arg} changed the document");
    }
    run(&mut s, "cell.add_impl", "schematic");
    assert!(s.ed.session.status.contains("no schematic"));
    assert_eq!(s.text(), before);
}

/// The real UI path: the Settings button, a dropdown choice, typing into an empty field,
/// an add button.
#[test]
fn panel_edits_by_mouse_and_keyboard() {
    let mut s = Session::open("rc", "res.shprim", "cell-ui", true);
    s.click_node("switch.cell");
    assert!(has_node(&s, "cell.pane"));
    s.click_node("cell.pin.0.dir");
    s.click_node("cell.pin.0.dir=out");
    assert!(s.text().contains("pin A out 0 -30"), "{}", s.text());
    s.click_node("cell.param.0.max");
    s.type_text("10k");
    s.key("Enter");
    assert!(s.text().contains("  max 10k"), "{}", s.text());
    // The add buttons end the page: scroll the panel to its bottom first, as a user would.
    let page = {
        let sh = &s.ed.shell;
        let n = sh
            .tables
            .nodes
            .find(sh.strings.find("cell.page").unwrap())
            .unwrap();
        sh.layout.rect()[sh.layout.row_of(n).unwrap()]
    };
    for _ in 0..40 {
        let mut ev = InputEvents::default();
        ev.push(schemify_ui::InputEvent::Wheel {
            x: page.x + 20.0,
            y: page.y + 40.0,
            dx: 0.0,
            dy: -5.0,
        });
        s.frame(ev);
    }
    s.click_node("cell.add_impl.verilog");
    assert!(s.text().contains("impl verilog verilog"), "{}", s.text());
}

/// A new component starts without a schematic impl, which E025 refuses on save; the panel
/// adds it and the file saves.
#[test]
fn a_new_component_becomes_saveable() {
    let mut s = Session::open("inv", "top.shcomp", "cell-new", true);
    run(&mut s, "file.clear_schematic", "");
    run(&mut s, "file.save", "");
    assert!(
        !s.ed.session.status.starts_with("saved"),
        "{}",
        s.ed.session.status
    );
    run(&mut s, "cell.add_impl", "schematic");
    assert!(s.text().contains("impl sch schematic"), "{}", s.text());
    run(&mut s, "file.save", "");
    assert!(
        s.ed.session.status.starts_with("saved"),
        "{}",
        s.ed.session.status
    );
}

/// The panel's look: a primitive under the Modern preset with the panel open.
#[test]
fn golden_cell_panel_modern() {
    let mut s = Session::open("rc", "res.shprim", "cell-golden", true);
    run(&mut s, "view.theme_modern", "");
    run(&mut s, "view.cell", "");
    let px = s.render();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/cell_panel_modern.png"
    );
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        common::save_png(&px, std::path::Path::new(path));
    }
    let want = std::fs::read(path).expect("golden missing; run with SCHEMIFY_BLESS=1");
    let want = schemify_render::Pixmap::from_png(std::io::Cursor::new(want)).expect("png");
    let bad = want
        .data()
        .iter()
        .zip(px.data())
        .filter(|(a, b)| a.r.abs_diff(b.r) > 2 || a.g.abs_diff(b.g) > 2 || a.b.abs_diff(b.b) > 2)
        .count();
    assert!(bad <= want.data().len() / 1000, "{bad} pixels differ");
}
