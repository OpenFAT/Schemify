//! The waveform window: its composition fits the window (lists measured, no overflow), pointer
//! gestures map to the viewer requests they mean (the point under the pointer stays put), and
//! commands drive the viewer tables end to end.

use schemify_ui::waves::{Drag, Effect, Geometry, WAVES_UI, WaveUi, plot_area};
use schemify_ui::{
    Button, Contribution, HealthKind, InputEvent, InputEvents, Mods, Rect, Strings, Sym,
    TextSystem, UiLayout, UiState, UiTables, compose, layout, records,
};
use schemify_wave::{Cursor, RawPlot, Request, VarKind, Variable, Waves};

const VIEW: (f32, f32) = (1200.0, 760.0);

/// 120 signals named `net<k>` on a 1 ms time axis, `net<k>(t) = sin(k t)`.
fn data() -> Waves {
    let n = 2001;
    let mut vars = vec![Variable {
        name: "time".into(),
        kind: VarKind::Time,
    }];
    let mut re: Vec<f64> = (0..n).map(|i| f64::from(i) * 5e-7).collect();
    for k in 1..=120 {
        vars.push(Variable {
            name: format!("v(net{k})"),
            kind: VarKind::Voltage,
        });
        re.extend((0..n).map(|i| (f64::from(k) * f64::from(i) * 1e-3).sin()));
    }
    let mut w = Waves::new(vec![
        RawPlot::from_columns("tran", vars, re, vec![]).unwrap(),
    ]);
    w.apply(&Request::AddTrace {
        src: "v(net1)".into(),
        dataset: 0,
        pane: 0,
    })
    .unwrap();
    w
}

struct Win {
    strings: Strings,
    base: Contribution,
    tables: UiTables,
    frame: UiLayout,
    geo: Geometry,
    text: TextSystem,
    state: UiState,
    ui: WaveUi,
    waves: Waves,
}

impl Win {
    fn new(waves: Waves) -> Win {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(WAVES_UI, &mut strings);
        assert!(rejects.is_empty(), "{rejects:?}");
        let mut w = Win {
            strings,
            base,
            tables: UiTables::default(),
            frame: UiLayout::default(),
            geo: Geometry::default(),
            text: TextSystem::new(),
            state: UiState::default(),
            ui: WaveUi::default(),
            waves,
        };
        w.rebuild();
        w
    }

    fn rebuild(&mut self) {
        let name = self.strings.intern("session");
        for _ in 0..3 {
            let c = self.ui.contribute(&self.waves, &mut self.strings);
            self.tables = compose(&self.strings, &self.base, &[(name, &c)]);
            self.frame = layout(
                &self.tables,
                &self.strings,
                &mut self.text,
                &self.state,
                VIEW,
            );
            self.geo = Geometry::of(&self.tables, &self.frame, &self.strings);
            let fit = (
                WaveUi::rows_fit(self.geo.signals),
                WaveUi::rows_fit(self.geo.traces),
            );
            if !self.ui.set_capacity(fit.0, fit.1) {
                return;
            }
        }
        panic!("list capacity does not settle");
    }

    fn apply(&mut self, reqs: Vec<Request>) -> Effect {
        let mut fx = Effect::default();
        for r in reqs {
            let res = self.waves.apply(&r);
            let e = self.ui.note(&self.waves, &r, &res);
            fx.redraw |= e.redraw;
            fx.recompose |= e.recompose;
        }
        if fx.recompose {
            self.rebuild();
        }
        fx
    }

    fn route(&mut self, evs: &[InputEvent], now: u64) -> Vec<Request> {
        let events = InputEvents {
            events: evs.to_vec(),
            text: String::new(),
        };
        let mut out = Vec::new();
        self.ui
            .route(&self.waves, &self.geo, &events, now, &mut out);
        out
    }

    fn command(&mut self, name: &str, arg: &str, node: &str, now: u64) -> Vec<Request> {
        let mut out = Vec::new();
        let fx = self
            .ui
            .command(&self.waves, &self.geo, name, arg, node, now, &mut out);
        if fx.recompose {
            self.rebuild();
        }
        out
    }

    fn rect(&self, id: &str) -> Rect {
        let n = self
            .tables
            .nodes
            .find(self.strings.find(id).unwrap())
            .unwrap();
        self.frame.rect()[self.frame.row_of(n).unwrap()]
    }

    fn signal_rows(&self) -> Vec<Sym> {
        let list = self
            .tables
            .nodes
            .find(self.strings.find("waves.signals").unwrap());
        let list = list.unwrap();
        self.tables
            .nodes
            .children(list)
            .map(|c| self.tables.nodes.id()[c as usize])
            .collect()
    }
}

fn inside(r: Rect, outer: Rect) -> bool {
    r.x >= outer.x - 0.5
        && r.y >= outer.y - 0.5
        && r.x + r.w <= outer.x + outer.w + 0.5
        && r.y + r.h <= outer.y + outer.h + 0.5
}

#[test]
fn window_composes_and_lists_fit_their_panels() {
    let mut w = Win::new(data());
    let bad = w.tables.health.kind.iter().filter(|k| {
        matches!(
            k,
            HealthKind::UnknownTarget | HealthKind::Orphaned | HealthKind::NodeReplaced
        )
    });
    assert_eq!(bad.count(), 0, "session rows only add nodes and set props");
    let rows = w.signal_rows();
    assert_eq!(rows.len(), usize::from(WaveUi::rows_fit(w.geo.signals)));
    assert!(
        rows.len() > 10 && rows.len() < 120,
        "a window of the 120 signals"
    );
    for &id in &rows {
        let r = w.rect(w.strings.get(id));
        assert!(
            inside(r, w.geo.signals),
            "row {} overflows the list",
            w.strings.get(id)
        );
    }
    let window = Rect {
        x: 0.0,
        y: 0.0,
        w: VIEW.0,
        h: VIEW.1,
    };
    assert!(inside(w.rect("waves.status"), window));
    // Three panes share the column equally and stack without overlap.
    w.apply(vec![Request::AddPane, Request::AddPane]);
    assert_eq!(w.geo.panes.len(), 3);
    for pair in w.geo.panes.windows(2) {
        assert!(pair[0].y + pair[0].h <= pair[1].y + 0.5);
        assert!((pair[0].h - pair[1].h).abs() <= 1.0);
    }
    // A smaller window re-measures the lists instead of overflowing.
    let before = rows.len();
    w.ui.set_capacity(0, 0);
    w.frame = layout(
        &w.tables,
        &w.strings,
        &mut w.text,
        &w.state,
        (1200.0, 500.0),
    );
    w.geo = Geometry::of(&w.tables, &w.frame, &w.strings);
    assert!(WaveUi::rows_fit(w.geo.signals) < before as u16);
}

#[test]
fn filter_and_scroll_window_the_signal_list() {
    let mut w = Win::new(data());
    w.command("waves.filter", "net11", "waves.filter", 0);
    let names: Vec<String> = w
        .signal_rows()
        .iter()
        .map(|&s| w.strings.get(s).to_owned())
        .collect();
    // net11 and net110..net119 match; their rows are the variables' column numbers.
    assert_eq!(names.len(), 11);
    assert!(
        names.contains(&"waves.sig.11".to_owned()) && names.contains(&"waves.sig.119".to_owned())
    );
    w.command("waves.filter", "", "waves.filter", 0);
    let first = w.signal_rows()[0];
    let r = w.geo.signals;
    let wheel = InputEvent::Wheel {
        x: r.x + 10.0,
        y: r.y + 10.0,
        dx: 0.0,
        dy: -2.0,
    };
    w.route(&[wheel], 0);
    w.rebuild();
    assert_ne!(w.signal_rows()[0], first, "wheel down scrolls the list");
    for _ in 0..100 {
        w.route(&[wheel], 0);
    }
    w.rebuild();
    let last = *w.signal_rows().last().unwrap();
    assert_eq!(
        w.strings.get(last),
        "waves.sig.120",
        "scrolling stops at the end"
    );
}

#[test]
fn wheel_zoom_keeps_the_point_under_the_pointer() {
    let mut w = Win::new(data());
    let p = plot_area(w.geo.panes[0]);
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
    let mut w = Win::new(data());
    let p = plot_area(w.geo.panes[0]);
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
    let reqs = w.route(&[click, release, click], 1000);
    assert_eq!(reqs, vec![Request::Autoscale]);
    let reqs = w.route(&[click, release], 5000);
    assert!(reqs.is_empty(), "a click long after is not a double-click");
}

#[test]
fn commands_drive_the_viewer() {
    let mut w = Win::new(data());
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
fn embedded_graph_routes_with_one_pane_rect() {
    let waves = data();
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
    let p = plot_area(geo.panes[0]);
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
