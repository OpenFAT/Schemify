//! Commands drive the viewer tables end to end: buttons, keys, swatches, and fields that
//! follow typing through `input` commands.
//!
//! Contents: no `pub` items.

use schemify_ui::waves;
use schemify_ui::waves::{DialogFx, Drag};
use schemify_ui::{Button, InputEvent, Kind, Prop, StyleProp};
use schemify_wave::{Cursor, PALETTE, Request};

use crate::Win;

// ---- private ----

#[test]
fn commands_drive_the_viewer() {
    let mut w = Win::new(crate::data());
    // Double-clicking a signal row plots it into the active pane; a single click only selects.
    assert!(
        w.command("waves.signal", "", "waves.sig.7", 1000)
            .is_empty()
    );
    let reqs = w.command("waves.signal", "", "waves.sig.7", 1200);
    assert_eq!(
        reqs,
        vec![Request::AddTrace {
            src: "v(net7)".into(),
            dataset: 0,
            pane: 0
        }]
    );
    w.apply(reqs);
    assert_eq!((w.waves.trace_count(), w.ui.selected_trace()), (2, Some(1)));
    assert_eq!(w.ui.status(), "plotted v(net7)");
    // The expression bar evaluates through the calculator; errors land in the status line.
    let reqs = w.command("waves.plot", "deriv(v(net2)) / 1k", "waves.expr", 0);
    w.apply(reqs);
    assert_eq!(w.waves.trace_count(), 3);
    let reqs = w.command("waves.plot", "v(nope)", "waves.expr", 0);
    w.apply(reqs);
    assert!(w.ui.status().contains("X004"), "{}", w.ui.status());
    let reqs = w.command("waves.plot", "ymax(v(net3))", "waves.expr", 0);
    w.apply(reqs);
    assert_eq!(w.ui.status(), "ymax(v(net3)) = 1");
    // Delete removes the selected trace; cursors toggle; log x needs positive x.
    w.command("waves.trace", "", "waves.trace.0", 0);
    let reqs = w.command("waves.delete_trace", "", "", 0);
    w.apply(reqs);
    assert_eq!((w.waves.trace_count(), w.waves.src(0)), (2, "v(net7)"));
    let reqs = w.command("waves.cursor_a", "", "", 0);
    w.apply(reqs);
    assert_eq!(w.waves.cursors()[0], Some(w.waves.x().at(1.0 / 3.0)));
    let reqs = w.command("waves.cursor_a", "", "", 0);
    w.apply(reqs);
    assert_eq!(w.waves.cursors()[0], None);
    let reqs = w.command("waves.log_x", "", "", 0);
    w.apply(reqs);
    assert!(w.waves.x().log && w.waves.x().lo > 0.0);
    // The Log X box follows the table.
    let id = w.strings.find("waves.log_x").unwrap();
    let n = w.tables.nodes.find(id).unwrap();
    assert!(w.tables.nodes.is_checked(&w.strings, n));
}

#[test]
fn swatches_recolour_the_selected_trace() {
    let mut w = Win::new(crate::data());
    for (k, &c) in PALETTE.iter().enumerate() {
        let class = w.strings.find(&format!("wswatch{k}")).unwrap();
        let row = w.tables.styles.row(class);
        assert_eq!(
            w.tables.styles.get(row, Kind::Button, StyleProp::Bg),
            c,
            "swatch {k} shows PALETTE[{k}]"
        );
    }
    let reqs = w.command("waves.props.swatch", "", "waves.props.swatch.9", 0);
    assert!(reqs.is_empty() && w.ui.status().contains("select a trace"));
    w.command("waves.trace", "", "waves.trace.0", 0);
    let reqs = w.command("waves.props.swatch", "", "waves.props.swatch.9", 0);
    assert_eq!(
        reqs,
        vec![Request::SetColor {
            trace: 0,
            color: PALETTE[9]
        }]
    );
    w.apply(reqs);
    assert_eq!(w.waves.color(0), PALETTE[9]);
    // The trace row gets a class of the new colour.
    let id = w.strings.find("waves.trace.0").unwrap();
    let row = w.tables.nodes.find(id).unwrap();
    let class = w.tables.nodes.prop(Prop::Style)[row as usize];
    assert_eq!(w.strings.get(class), "wtrace-a0a0a0ff-sel");
    let style = w
        .tables
        .styles
        .row(w.strings.find("wtrace-a0a0a0ff").unwrap());
    assert_eq!(
        w.tables.styles.get(style, Kind::Item, StyleProp::Fg),
        PALETTE[9]
    );
}

#[test]
fn keys_place_horizontal_cursors_track_steps_and_measure() {
    let mut w = Win::new(crate::data());
    let p = waves::plot_area(w.geo.panes[0]);
    let (x, y) = (p.x + 0.5 * p.w, p.y + 0.25 * p.h);
    w.route(&[InputEvent::PointerMove { x, y }], 0);
    // Shift+A: horizontal cursor A at the pointer's y; again hides it.
    let want = w.waves.panes()[0].at(0.75);
    let reqs = w.command("waves.hcursor_a", "", "", 0);
    assert!(
        matches!(reqs[..], [Request::HCursor { pane: 0, which: Cursor::A, at: Some(v) }]
            if (v - want).abs() < 1e-6),
        "{reqs:?}"
    );
    w.apply(reqs);
    let reqs = w.command("waves.hcursor_a", "", "", 0);
    assert_eq!(
        reqs,
        vec![Request::HCursor {
            pane: 0,
            which: Cursor::A,
            at: None
        }]
    );
    w.apply(reqs);
    assert_eq!(w.waves.hcursors(0), [None, None]);
    // A press within 6 px of a horizontal cursor drags it.
    let reqs = w.command("waves.hcursor_b", "", "", 0);
    w.apply(reqs);
    let reqs = w.route(
        &[
            InputEvent::PointerDown {
                x,
                y: y + 3.0,
                button: Button::Left,
            },
            InputEvent::PointerMove { x, y: y + 20.0 },
        ],
        10_000,
    );
    assert!(matches!(
        w.ui.drag(),
        Drag::HCursor {
            pane: 0,
            which: Cursor::B
        }
    ));
    assert!(matches!(
        reqs[..],
        [Request::HCursor {
            pane: 0,
            which: Cursor::B,
            at: Some(_)
        }]
    ));
    w.route(
        &[InputEvent::PointerUp {
            x,
            y: y + 20.0,
            button: Button::Left,
        }],
        10_000,
    );
    // t tracks the step under the pointer; t again shows every step.
    w.route(&[InputEvent::PointerMove { x, y }], 10_000);
    let reqs = w.command("waves.track", "", "", 0);
    assert!(
        matches!(&reqs[..], [Request::SetPane { pane: 0, props }] if props.step == Some(0)),
        "{reqs:?} {}",
        w.ui.status()
    );
    w.apply(reqs);
    let reqs = w.command("waves.track", "", "", 0);
    assert!(matches!(&reqs[..], [Request::SetPane { props, .. }] if props.step.is_none()));
    // m: the measurement tooltip follows the pointer.
    w.command("waves.measure", "", "", 0);
    assert!(w.ui.measure());
    w.route(&[InputEvent::PointerMove { x: x + 5.0, y }], 10_000);
    assert!(w.last.redraw, "the tooltip moves with the pointer");
    assert_eq!(w.ui.pointer(), Some((x + 5.0, y)));
}

/// Every field of the window follows typing through its `input` command (plugin-abi §11.14):
/// the filter applies at once, and the expression and a dialog field typed then confirmed by a
/// button (no argument) use the typed text.
#[test]
fn fields_follow_typing_through_input_commands() {
    let mut w = Win::new(crate::data());
    w.command("waves.props", "", "", 0);
    let input = w.tables.nodes.prop(Prop::Input);
    let fields = (w.tables.nodes.kind().iter())
        .enumerate()
        .filter(|&(_, &k)| k == Kind::TextInput);
    let mut n = 0;
    for (i, _) in fields {
        let id = w.strings.get(w.tables.nodes.id()[i]);
        assert!(!input[i].is_none(), "{id} has no input command");
        n += 1;
    }
    assert!(n >= 13, "expression, filter and the dialog's fields: {n}");
    w.command("waves.props.field", "48", "waves.props.f.1", 0);
    assert!(w.last.recompose);
    let reqs = w.command("waves.props.ok", "", "waves.props.ok", 0);
    assert_eq!(w.last.dialog, DialogFx::Close);
    w.apply(reqs);
    assert_eq!(w.waves.props(0).bins, 48);
    w.command("waves.filter", "net11", "waves.filter", 0);
    assert_eq!(w.signal_rows().len(), 11);
    w.command("waves.expr", "v(net3)", "waves.expr", 0);
    assert!(!w.last.redraw && !w.last.recompose, "kept for Enter");
    let reqs = w.command("waves.plot_button", "", "waves.plot", 0);
    assert!(matches!(&reqs[..], [Request::AddTrace { src, .. }] if src == "v(net3)"));
}
