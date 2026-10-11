//! Viewer tables: requests against a model of what they mean (zoom/pan inverses, box zoom
//! containment, autoscale covering the data, readouts equal to interpolation), refusals leaving
//! the tables unchanged, and capacities.

use proptest::prelude::*;
use schemify_wave::{
    Applied, Axis, Cursor, MAX_PANES, RawPlot, Request, VarKind, Variable, ViewError, Waves,
    group_datasets,
};

use crate::fixture;

fn add(w: &mut Waves, src: &str, pane: u8) -> Result<Applied, ViewError> {
    w.apply(&Request::AddTrace {
        src: src.into(),
        dataset: 0,
        pane,
    })
}

fn rc() -> Waves {
    Waves::new(fixture("rc_tran.bin.raw"))
}

fn near(a: Axis, b: Axis) -> bool {
    let tol = 1e-9 * (b.hi - b.lo).abs().max(1e-300);
    a.log == b.log && (a.lo - b.lo).abs() <= tol && (a.hi - b.hi).abs() <= tol
}

#[test]
fn plain_signals_borrow_the_dataset() {
    let mut w = rc();
    assert_eq!(add(&mut w, "v(out)", 0), Ok(Applied::Trace(0)));
    let d = &w.datasets()[0];
    let (x, y) = w.xy(0);
    assert!(std::ptr::eq(x, d.scale()), "x borrows the scale column");
    assert!(
        std::ptr::eq(y, d.col(d.find_var("v(out)").unwrap())),
        "y borrows v(out)"
    );
    // An expression owns its values.
    add(&mut w, "v(out) * 2", 0).unwrap();
    assert!(!std::ptr::eq(w.xy(1).1, w.datasets()[0].col(2)));
    assert_eq!(w.xy(1).1[100], 2.0 * w.xy(0).1[100]);
}

#[test]
fn first_trace_autoscales_and_measurements_report_values() {
    let mut w = rc();
    add(&mut w, "v(in)", 0).unwrap();
    let t = w.datasets()[0].scale();
    assert_eq!(
        w.x(),
        Axis {
            lo: 0.0,
            hi: t[t.len() - 1],
            log: false
        }
    );
    let y = w.panes()[0];
    assert!(y.lo < 0.0 && y.hi > 1.0 && y.hi < 1.1, "padded 0..1: {y:?}");
    let r = add(&mut w, "ymax(v(in))", 0).unwrap();
    assert_eq!(r, Applied::Value("ymax(v(in)) = 1".into()));
    assert_eq!(
        add(&mut w, "1 + 2", 0),
        Ok(Applied::Value("1 + 2 = 3".into()))
    );
    let c = add(&mut w, "cplx(1, -2)", 0);
    assert_eq!(c, Ok(Applied::Value("cplx(1, -2) = 1 - 2j".into())));
    assert_eq!(w.trace_count(), 1);
}

#[test]
fn complex_traces_show_magnitude_on_log_axes() {
    let mut w = Waves::new(fixture("rlc_ac.bin.raw"));
    add(&mut w, "v(out)", 0).unwrap();
    assert!(w.is_magnitude(0));
    w.apply(&Request::LogX { pane: 0, on: true }).unwrap();
    w.apply(&Request::LogY { pane: 0, on: true }).unwrap();
    let f = w.datasets()[0].scale();
    assert_eq!(
        (w.x().lo, w.x().hi, w.x().log),
        (f[0], f[f.len() - 1], true)
    );
    let y = w.panes()[0];
    // |H| peaks at Q/sqrt(1 - 1/4Q²) = 1.155 (Q = 1) and falls to 2.5e-6 at 100 MHz.
    assert!(
        y.log && y.lo > 0.0 && y.lo < 2.5e-6 && y.hi > 1.155,
        "{y:?}"
    );
    let v = w.value_at(0, 1e3).unwrap();
    assert!((v - 1.0).abs() < 1e-3, "|H(1 kHz)| ≈ 1, got {v}");
}

#[test]
fn refusals_change_nothing() {
    let mut w = rc();
    add(&mut w, "v(out)", 0).unwrap();
    w.apply(&Request::AddPane).unwrap();
    add(&mut w, "-1 - v(in)", 1).unwrap();
    let (x, panes, n) = (w.x(), w.panes().to_vec(), w.trace_count());
    for (r, e) in [
        (
            Request::AddTrace {
                src: "v(nope)".into(),
                dataset: 0,
                pane: 0,
            },
            "X004",
        ),
        (
            Request::AddTrace {
                src: "v(out".into(),
                dataset: 0,
                pane: 0,
            },
            "X009",
        ),
        (
            Request::AddTrace {
                src: "v(out)".into(),
                dataset: 3,
                pane: 0,
            },
            "dataset",
        ),
        (
            Request::AddTrace {
                src: "v(out)".into(),
                dataset: 0,
                pane: 9,
            },
            "pane",
        ),
        (Request::RemoveTrace { trace: 5 }, "trace"),
        (
            Request::ZoomY {
                pane: 2,
                at: 0.5,
                factor: 0.5,
            },
            "pane",
        ),
        (Request::LogY { pane: 1, on: true }, "positive"),
    ] {
        let err = w.apply(&r).unwrap_err().to_string();
        assert!(err.contains(e), "{r:?}: {err}");
        assert_eq!(
            (w.x(), w.panes(), w.trace_count()),
            (x, &panes[..], n),
            "{r:?}"
        );
    }
}

#[test]
fn panes_renumber_when_one_is_removed() {
    let mut w = rc();
    w.apply(&Request::AddPane).unwrap();
    w.apply(&Request::AddPane).unwrap();
    add(&mut w, "v(in)", 0).unwrap();
    add(&mut w, "v(out)", 1).unwrap();
    add(&mut w, "i(v1)", 2).unwrap();
    w.apply(&Request::RemovePane { pane: 1 }).unwrap();
    assert_eq!(w.panes().len(), 2);
    assert_eq!((w.trace_count(), w.src(1), w.pane_of(1)), (2, "i(v1)", 1));
    w.apply(&Request::RemovePane { pane: 0 }).unwrap();
    w.apply(&Request::RemovePane { pane: 0 }).unwrap(); // the last pane stays
    assert_eq!(w.panes().len(), 1);
    for _ in 1..MAX_PANES {
        w.apply(&Request::AddPane).unwrap();
    }
    assert_eq!(w.apply(&Request::AddPane), Err(ViewError::Full("panes")));
}

#[test]
fn cursors_read_interpolated_values() {
    let mut w = Waves::new(group_datasets(fixture("dc_step.bin.raw")));
    add(&mut w, "v(a)", 0).unwrap();
    w.apply(&Request::Cursor {
        which: Cursor::A,
        at: Some(1.025),
    })
    .unwrap();
    w.apply(&Request::Cursor {
        which: Cursor::B,
        at: Some(0.5),
    })
    .unwrap();
    w.apply(&Request::SwapCursors).unwrap();
    assert_eq!(w.cursors(), [Some(0.5), Some(1.025)]);
    // Midway between two sweep points of the first step: the mean of their values.
    let (x, y) = w.xy(0);
    let k = x.iter().position(|&v| (v - 1.0).abs() < 1e-12).unwrap();
    let mid = (y[k] + y[k + 1]) / 2.0;
    assert!((w.value_at(0, 1.025).unwrap() - mid).abs() < 1e-12);
    assert_eq!(w.value_at(0, 7.0), None);
}

fn ramp() -> Waves {
    let n = 1001;
    let vars = vec![
        Variable {
            name: "time".into(),
            kind: VarKind::Time,
        },
        Variable {
            name: "v(r)".into(),
            kind: VarKind::Voltage,
        },
    ];
    let mut re: Vec<f64> = (0..n).map(|i| f64::from(i) * 1e-3).collect();
    re.extend((0..n).map(|i| f64::from(i).sin()));
    let mut w = Waves::new(vec![
        RawPlot::from_columns("ramp", vars, re, vec![]).unwrap(),
    ]);
    add(&mut w, "v(r)", 0).unwrap();
    w
}

proptest! {
    #[test]
    fn zoom_then_unzoom_restores_the_view(at in 0.0f64..1.0, f in 0.01f64..100.0, log: bool) {
        let mut w = ramp();
        if log {
            w.apply(&Request::LogX { pane: 0, on: true }).unwrap();
        }
        let before = w.x();
        w.apply(&Request::ZoomX { pane: 0, at, factor: f }).unwrap();
        w.apply(&Request::ZoomX { pane: 0, at, factor: 1.0 / f }).unwrap();
        prop_assert!(near(w.x(), before), "{:?} vs {before:?}", w.x());
    }

    #[test]
    fn pan_is_undone_by_the_opposite_pan(by in -3.0f64..3.0, pane: bool) {
        let mut w = ramp();
        let before = (w.x(), w.panes()[0]);
        if pane {
            w.apply(&Request::PanY { pane: 0, by }).unwrap();
            w.apply(&Request::PanY { pane: 0, by: -by }).unwrap();
        } else {
            w.apply(&Request::PanX { pane: 0, by }).unwrap();
            w.apply(&Request::PanX { pane: 0, by: -by }).unwrap();
        }
        prop_assert!(near(w.x(), before.0) && near(w.panes()[0], before.1));
    }

    #[test]
    fn box_zoom_lands_inside_the_old_view(a in 0.0f64..1.0, b in 0.0f64..1.0) {
        let mut w = ramp();
        let old = w.x();
        w.apply(&Request::BoxX { pane: 0, from: a, to: b }).unwrap();
        let new = w.x();
        if (a - b).abs() > 1e-6 {
            prop_assert!((new.lo - old.at(a.min(b))).abs() < 1e-12);
            prop_assert!((new.hi - old.at(a.max(b))).abs() < 1e-12);
        } else {
            prop_assert_eq!(new, old); // a degenerate box is ignored
        }
    }

    #[test]
    fn autoscale_covers_every_visible_sample(at in 0.0f64..1.0, f in 0.05f64..0.9) {
        let mut w = ramp();
        w.apply(&Request::ZoomX { pane: 0, at, factor: f }).unwrap();
        w.apply(&Request::AutoscaleY { pane: 0 }).unwrap();
        let (x, y) = w.xy(0);
        let (vx, vy) = (w.x(), w.panes()[0]);
        for (xv, yv) in x.iter().zip(y) {
            if *xv >= vx.lo && *xv <= vx.hi {
                prop_assert!(*yv >= vy.lo && *yv <= vy.hi);
            }
        }
        w.apply(&Request::Autoscale).unwrap();
        prop_assert_eq!((w.x().lo, w.x().hi), (0.0, 1.0));
    }
}
