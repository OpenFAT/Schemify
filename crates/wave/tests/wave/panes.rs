//! Pane properties and readouts (spec waves): readouts against a reference interpolator, buses
//! against their bits, own x axes (fft, per-step results, unlocked panes), histogram mode,
//! dataset tracking, cursors, colours, and reloading new data.

use proptest::prelude::*;
use schemify_wave::{
    Applied, Cursor, PaneMode, PaneProps, RawPlot, Request, VarKind, Variable, ViewError, Waves,
    group_datasets,
};

use crate::fixture;

fn var(name: &str, kind: VarKind) -> Variable {
    Variable {
        name: name.into(),
        kind,
    }
}

/// A dataset with x = `time` and one column per name; `cols[k]` holds column `k`'s samples.
fn dataset(x: &[f64], cols: &[(&str, Vec<f64>)]) -> RawPlot {
    let mut vars = vec![var("time", VarKind::Time)];
    let mut re = x.to_vec();
    for (name, c) in cols {
        assert_eq!(c.len(), x.len());
        vars.push(var(name, VarKind::Voltage));
        re.extend_from_slice(c);
    }
    RawPlot::from_columns("synthetic", vars, re, vec![]).unwrap()
}

fn add(w: &mut Waves, src: &str, pane: u8) -> Result<Applied, ViewError> {
    w.apply(&Request::AddTrace {
        src: src.into(),
        dataset: 0,
        pane,
    })
}

fn set(w: &mut Waves, pane: u8, f: impl FnOnce(&mut PaneProps)) -> Result<Applied, ViewError> {
    let mut props = w.props(usize::from(pane));
    f(&mut props);
    w.apply(&Request::SetPane { pane, props })
}

/// The reference evaluator of a readout: a linear scan for the first sample equal to `x`, else
/// the segment strictly around it, interpolated.
fn reference(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    if xs.is_empty() || x < xs[0] || x > xs[xs.len() - 1] {
        return None;
    }
    for k in 0..xs.len() {
        if xs[k] == x {
            return Some(ys[k]);
        }
        if k + 1 < xs.len() && xs[k] < x && x < xs[k + 1] {
            let (x0, x1, y0, y1) = (xs[k], xs[k + 1], ys[k], ys[k + 1]);
            return Some(y0 + (y1 - y0) * (x - x0) / (x1 - x0));
        }
    }
    None
}

/// Sweep data: each step restarts x at 0 with positive (or zero, after the first) increments.
fn stepped() -> impl Strategy<Value = (Vec<f64>, Vec<f64>)> {
    prop::collection::vec(prop::collection::vec((0u8..4, -50i32..50), 2..30), 1..4).prop_map(
        |steps| {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            for st in steps {
                let mut t = 0.0;
                for (k, (dx, v)) in st.into_iter().enumerate() {
                    if k > 0 {
                        t += if k == 1 {
                            1.0 + f64::from(dx)
                        } else {
                            f64::from(dx) * 0.5
                        };
                    }
                    x.push(t);
                    y.push(f64::from(v) * 0.1);
                }
            }
            (x, y)
        },
    )
}

proptest! {
    #[test]
    fn readouts_equal_the_reference_interpolator(
        (x, y) in stepped(),
        q in -1.0f64..60.0,
    ) {
        let mut w = Waves::new(vec![dataset(&x, &[("v(y)", y)])]);
        add(&mut w, "v(y)", 0).unwrap();
        let (xs, ys) = w.xy(0);
        let steps = w.steps(0).to_vec();
        let mut first = None;
        for (s, r) in steps.iter().enumerate() {
            let (a, b) = (r.start as usize, r.end as usize);
            let want = reference(&xs[a..b], &ys[a..b], q);
            prop_assert_eq!(w.value_at_step(0, s, q), want, "step {}", s);
            first = first.or(want);
        }
        prop_assert_eq!(w.value_at(0, q), first);
        // Tracking one step reads only that step.
        let last = steps.len() - 1;
        set(&mut w, 0, |p| p.step = Some(last as u32)).unwrap();
        prop_assert_eq!(w.shown_steps(0), last..last + 1);
        prop_assert_eq!(w.value_at(0, q), w.value_at_step(0, last, q));
    }

    #[test]
    fn a_bus_is_its_thresholded_bits(
        bits in prop::collection::vec(prop::collection::vec(-8i32..8, 12), 1..6),
        thr in -8i32..8,
    ) {
        let x: Vec<f64> = (0..12).map(f64::from).collect();
        let names: Vec<String> = (0..bits.len()).map(|k| format!("v(b{k})")).collect();
        let cols: Vec<(&str, Vec<f64>)> = names
            .iter()
            .zip(&bits)
            .map(|(n, b)| (n.as_str(), b.iter().map(|&v| f64::from(v)).collect()))
            .collect();
        let mut w = Waves::new(vec![dataset(&x, &cols)]);
        let t = f64::from(thr) + 0.5;
        set(&mut w, 0, |p| p.threshold = t).unwrap();
        let src = format!("{{{}}}", names.join(", "));
        prop_assert_eq!(add(&mut w, &src, 0), Ok(Applied::Trace(0)));
        prop_assert_eq!(usize::from(w.bus_bits(0)), bits.len());
        let want = |t: f64, k: usize| {
            bits.iter().fold(0u32, |acc, b| (acc << 1) | u32::from(f64::from(b[k]) >= t))
        };
        for (k, (&xk, &yk)) in x.iter().zip(w.xy(0).1).enumerate() {
            prop_assert_eq!(yk, f64::from(want(t, k)));
            prop_assert_eq!(w.logic_at(0, xk), Some(want(t, k)));
        }
        // A new threshold re-evaluates the bus.
        set(&mut w, 0, |p| p.threshold = t - 1.0).unwrap();
        for (k, &yk) in w.xy(0).1.iter().enumerate() {
            prop_assert_eq!(yk, f64::from(want(t - 1.0, k)));
        }
    }
}

#[test]
fn bus_values_hold_between_samples_and_follow_their_pane() {
    let x = [0.0, 1.0, 2.0];
    let mut w = Waves::new(vec![dataset(
        &x,
        &[("v(a)", vec![0.0, 1.0, 1.0]), ("v(b)", vec![1.0, 0.0, 1.0])],
    )]);
    add(&mut w, "{v(a), v(b)}", 0).unwrap();
    assert_eq!(w.xy(0).1, &[1.0, 2.0, 3.0]);
    assert_eq!(w.value_at(0, 0.9), Some(1.0), "held, not interpolated");
    assert_eq!(w.value_at(0, 2.0), Some(3.0));
    // Moving it to a pane with another threshold re-evaluates it.
    w.apply(&Request::AddPane).unwrap();
    set(&mut w, 1, |p| p.threshold = 2.0).unwrap();
    w.apply(&Request::MoveTrace { trace: 0, pane: 1 }).unwrap();
    assert_eq!(w.xy(0).1, &[0.0, 0.0, 0.0]);
    // Malformed buses are refused.
    for bad in ["{}", "{v(a),}", "{v(a), 1}", "{v(nope)}"] {
        assert!(add(&mut w, bad, 0).is_err(), "{bad}");
    }
    let wide = format!("{{{}}}", ["v(a)"; 33].join(","));
    assert!(matches!(add(&mut w, &wide, 0), Err(ViewError::Bus(_))));
    assert_eq!(w.trace_count(), 1);
}

#[test]
fn fft_traces_get_their_own_frequency_axis() {
    let mut w = Waves::new(fixture("rc_tran.bin.raw"));
    add(&mut w, "v(out)", 0).unwrap();
    let time = w.x();
    // Into a pane that shows time: a new unlocked pane, fitted to the bins.
    assert_eq!(add(&mut w, "fft(v(out))", 0), Ok(Applied::Trace(1)));
    assert_eq!((w.panes().len(), w.pane_of(1)), (2, 1));
    assert!(w.has_own_axis(1) && w.props(1).own_x() && !w.props(1).locked);
    let f = w.x_of(1);
    let bins = w.xy(1).0;
    assert_eq!((f.lo, f.hi), (bins[0], bins[bins.len() - 1]));
    assert_eq!(w.x(), time, "the time axis does not move");
    // Full zoom fits each axis to its own traces.
    w.apply(&Request::ZoomX {
        pane: 1,
        at: 0.5,
        factor: 0.1,
    })
    .unwrap();
    assert_eq!(w.x(), time);
    w.apply(&Request::Autoscale).unwrap();
    assert_eq!((w.x(), w.x_of(1)), (time, f));
    // It cannot join or turn into a locked pane.
    let before = (w.pane_of(1), w.props(1));
    assert_eq!(
        w.apply(&Request::MoveTrace { trace: 1, pane: 0 }),
        Err(ViewError::OwnAxis)
    );
    assert_eq!(set(&mut w, 1, |p| p.locked = true), Err(ViewError::OwnAxis));
    assert_eq!((w.pane_of(1), w.props(1)), before);
    // Into an empty locked pane: that pane unlocks.
    let mut w = Waves::new(fixture("rc_tran.bin.raw"));
    add(&mut w, "fft(v(in))", 0).unwrap();
    assert_eq!(w.panes().len(), 1);
    assert!(!w.props(0).locked);
}

#[test]
fn per_step_results_of_a_sweep_are_traces_on_the_step_axis() {
    let mut w = Waves::new(group_datasets(fixture("dc_step.bin.raw")));
    let steps = w.datasets()[0].steps().len();
    assert!(steps > 1);
    assert_eq!(add(&mut w, "ymax(v(a))", 0), Ok(Applied::Trace(0)));
    assert!(w.has_own_axis(0) && !w.props(0).locked);
    let (x, y) = w.xy(0);
    assert_eq!((x.len(), y.len()), (steps, steps));
    assert_eq!(x[steps - 1], (steps - 1) as f64, "x is the step index");
}

#[test]
fn unlocked_panes_keep_their_own_x() {
    let mut w = Waves::new(fixture("rc_tran.bin.raw"));
    w.apply(&Request::AddPane).unwrap();
    add(&mut w, "v(in)", 0).unwrap();
    add(&mut w, "v(out)", 1).unwrap();
    set(&mut w, 1, |p| p.locked = false).unwrap();
    let shared = w.x();
    assert_eq!(w.x_of(1), shared, "unlocking starts from the shared view");
    w.apply(&Request::ZoomX {
        pane: 1,
        at: 0.0,
        factor: 0.5,
    })
    .unwrap();
    assert_eq!(w.x(), shared);
    assert!((w.x_of(1).hi - (shared.lo + 0.5 * (shared.hi - shared.lo))).abs() < 1e-15);
    w.apply(&Request::PanX { pane: 0, by: 0.25 }).unwrap();
    assert_ne!(w.x(), shared);
    assert!(w.x_of(1).hi < shared.hi, "pane 1 did not follow");
    // Typed ranges land on the axis the pane shows; invalid ones are refused.
    w.apply(&Request::SetX {
        pane: 1,
        lo: 1e-6,
        hi: 2e-6,
    })
    .unwrap();
    assert_eq!((w.x_of(1).lo, w.x_of(1).hi), (1e-6, 2e-6));
    for (lo, hi) in [(2.0, 1.0), (f64::NAN, 1.0), (1.0, 1.0)] {
        let r = Request::SetY { pane: 0, lo, hi };
        assert!(
            matches!(w.apply(&r), Err(ViewError::Range(..))),
            "{lo} {hi}"
        );
    }
    // Locking again shows the shared axis.
    set(&mut w, 1, |p| p.locked = true).unwrap();
    assert_eq!(w.x_of(1), w.x());
}

#[test]
fn histogram_counts_every_sample_and_fits_its_axes() {
    let x: Vec<f64> = (0..100).map(f64::from).collect();
    let y: Vec<f64> = (0..100).map(|k| f64::from(k % 10)).collect();
    let mut w = Waves::new(vec![dataset(&x, &[("v(y)", y)])]);
    add(&mut w, "v(y)", 0).unwrap();
    set(&mut w, 0, |p| {
        p.mode = PaneMode::Histogram;
        p.bins = 10;
    })
    .unwrap();
    let xa = w.x_of(0);
    assert_eq!((xa.lo, xa.hi), (0.0, 9.0), "x is the value extent");
    let mut counts = Vec::new();
    w.histogram(0, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 100);
    // Ten values over ten bins of width 0.9: one value per bin, ten samples each.
    assert_eq!(counts, vec![10; 10]);
    assert_eq!((w.panes()[0].lo, w.panes()[0].hi), (0.0, 10.0 * 1.05));
    // Bins outside the domain are refused and change nothing.
    let before = w.props(0);
    assert_eq!(set(&mut w, 0, |p| p.bins = 1), Err(ViewError::Props));
    assert_eq!(
        set(&mut w, 0, |p| p.bins = schemify_wave::MAX_BINS + 1),
        Err(ViewError::Props)
    );
    assert_eq!(
        set(&mut w, 0, |p| p.threshold = f64::INFINITY),
        Err(ViewError::Props)
    );
    assert_eq!(w.props(0), before);
}

#[test]
fn tracking_picks_the_step_under_the_pointer() {
    // Step s is y = 10 s + x over x = 0..4.
    let (mut x, mut y) = (Vec::new(), Vec::new());
    for s in 0..4 {
        for k in 0..5 {
            x.push(f64::from(k));
            y.push(10.0 * f64::from(s) + f64::from(k));
        }
    }
    let mut w = Waves::new(vec![dataset(&x, &[("v(y)", y)])]);
    add(&mut w, "v(y)", 0).unwrap();
    assert_eq!(w.steps(0).len(), 4);
    assert_eq!(w.closest_step(0, 2.5, 21.0), Some(2));
    assert_eq!(w.closest_step(0, 2.5, -100.0), Some(0));
    assert_eq!(w.closest_step(0, 9.0, 21.0), None, "no step covers x");
    assert_eq!(w.closest_step(5, 2.5, 21.0), None, "no such pane");
    set(&mut w, 0, |p| p.step = Some(2)).unwrap();
    assert_eq!(w.value_at(0, 1.0), Some(21.0));
    set(&mut w, 0, |p| p.step = Some(7)).unwrap();
    assert_eq!(w.shown_steps(0), 4..4, "a missing step shows nothing");
    assert_eq!(w.value_at(0, 1.0), None);
}

#[test]
fn horizontal_cursors_colours_and_their_refusals() {
    let mut w = Waves::new(fixture("rc_tran.bin.raw"));
    add(&mut w, "v(in)", 0).unwrap();
    let h = |which, at| Request::HCursor { pane: 0, which, at };
    w.apply(&h(Cursor::A, Some(0.25))).unwrap();
    w.apply(&h(Cursor::B, Some(f64::NAN))).unwrap();
    assert_eq!(w.hcursors(0), [Some(0.25), None]);
    w.apply(&h(Cursor::A, None)).unwrap();
    assert_eq!(w.hcursors(0), [None, None]);
    let bad = Request::HCursor {
        pane: 3,
        which: Cursor::A,
        at: Some(1.0),
    };
    assert_eq!(w.apply(&bad), Err(ViewError::NoPane(3)));
    w.apply(&Request::SetColor {
        trace: 0,
        color: 0x1234_56ff,
    })
    .unwrap();
    assert_eq!(w.color(0), 0x1234_56ff);
    let bad = Request::SetColor { trace: 4, color: 0 };
    assert_eq!(w.apply(&bad), Err(ViewError::NoTrace(4)));
    // Removing a pane removes its cursors and properties with it.
    w.apply(&Request::AddPane).unwrap();
    w.apply(&Request::HCursor {
        pane: 1,
        which: Cursor::B,
        at: Some(3.0),
    })
    .unwrap();
    w.apply(&Request::RemovePane { pane: 0 }).unwrap();
    assert_eq!(w.hcursors(0), [None, Some(3.0)]);
}

#[test]
fn reload_keeps_traces_that_still_evaluate() {
    let mut w = Waves::new(fixture("rc_tran.bin.raw"));
    add(&mut w, "v(out)", 0).unwrap();
    add(&mut w, "2 * v(out)", 0).unwrap();
    add(&mut w, "{v(in), v(out)}", 0).unwrap();
    let before: Vec<Vec<f64>> = (0..3).map(|i| w.xy(i).1.to_vec()).collect();
    let x = w.x();
    assert!(w.reload(fixture("rc_tran.bin.raw")).is_empty());
    assert_eq!(w.x(), x, "the view stays");
    for (i, b) in before.iter().enumerate() {
        assert_eq!(w.xy(i).1, &b[..], "{}", w.src(i));
    }
    // A dataset without `v(out)`: the traces that read it are dropped and named.
    let t = [0.0, 1.0];
    let dropped = w.reload(vec![dataset(&t, &[("v(in)", vec![0.0, 1.0])])]);
    assert_eq!(dropped.len(), 3, "{dropped:?}");
    assert!(dropped[0].starts_with("v(out): "), "{dropped:?}");
    assert_eq!(w.trace_count(), 0);
}
