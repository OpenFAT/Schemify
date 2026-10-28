//! CPU goldens of the editor, the waves window and dock, the view switcher, and the UI string
//! table under pointer moves.

use schemify_ui::{Button, InputEvent, InputEvents};

use super::node_prop;
use crate::common::{self, Session, U, Ui, VIEW};

/// The editor on the inverter fixture (top cell with two `inv` sub-cell instances), dark
/// theme, default UI. Re-bless with `SCHEMIFY_BLESS=1`.
#[test]
fn golden_editor_dark() {
    let mut s = Session::open("inv", "top.shcomp", "golden", Ui::Default);
    let px = s.render();
    common::check_golden("editor_dark", px);
}

/// Waves: the chooser lists `.raw` files of the project; picking one shows the viewer;
/// Ctrl+W returns to the schematic.
#[test]
fn waves_window_opens_a_raw_file_and_closes() {
    let mut s = Session::open("inv", "top.shcomp", "waves", Ui::Default);
    let raw = concat!(
        env!("SCHEMIFY_ROOT"),
        "/crates/model/wave/tests/fixtures/rc_tran.bin.raw"
    );
    std::fs::copy(raw, s.dir.join("rc_tran.raw")).unwrap();
    s.click_node("tb.waves");
    s.click_node("dialog.choose.row.0");
    assert!(s.ed.waves_open(), "{}", s.status());
    let px = s.render();
    assert_eq!(px.width(), VIEW.0 as u16);
    s.key("Ctrl+W");
    assert!(!s.ed.waves_open());
}

/// The same with a selection and the light canvas (Shift+O), to pin marks and the palette.
#[test]
fn golden_editor_light_selection() {
    let mut s = Session::open("inv", "top.shcomp", "golden-light", Ui::Default);
    s.click((10 * U, 0));
    s.key("Shift+O");
    assert!(s.ed.opts().light);
    let px = s.render();
    common::check_golden("editor_light_selection", px);
}

/// The default UI's "Modern" preset (View → Modern theme, `ui/default-plugin/modern.records`)
/// over the dark and light canvas: one golden each. Toggling it twice restores the parity look.
#[test]
fn golden_editor_modern() {
    let mut s = Session::open("inv", "top.shcomp", "golden-modern", Ui::Default);
    let rgb = |px: &schemify_render::Pixmap, x, y| {
        let c = px.sample(x, y);
        (c.r, c.g, c.b)
    };
    // Parity: xschem's black canvas and Tk-grey menubar.
    let parity = s.render();
    assert_eq!(rgb(&parity, 1100, 200), (0, 0, 0));
    assert_eq!(rgb(&parity, 1100, 5), (0xd9, 0xd9, 0xd9));
    s.run("view.theme_modern", "");
    let px = s.render();
    // The canvas background is the preset's (15171b), not xschem's black.
    assert_eq!(
        rgb(&px, 1100, 200),
        (0x15, 0x17, 0x1b),
        "the preset's canvas"
    );
    assert_eq!(
        rgb(&px, 1100, 5),
        (0x1e, 0x21, 0x27),
        "the preset's menubar"
    );
    common::check_golden("editor_modern_dark", px);
    s.click((10 * U, 0));
    s.key("Shift+O");
    assert!(s.ed.opts().light);
    common::check_golden("editor_modern_light_selection", s.render());
    s.key("Shift+O");
    s.key("Esc");
    s.run("view.theme_modern", "");
    let back = s.render();
    assert_eq!(
        rgb(&back, 1100, 200),
        (0, 0, 0),
        "toggling back restores parity"
    );
    assert_eq!(rgb(&back, 1100, 5), (0xd9, 0xd9, 0xd9));
}

/// Pointer moves rewrite the status line (coordinates) every frame; the UI string table stays
/// the same size (live strings), so a long session cannot fill it.
#[test]
fn pointer_moves_do_not_grow_the_ui_string_table() {
    let mut s = Session::open("inv", "top.shcomp", "strings", Ui::Default);
    s.move_to((0, 0));
    let len = s.ed.shell().strings().len();
    for i in 0..300 {
        s.move_to((i * 10 * U, (i % 7) * 10 * U));
    }
    assert_eq!(
        s.ed.shell().strings().len(),
        len,
        "the permanent table did not grow"
    );
    assert!(
        s.ed.shell().strings().live_len() < 4096,
        "one live generation"
    );
    let status = s.ed.shell().strings().find("status.msg").unwrap();
    let n = s.ed.shell().tables().nodes.find(status).unwrap();
    let label = s.ed.shell().tables().nodes.prop(schemify_ui::Prop::Label)[n as usize];
    assert!(
        s.ed.shell().strings().get(label).contains("mouse = 2990 "),
        "the status line shows the last move: {}",
        s.ed.shell().strings().get(label)
    );
}

/// The waves dock divider: a press on the boundary and a drag move it (the editor above
/// shrinks), clamped to the allowed range; a press away from it is the editor's.
#[test]
fn waves_dock_divider_drags() {
    let mut s = Session::open("inv", "top.shcomp", "dock-split", Ui::Default);
    let raw = concat!(
        env!("SCHEMIFY_ROOT"),
        "/crates/model/wave/tests/fixtures/rc_tran.bin.raw"
    );
    std::fs::copy(raw, s.dir.join("rc_tran.raw")).unwrap();
    s.click_node("tb.waves");
    s.click_node("dialog.choose.row.0");
    assert!(s.ed.waves_open(), "{}", s.status());
    // The dock takes its share of the window from the next frame on.
    s.frame(InputEvents::default());
    let split = (VIEW.1 * 0.6).floor();
    let before = s.ed.canvas_rect();
    assert!(
        before.y1 <= f64::from(split) && before.y1 > 400.0,
        "{before:?}"
    );
    let drag = |s: &mut Session, from: f32, to: f32| {
        s.send([
            InputEvent::PointerMove { x: 600.0, y: from },
            InputEvent::PointerDown {
                x: 600.0,
                y: from,
                button: Button::Left,
            },
            InputEvent::PointerMove { x: 600.0, y: to },
            InputEvent::PointerUp {
                x: 600.0,
                y: to,
                button: Button::Left,
            },
        ]);
    };
    drag(&mut s, split, 300.0);
    let after = s.ed.canvas_rect();
    assert!(after.y1 <= 300.0 && after.y1 > 200.0, "{after:?}");
    assert!(s.ed.waves_open());
    // Clamped at 15% of the window.
    drag(&mut s, 300.0, 5.0);
    assert!(s.ed.canvas_rect().y1 <= f64::from(VIEW.1 * 0.15) + 1.0);
    // A press 40 px above the divider belongs to the editor: the divider stays.
    let at = (VIEW.1 * 0.15).floor();
    drag(&mut s, at - 40.0, 600.0);
    assert!(s.ed.canvas_rect().y1 <= f64::from(VIEW.1 * 0.15) + 1.0);
}

/// A primitive under the Modern preset: the symbol view, and the view switcher with
/// Schematic greyed out (a `.shprim` has none).
#[test]
fn golden_primitive_modern() {
    let mut s = Session::open("rc", "res.shprim", "golden-prim", Ui::Default);
    s.run("view.theme_modern", "");
    common::check_golden("primitive_modern", s.render());
}

/// `(style, disabled)` of the three view-switcher buttons, schematic / symbol / docs.
fn switch_state(s: &Session) -> [(String, String); 3] {
    use schemify_ui::Prop;
    ["switch.schematic", "switch.symbol", "switch.docs"].map(|id| {
        (
            node_prop(s, id, Prop::Style),
            node_prop(s, id, Prop::Disabled),
        )
    })
}

fn st(style: &str, disabled: &str) -> (String, String) {
    (style.to_owned(), disabled.to_owned())
}

/// The view switcher over the canvas follows the file kind: a `.shcomp` has a schematic and
/// a symbol, a `.shprim` only a symbol, a `.shbench` only a schematic. The missing side is
/// disabled (clicks do nothing; the command itself is refused with a status message).
#[test]
fn view_switcher_follows_the_file_kind() {
    use schemify_editor::session::View;
    // .shcomp: both sides; clicking switches and the `on` style follows.
    let mut s = Session::open("inv", "top.shcomp", "switch-comp", Ui::Default);
    assert_eq!(s.ed.active().map(|a| a.1), Some(View::Schematic));
    let [sch, sym, _] = switch_state(&s);
    assert_eq!((sch, sym), (st("viewsw-on", "0"), st("viewsw", "0")));
    s.click_node("switch.symbol");
    assert_eq!(s.ed.active().map(|a| a.1), Some(View::Symbol));
    let [sch, sym, _] = switch_state(&s);
    assert_eq!((sch, sym), (st("viewsw", "0"), st("viewsw-on", "0")));
    s.click_node("switch.schematic");
    assert_eq!(s.ed.active().map(|a| a.1), Some(View::Schematic));
    // Docs toggles the docs pane and lights while it is shown.
    let docs_before = switch_state(&s)[2].0.clone();
    s.click_node("switch.docs");
    assert_ne!(switch_state(&s)[2].0, docs_before);

    // .shprim: no schematic.
    let mut p = Session::open("rc", "res.shprim", "switch-prim", Ui::Default);
    assert_eq!(p.ed.active().map(|a| a.1), Some(View::Symbol));
    let [sch, sym, _] = switch_state(&p);
    assert_eq!((sch, sym), (st("viewsw-off", "1"), st("viewsw-on", "0")));
    p.click_node("switch.schematic");
    assert_eq!(
        p.ed.active().map(|a| a.1),
        Some(View::Symbol),
        "disabled: no-op"
    );
    p.run("view.schematic", "");
    assert_eq!(p.ed.active().map(|a| a.1), Some(View::Symbol));
    assert!(p.status().contains("no schematic"), "{}", p.status());

    // .shbench: no symbol.
    let mut b = Session::open("rc", "tb.shbench", "switch-bench", Ui::Default);
    assert_eq!(b.ed.active().map(|a| a.1), Some(View::Schematic));
    let [sch, sym, _] = switch_state(&b);
    assert_eq!((sch, sym), (st("viewsw-on", "0"), st("viewsw-off", "1")));
    b.run("view.symbol", "");
    assert_eq!(b.ed.active().map(|a| a.1), Some(View::Schematic));
    assert!(b.status().contains("no symbol"), "{}", b.status());
}

/// The switcher floats: centred along the canvas's top edge, painted after the canvas
/// (overlay layer), lit under the pointer only when enabled, and dead under a modal dialog.
#[test]
fn view_switcher_floats_over_the_canvas() {
    use schemify_editor::session::View;
    use schemify_ui::{FLOAT_INSET, Layer};
    let mut s = Session::open("rc", "res.shprim", "switch-float", Ui::Default);
    let sh = &s.ed.shell();
    let row = |id: &str| {
        let n = sh
            .tables()
            .nodes
            .find(sh.strings().find(id).unwrap())
            .unwrap();
        sh.layout().row_of(n).unwrap()
    };
    let r = row("view.switch");
    assert!(
        sh.layout().rows(Layer::Overlay).contains(&r),
        "painted above the canvas"
    );
    let sw = sh.layout().rect()[r];
    let c = s.ed.canvas_rect();
    assert!((f64::from(sw.y) - (c.y0 + f64::from(FLOAT_INSET))).abs() < 0.5);
    let mid = f64::from(sw.x + sw.w / 2.0);
    assert!(
        (mid - (c.x0 + c.x1) / 2.0).abs() < 1.0,
        "centred: {mid} vs {c:?}"
    );
    let (on, off) = (
        sh.layout().rect()[row("switch.docs")],
        sh.layout().rect()[row("switch.schematic")],
    );
    // Hover: the enabled button lights, the disabled one keeps its colour. Probe the left
    // padding, clear of the label.
    let probe = |s: &mut Session, b: schemify_ui::Rect| {
        let px = s.render();
        let c = px.sample((b.x + 3.0) as u16, (b.y + b.h / 2.0) as u16);
        (c.r, c.g, c.b)
    };
    s.move_screen(0.0, 400.0);
    let (on0, off0) = (probe(&mut s, on), probe(&mut s, off));
    s.move_screen(on.x + 3.0, on.y + on.h / 2.0);
    assert_ne!(probe(&mut s, on), on0, "an enabled button lights up");
    s.move_screen(off.x + 3.0, off.y + off.h / 2.0);
    assert_eq!(probe(&mut s, off), off0, "a disabled button does not");
    // Modal: with the insert-symbol chooser open, clicking the switcher does nothing.
    let mut m = Session::open("inv", "top.shcomp", "switch-modal", Ui::Default);
    m.key("Ins");
    assert!(!m.ed.shell().ui().dialog().is_none(), "a dialog is open");
    m.click_node("switch.symbol");
    assert_eq!(m.ed.active().map(|a| a.1), Some(View::Schematic));
}
