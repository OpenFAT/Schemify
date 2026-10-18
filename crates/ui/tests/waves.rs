//! The waveform window: its composition fits the window (lists measured, no overflow), pointer
//! gestures map to the viewer requests they mean (the point under the pointer stays put),
//! commands drive the viewer tables end to end, and the graph properties dialog round-trips
//! (spec waves §8: OK on an unchanged dialog changes nothing).

use schemify_ui::waves::{
    DialogFx, Drag, Effect, Geometry, PROPS_DIALOG, ReadoutHead, ReadoutRow, WAVES_UI, WaveUi,
    lane_name, legend_label, measure_at, measure_label, plot_area, readout_head, readout_rows,
};
use schemify_ui::{
    Button, Contribution, HealthKind, InputEvent, InputEvents, Kind, Mods, Prop, Rect, Strings,
    StyleProp, Sym, TextSystem, UiLayout, UiState, UiTables, compose, layout, records,
};
use schemify_wave::{
    Cursor, PALETTE, PaneMode, PaneProps, Radix, RawPlot, Request, VarKind, Variable, Waves,
};

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
    /// Effect of the last `route` or `command`.
    last: Effect,
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
            last: Effect::default(),
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
        self.last = self
            .ui
            .route(&self.waves, &self.geo, &events, now, &mut out);
        self.show(self.last.dialog);
        out
    }

    /// Shows or closes the properties dialog as the app does.
    fn show(&mut self, fx: DialogFx) {
        match fx {
            DialogFx::Keep => return,
            DialogFx::Open => {
                let id = self.strings.intern(PROPS_DIALOG);
                self.state.set_dialog(id);
            }
            DialogFx::Close => self.state.set_dialog(Sym::NONE),
        }
        self.rebuild();
    }

    fn command(&mut self, name: &str, arg: &str, node: &str, now: u64) -> Vec<Request> {
        let mut out = Vec::new();
        let fx = self
            .ui
            .command(&self.waves, &self.geo, name, arg, node, now, &mut out);
        self.last = fx;
        if fx.recompose {
            self.rebuild();
        }
        self.show(fx.dialog);
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

/// Deterministic xorshift (seed-replayable cases without a proptest dependency).
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// A finite value in one of the canonical number text forms: plain decimal, tiny or huge
/// (exponent), integral.
fn pick(seed: &mut u64) -> f64 {
    let r = rng(seed);
    let m = (r >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
    match r % 5 {
        0 => m * 1e3,
        1 => m * 1e-14,
        2 => m * 1e17,
        3 => (m * 1e6).round(),
        _ => m,
    }
}

/// `lo .. lo + w`, a valid linear range.
fn range(seed: &mut u64) -> (f64, f64) {
    let lo = pick(seed);
    let w = lo.abs().max(1.0) * (0.01 + (rng(seed) % 10_000) as f64 / 100.0);
    (lo, lo + w)
}

#[test]
fn properties_dialog_round_trips() {
    let mut seed = 0x2545_f491_4f6c_dd1d;
    let waves = data();
    for case in 0..64 {
        let mut w = Win::new(waves.clone());
        let modes = [PaneMode::Analog, PaneMode::Digital, PaneMode::Histogram];
        let props = PaneProps {
            mode: modes[(rng(&mut seed) % 3) as usize],
            threshold: pick(&mut seed),
            radix: if rng(&mut seed).is_multiple_of(2) {
                Radix::Hex
            } else {
                Radix::Dec
            },
            bins: 2 + (rng(&mut seed) % 1023) as u16,
            locked: rng(&mut seed).is_multiple_of(2),
            step: rng(&mut seed)
                .is_multiple_of(3)
                .then(|| (rng(&mut seed) % 5) as u32),
        };
        let (xl, xh) = range(&mut seed);
        let (yl, yh) = range(&mut seed);
        let mut opt = || rng(&mut seed).is_multiple_of(2).then(|| pick(&mut seed));
        let (ca, cb, ha, hb) = (opt(), opt(), opt(), opt());
        let setup = [
            Request::SetPane { pane: 0, props },
            Request::SetX {
                pane: 0,
                lo: xl,
                hi: xh,
            },
            Request::SetY {
                pane: 0,
                lo: yl,
                hi: yh,
            },
            Request::Cursor {
                which: Cursor::A,
                at: ca,
            },
            Request::Cursor {
                which: Cursor::B,
                at: cb,
            },
            Request::HCursor {
                pane: 0,
                which: Cursor::A,
                at: ha,
            },
            Request::HCursor {
                pane: 0,
                which: Cursor::B,
                at: hb,
            },
        ];
        for r in &setup {
            w.waves
                .apply(r)
                .unwrap_or_else(|e| panic!("case {case}: {r:?}: {e}"));
        }
        let state = |w: &Win| {
            (
                w.waves.props(0),
                w.waves.x_of(0),
                w.waves.panes()[0],
                w.waves.cursors(),
                w.waves.hcursors(0),
            )
        };
        let before = state(&w);
        w.command("waves.props", "", "", 0);
        assert_eq!(w.last.dialog, DialogFx::Open, "case {case}");
        let reqs = w.command("waves.props.ok", "", "", 0);
        assert_eq!(
            w.last.dialog,
            DialogFx::Close,
            "case {case}: {}",
            w.ui.status()
        );
        assert_eq!(reqs.len(), 1, "case {case}: only the properties: {reqs:?}");
        w.apply(reqs);
        assert_eq!(state(&w), before, "case {case}");
    }
}

#[test]
fn properties_dialog_applies_edits_and_keeps_bad_ones_open() {
    let mut w = Win::new(data());
    w.command("waves.props", "", "", 0);
    let bad = w
        .tables
        .health
        .kind
        .iter()
        .filter(|k| matches!(k, HealthKind::UnknownTarget | HealthKind::Orphaned));
    assert_eq!(bad.count(), 0, "the dialog's rows target declared nodes");
    let window = Rect {
        x: 0.0,
        y: 0.0,
        w: VIEW.0,
        h: VIEW.1,
    };
    assert!(inside(w.rect(PROPS_DIALOG), window));
    // A field that does not parse keeps the dialog open and names the field.
    let reqs = w.command("waves.props.ok", "2meg", "waves.props.f.1", 0);
    assert!(reqs.is_empty());
    assert_eq!(w.last.dialog, DialogFx::Keep);
    assert!(w.ui.status().starts_with("bins"), "{}", w.ui.status());
    assert_eq!(w.state.dialog(), w.strings.find(PROPS_DIALOG).unwrap());
    // Mode cycles analog -> digital; Enter in a field applies the whole dialog.
    w.command("waves.props.mode", "", "", 0);
    let reqs = w.command("waves.props.ok", "64", "waves.props.f.1", 0);
    assert_eq!(w.last.dialog, DialogFx::Close);
    assert!(w.state.dialog().is_none());
    w.apply(reqs);
    let p = w.waves.props(0);
    assert_eq!((p.mode, p.bins), (PaneMode::Digital, 64));
    // A typed cursor position (SI suffix) moves only that cursor.
    w.command("waves.props", "", "", 0);
    let reqs = w.command("waves.props.ok", "1.5u", "waves.props.f.7", 0);
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    let at = reqs.iter().find_map(|r| match r {
        Request::Cursor {
            which: Cursor::A,
            at,
        } => Some(*at),
        _ => None,
    });
    assert!(
        matches!(at, Some(Some(v)) if (v - 1.5e-6).abs() < 1e-20),
        "{reqs:?}"
    );
    w.apply(reqs);
    // An x range typed out of order is refused; Cancel closes without changes.
    w.command("waves.props", "", "", 0);
    let x = w.waves.x();
    let reqs = w.command("waves.props.ok", "5", "waves.props.f.3", 0);
    assert!(reqs.is_empty() && w.ui.status().starts_with("x range"));
    w.command("waves.props.cancel", "", "", 0);
    assert!(w.state.dialog().is_none());
    assert_eq!(w.waves.x(), x);
}

#[test]
fn swatches_recolour_the_selected_trace() {
    let mut w = Win::new(data());
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
    let mut w = Win::new(data());
    let p = plot_area(w.geo.panes[0]);
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
    let mut w = Win::new(data());
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

// --- readout, legend and measurement strings (the render crate only places them) ----------

/// Time 0..=10 s: `v(ramp)` = t mV, `v(d0)` = 1 from t = 5, `v(d1)` = 1 at odd t.
fn small() -> Waves {
    let n = 11u8;
    let col = |name: &str, kind| Variable {
        name: name.into(),
        kind,
    };
    let vars = vec![
        col("time", VarKind::Time),
        col("v(ramp)", VarKind::Voltage),
        col("v(d0)", VarKind::Voltage),
        col("v(d1)", VarKind::Voltage),
    ];
    let mut re: Vec<f64> = (0..n).map(f64::from).collect();
    re.extend((0..n).map(|i| f64::from(i) * 1e-3));
    re.extend((0..n).map(|i| f64::from(u8::from(i >= 5))));
    re.extend((0..n).map(|i| f64::from(i % 2)));
    Waves::new(vec![
        RawPlot::from_columns("tran", vars, re, vec![]).unwrap(),
    ])
}

fn add(w: &mut Waves, src: &str, pane: u8) {
    let r = Request::AddTrace {
        src: src.into(),
        dataset: 0,
        pane,
    };
    w.apply(&r).unwrap_or_else(|e| panic!("{src}: {e}"));
}

fn cursors(w: &mut Waves, a: Option<f64>, b: Option<f64>) {
    for (which, at) in [(Cursor::A, a), (Cursor::B, b)] {
        w.apply(&Request::Cursor { which, at }).unwrap();
    }
}

type Head = (
    [&'static str; 2],
    [bool; 2],
    Option<&'static str>,
    Option<&'static str>,
);

/// Cursor a, cursor b, horizontal cursor A of pane 0, the active pane.
type Cursors = (Option<f64>, Option<f64>, Option<f64>, u8);

#[test]
fn readout_head_formats_cursors_and_deltas() {
    // (a, b, horizontal A on pane 0, active pane) -> (x, shown, dx, dy)
    let cases: [(Cursors, Head); 7] = [
        (
            (None, None, None, 0),
            (["x = -", "x = -"], [false, false], None, None),
        ),
        (
            (Some(2.0), None, None, 0),
            (["x = 2", "x = -"], [true, false], None, None),
        ),
        (
            (None, Some(6.0), None, 0),
            (["x = -", "x = 6"], [false, true], None, None),
        ),
        (
            (Some(2.0), Some(6.0), None, 0),
            (
                ["x = 2", "x = 6"],
                [true, true],
                Some("Δx = 4   1/Δx = 250m"),
                None,
            ),
        ),
        (
            (Some(3.0), Some(3.0), None, 0),
            (
                ["x = 3", "x = 3"],
                [true, true],
                Some("Δx = 0   1/Δx = -"),
                None,
            ),
        ),
        (
            (None, None, Some(1e-3), 0),
            (
                ["x = -", "x = -"],
                [false, false],
                None,
                Some("pane 1: A y = 1m  B y = -  Δy = -"),
            ),
        ),
        // An active pane past the last reads the last pane's cursors but prints as given.
        (
            (None, None, Some(1e-3), 5),
            (
                ["x = -", "x = -"],
                [false, false],
                None,
                Some("pane 6: A y = 1m  B y = -  Δy = -"),
            ),
        ),
    ];
    for ((a, b, h, active), (x, shown, dx, dy)) in cases {
        let mut w = small();
        add(&mut w, "v(ramp)", 0);
        cursors(&mut w, a, b);
        w.apply(&Request::HCursor {
            pane: 0,
            which: Cursor::A,
            at: h,
        })
        .unwrap();
        let want = ReadoutHead {
            x: x.map(str::to_owned),
            shown,
            dx: dx.map(str::to_owned),
            dy: dy.map(str::to_owned),
        };
        assert_eq!(readout_head(&w, active), want, "a={a:?} b={b:?} h={h:?}");
    }
}

#[test]
fn readout_rows_read_traces_at_the_cursors() {
    let mut w = small();
    w.apply(&Request::AddPane).unwrap();
    let digital = PaneProps {
        mode: PaneMode::Digital,
        ..PaneProps::default()
    };
    w.apply(&Request::SetPane {
        pane: 1,
        props: digital,
    })
    .unwrap();
    add(&mut w, "v(ramp)", 0);
    add(&mut w, "v(ramp) + v(ramp) - v(ramp)", 0);
    add(&mut w, "v(d0)", 1);
    add(&mut w, "{v(d0), v(d1)}", 1);
    let row = |w: &Waves, t: usize, cells: [&str; 4]| ReadoutRow {
        color: w.color(t),
        cells: cells.map(str::to_owned),
    };
    let all = [
        row(&w, 0, ["v(ramp)", "2m", "6m", "4m"]),
        row(&w, 1, ["v(ramp) + v(ra", "2m", "6m", "4m"]), // cut at READOUT_NAME_CHARS
        row(&w, 2, ["v(d0)", "0", "1", "-"]),
        row(&w, 3, ["{v(d0), v(d1)}", "0x0", "0x2", "-"]), // a bus in the pane's radix
    ];
    let mut out = Vec::new();
    cursors(&mut w, Some(2.0), Some(6.0));
    for max in [0, 1, 3, 4, 100] {
        readout_rows(&w, max, &mut out);
        assert_eq!(out, all[..max.min(4)], "max {max}");
    }
    // No cursors, or a pane on its own x: every value is `-`.
    cursors(&mut w, None, None);
    readout_rows(&w, 1, &mut out);
    assert_eq!(out, [row(&w, 0, ["v(ramp)", "-", "-", "-"])]);
    cursors(&mut w, Some(2.0), Some(6.0));
    let props = PaneProps {
        locked: false,
        ..PaneProps::default()
    };
    w.apply(&Request::SetPane { pane: 0, props }).unwrap();
    readout_rows(&w, 1, &mut out);
    assert_eq!(out, [row(&w, 0, ["v(ramp)", "-", "-", "-"])]);
    // Outside the data: `-`.
    cursors(&mut w, Some(-50.0), Some(50.0));
    readout_rows(&w, 4, &mut out);
    assert!(
        out.iter().all(|r| r.cells[1..] == ["-", "-", "-"]),
        "{out:?}"
    );
}

#[test]
fn legend_and_lane_labels() {
    let mut w = small();
    add(&mut w, "v(ramp)", 0);
    let mut t = String::from("stale");
    // (cursor a, pane locked) -> label
    for (a, locked, want) in [
        (None, true, "v(ramp)"),
        (Some(2.0), true, "v(ramp) = 2m"),
        (Some(50.0), true, "v(ramp)"), // outside the data
        (Some(2.0), false, "v(ramp)"), // own x: cursor a does not apply
    ] {
        cursors(&mut w, a, None);
        let props = PaneProps {
            locked,
            ..PaneProps::default()
        };
        w.apply(&Request::SetPane { pane: 0, props }).unwrap();
        legend_label(&w, 0, &mut t);
        assert_eq!(t, want, "a={a:?} locked={locked}");
    }
    for (src, want) in [
        ("v(d0)", "v(d0)"),
        ("0123456789ab", "012345678"),
        ("µµµµµµµµµµµ", "µµµµµµµµµ"),
        ("", ""),
    ] {
        assert_eq!(lane_name(src), want);
    }
}

#[test]
fn measure_reads_the_plot_under_the_pointer() {
    let mut w = small();
    add(&mut w, "v(ramp)", 0);
    let pane = Rect {
        x: 0.0,
        y: 0.0,
        w: 200.0,
        h: 100.0,
    };
    let geo = Geometry {
        panes: vec![pane],
        ..Geometry::default()
    };
    let p = plot_area(pane);
    let (xa, ya) = (w.x_of(0), w.panes()[0]);
    let near = |got: Option<(f64, f64)>, want: (f64, f64)| {
        got.is_some_and(|(x, y)| {
            (x - want.0).abs() <= 1e-4 * (xa.hi - xa.lo)
                && (y - want.1).abs() <= 1e-4 * (ya.hi - ya.lo)
        })
    };
    let (cx, cy) = (p.x + p.w / 2.0, p.y + p.h / 2.0);
    let mid = ((xa.lo + xa.hi) / 2.0, (ya.lo + ya.hi) / 2.0);
    assert!(near(measure_at(&geo, &w, cx, cy), mid), "plot centre");
    let corner = measure_at(&geo, &w, p.x, p.y + p.h - 1e-3);
    assert!(near(corner, (xa.lo, ya.lo)), "bottom left {corner:?}");
    // Negative space: the y-label strip, the legend, outside every pane, no panes at all.
    for (x, y) in [(10.0, cy), (cx, 5.0), (cx, 150.0), (-1.0, -1.0)] {
        assert_eq!(measure_at(&geo, &w, x, y), None, "({x}, {y})");
        assert_eq!(measure_label(&geo, &w, x, y), None);
    }
    assert_eq!(measure_at(&Geometry::default(), &w, cx, cy), None);
    let (vx, vy) = measure_at(&geo, &w, cx, cy).unwrap();
    let want = format!(
        "y={}  x={}",
        schemify_wave::format_si(vy, 4),
        schemify_wave::format_si(vx, 4)
    );
    assert_eq!(measure_label(&geo, &w, cx, cy), Some(want));
}
