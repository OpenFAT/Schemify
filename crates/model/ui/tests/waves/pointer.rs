//! Pointer gestures map to the viewer requests they mean: wheel zoom keeps the point under
//! the pointer, drags pan, box-zoom and move cursors, an embedded graph routes with one pane.
//!
//! Contents: no `pub` items.

use schemify_ui::waves;
use schemify_ui::waves::{DialogFx, Drag, Geometry, PROPS_DIALOG, WaveUi};
use schemify_ui::{Button, InputEvent, InputEvents, Mods, Rect};
use schemify_wave::{Cursor, Request};

use crate::Win;

// ---- private ----

#[test]
fn wheel_zoom_keeps_the_point_under_the_pointer() {
    let mut w = Win::new(crate::data());
    let p = waves::plot_area(w.geo.panes[0]);
    let (x, y) = (p.x + 0.3 * p.w, p.y + 0.25 * p.h);
    let (x_at, y_at) = (w.waves.x().at(0.3), w.waves.panes()[0].at(0.75));
    let wheel = InputEvent::Wheel {
        x,
        y,
        dx: 0.0,
        dy: 1.0,
    };
    let reqs = w.route(&[wheel], 0);
    assert!(matches!(reqs[..], [Request::ZoomX { factor, .. }] if factor < 1.0));
    w.apply(reqs);
    // Pointer fractions come from f32 pixels: 1e-6 of the span.
    let span = w.waves.x().hi - w.waves.x().lo;
    assert!((w.waves.x().at(0.3) - x_at).abs() < 1e-6 * span);
    // Shift+wheel, and the wheel over the y labels, zoom y about the pointer instead.
    let shift = InputEvent::Mods {
        mods: Mods(Mods::SHIFT),
    };
    let reqs = w.route(&[shift, wheel], 0);
    assert!(matches!(reqs[..], [Request::ZoomY { pane: 0, .. }]));
    w.apply(reqs);
    assert!((w.waves.panes()[0].at(0.75) - y_at).abs() < 1e-6);
    let unshift = InputEvent::Mods {
        mods: Mods::default(),
    };
    let strip = InputEvent::Wheel {
        x: p.x - 20.0,
        y,
        dx: 0.0,
        dy: -1.0,
    };
    assert!(matches!(
        w.route(&[unshift, strip], 0)[..],
        [Request::ZoomY { .. }]
    ));
}

#[test]
fn drags_pan_box_zoom_and_move_cursors() {
    let mut w = Win::new(crate::data());
    let p = waves::plot_area(w.geo.panes[0]);
    let (x0, y0) = (p.x + 100.0, p.y + 50.0);
    let before = (w.waves.x(), w.waves.panes()[0]);
    // Left drag right and down: the data follows the pointer.
    let reqs = w.route(
        &[
            InputEvent::PointerDown {
                x: x0,
                y: y0,
                button: Button::Left,
            },
            InputEvent::PointerMove {
                x: x0 + 40.0,
                y: y0 + 20.0,
            },
            InputEvent::PointerUp {
                x: x0 + 40.0,
                y: y0 + 20.0,
                button: Button::Left,
            },
        ],
        0,
    );
    w.apply(reqs);
    let span = before.0.hi - before.0.lo;
    let dx = f64::from(40.0 / p.w) * span;
    assert!((w.waves.x().lo - (before.0.lo - dx)).abs() < 1e-12 * span.max(1.0));
    assert!(
        w.waves.panes()[0].lo > before.1.lo,
        "dragging down moves the view up"
    );
    // Right drag: box zoom to the dragged fractions of both axes.
    let old = (w.waves.x(), w.waves.panes()[0]);
    let reqs = w.route(
        &[
            InputEvent::PointerDown {
                x: p.x + 0.2 * p.w,
                y: p.y + 0.1 * p.h,
                button: Button::Right,
            },
            InputEvent::PointerMove {
                x: p.x + 0.6 * p.w,
                y: p.y + 0.5 * p.h,
            },
        ],
        0,
    );
    assert!(reqs.is_empty());
    assert!(matches!(w.ui.drag(), Drag::Box { pane: 0, .. }));
    let up = InputEvent::PointerUp {
        x: p.x + 0.6 * p.w,
        y: p.y + 0.5 * p.h,
        button: Button::Right,
    };
    let reqs = w.route(&[up], 0);
    w.apply(reqs);
    assert!((w.waves.x().lo - old.0.at(0.2)).abs() < 1e-6 * span);
    assert!((w.waves.x().hi - old.0.at(0.6)).abs() < 1e-6 * span);
    assert!((w.waves.panes()[0].hi - old.1.at(0.9)).abs() < 1e-6);
    // A cursor is dragged when the press lands within 6 px of it.
    let cx = w.waves.x().at(0.5);
    w.apply(vec![Request::Cursor {
        which: Cursor::B,
        at: Some(cx),
    }]);
    let px = p.x + 0.5 * p.w;
    let reqs = w.route(
        &[
            InputEvent::PointerDown {
                x: px + 4.0,
                y: y0,
                button: Button::Left,
            },
            InputEvent::PointerMove {
                x: p.x + 0.75 * p.w,
                y: y0,
            },
        ],
        0,
    );
    assert!(matches!(
        reqs[..],
        [Request::Cursor {
            which: Cursor::B,
            at: Some(_)
        }]
    ));
    w.apply(reqs);
    assert!((w.waves.cursors()[1].unwrap() - w.waves.x().at(0.75)).abs() < 1e-6 * span);
    // Double-click in the plot autoscales (xschem's `f`).
    let click = InputEvent::PointerDown {
        x: x0,
        y: y0,
        button: Button::Left,
    };
    let release = InputEvent::PointerUp {
        x: x0,
        y: y0,
        button: Button::Left,
    };
    w.route(&[release], 100);
    // Double-click in the plot opens the graph properties dialog (xschem; `f` autoscales).
    let reqs = w.route(&[click, release, click], 1000);
    assert!(reqs.is_empty());
    assert_eq!(w.last.dialog, DialogFx::Open);
    assert_eq!(w.state.dialog(), w.strings.find(PROPS_DIALOG).unwrap());
    w.command("waves.props.cancel", "", "", 1100);
    assert!(w.state.dialog().is_none());
    let reqs = w.route(&[release, click, release], 5000);
    assert!(
        reqs.is_empty() && w.last.dialog == DialogFx::Keep,
        "a click long after is not a double-click"
    );
}

#[test]
fn embedded_graph_routes_with_one_pane_rect() {
    let waves = crate::data();
    let mut ui = WaveUi::default();
    let geo = Geometry {
        panes: vec![Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        }],
        ..Geometry::default()
    };
    let p = waves::plot_area(geo.panes[0]);
    let events = InputEvents {
        events: vec![InputEvent::Wheel {
            x: p.x + p.w / 2.0,
            y: p.y + 10.0,
            dx: 0.0,
            dy: 1.0,
        }],
        text: String::new(),
    };
    let mut out = Vec::new();
    ui.route(&waves, &geo, &events, 0, &mut out);
    assert!(matches!(out[..], [Request::ZoomX { at, .. }] if (at - 0.5).abs() < 1e-6));
}
