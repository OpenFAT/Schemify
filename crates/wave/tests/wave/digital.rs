//! Digital lanes and histogram bins (spec waves §4, §5): `logic_runs` against a naive per-column
//! oracle, metamorphic relations of thresholding, and `bin_counts` conservation properties.

use proptest::prelude::*;
use schemify_wave::{Run, Scale, bin_counts, logic, logic_runs};

/// Naive oracle of [`logic_runs`]: every column scans every sample.
fn naive_runs(xs: &[f64], ys: &[f64], thr: Option<f64>, sx: &Scale) -> Vec<Run> {
    let cols = ((sx.p1 - sx.p0).floor() as usize).max(1);
    let edge = |c: usize| sx.value(sx.p0 + c as f32);
    let mut out: Vec<Run> = Vec::new();
    for c in 0..cols {
        let (e0, e1) = (edge(c), edge(c + 1));
        let mut vals = Vec::new();
        // Held into the column: the last sample before it, when a later sample exists.
        if let Some(k) = (0..xs.len()).rev().find(|&k| xs[k] < e0)
            && xs.iter().any(|&x| x >= e0)
        {
            vals.push(logic(ys[k], thr));
        }
        for (&x, &y) in xs.iter().zip(ys) {
            if x >= e0 && x < e1 {
                vals.push(logic(y, thr));
            }
        }
        let Some(&v0) = vals.first() else { continue };
        let value = vals.iter().all(|&v| v == v0).then_some(v0);
        let c = c as u32;
        if out.last().is_some_and(|r| r.c1 == c && r.value == value) {
            out.last_mut().unwrap().c1 = c + 1;
        } else {
            out.push(Run {
                c0: c,
                c1: c + 1,
                value,
            });
        }
    }
    out
}

/// Samples on a 1/64 grid with ascending x (repeats allowed), so scaling by powers of two and
/// offsets on the grid are exact, and thresholds on the 1/128 half-grid are never hit.
fn lane() -> impl Strategy<Value = (Vec<f64>, Vec<f64>)> {
    prop::collection::vec((0u8..5, -64i32..64), 1..300).prop_map(|s| {
        let mut t = 0.0;
        let (mut x, mut y) = (Vec::new(), Vec::new());
        for (dx, v) in s {
            t += f64::from(dx) / 64.0;
            x.push(t);
            y.push(f64::from(v) / 64.0);
        }
        (x, y)
    })
}

fn scale_over(x: &[f64], from: f64, to: f64, px: f32) -> Scale {
    let (lo, hi) = (x[0], x[x.len() - 1].max(x[0] + 1.0));
    Scale::new(
        lo + from * (hi - lo),
        lo + to * (hi - lo),
        false,
        3.0,
        3.0 + px,
    )
}

proptest! {
    #[test]
    fn logic_runs_equal_the_naive_oracle(
        (x, y) in lane(),
        thr in -64i32..64,
        view in (-0.2f64..0.5, 0.6f64..1.3),
        px in 1.0f32..400.0,
        bus: bool,
    ) {
        let sx = scale_over(&x, view.0, view.1, px);
        let t = (!bus).then_some(f64::from(thr) / 64.0 + 1.0 / 128.0);
        // A bus column holds non-negative integers.
        let y: Vec<f64> = if bus { y.iter().map(|v| (v * 64.0).abs().floor()).collect() } else { y };
        let mut got = vec![Run { c0: 9, c1: 9, value: None }]; // appends after what is there
        logic_runs(&x, &y, t, &sx, &mut got);
        prop_assert_eq!(&got[1..], &naive_runs(&x, &y, t, &sx)[..]);
        // Runs never overlap and cover only columns with data.
        prop_assert!(got[1..].windows(2).all(|w| w[0].c1 <= w[1].c0));
    }

    #[test]
    fn thresholding_is_invariant_under_shared_scaling_and_offset(
        (x, y) in lane(),
        thr in -64i32..64,
        k in 0usize..4,
        off in -64i32..64,
        px in 1.0f32..300.0,
    ) {
        let sx = scale_over(&x, 0.0, 1.0, px);
        let t = f64::from(thr) / 64.0 + 1.0 / 128.0;
        let runs = |y: &[f64], t: f64| {
            let mut out = Vec::new();
            logic_runs(&x, y, Some(t), &sx, &mut out);
            out
        };
        let base = runs(&y, t);
        // Scaling by a power of two and shifting by a grid step are exact in f64.
        let g = [0.25, 0.5, 2.0, 1024.0][k];
        let c = f64::from(off) / 64.0;
        let scaled: Vec<f64> = y.iter().map(|v| v * g).collect();
        prop_assert_eq!(&runs(&scaled, t * g), &base);
        let shifted: Vec<f64> = y.iter().map(|v| v + c).collect();
        prop_assert_eq!(&runs(&shifted, t + c), &base);
        // Negating signal and threshold complements every bit (no sample sits on the threshold).
        let neg: Vec<f64> = y.iter().map(|v| -v).collect();
        let flipped: Vec<Run> = base
            .iter()
            .map(|r| Run { value: r.value.map(|v| 1 - v), ..*r })
            .collect();
        prop_assert_eq!(&runs(&neg, -t), &flipped);
    }

    #[test]
    fn bins_conserve_and_add_up(
        a in prop::collection::vec(-100.0f64..100.0, 0..200),
        b in prop::collection::vec(-100.0f64..100.0, 0..200),
        range in (-80.0f64..0.0, 1.0f64..80.0),
        n in 1usize..50,
        log: bool,
    ) {
        let (t0, t1) = if log { (-1.0, range.1.log10().max(0.5)) } else { range };
        let count = |y: &[f64]| {
            let mut bins = vec![0u32; n];
            bin_counts(y, t0, t1, log, &mut bins);
            bins
        };
        let (ca, cb) = (count(&a), count(&b));
        // Conservation: every sample inside the range lands in exactly one bin.
        let inside = |y: &[f64]| {
            y.iter()
                .map(|&v| if log { v.log10() } else { v })
                .filter(|&t| t >= t0 && t <= t1)
                .count() as u32
        };
        prop_assert_eq!(ca.iter().sum::<u32>(), inside(&a));
        // Additivity over concatenation, invariance under reordering.
        let ab: Vec<f64> = a.iter().chain(&b).copied().collect();
        let sum: Vec<u32> = ca.iter().zip(&cb).map(|(p, q)| p + q).collect();
        prop_assert_eq!(count(&ab), sum);
        let mut rev = a.clone();
        rev.reverse();
        prop_assert_eq!(count(&rev), ca);
    }
}

#[test]
fn bins_place_centres_exactly_and_ends_inside() {
    let mut bins = [0u32; 4];
    // Centres of four unit bins over [0, 4], both ends, and values outside.
    bin_counts(
        &[0.5, 1.5, 2.5, 3.5, 0.0, 4.0, -0.1, 4.1, f64::NAN],
        0.0,
        4.0,
        false,
        &mut bins,
    );
    assert_eq!(bins, [2, 1, 1, 2]);
    // On a log axis, decades are bins and values <= 0 are not counted.
    let mut bins = [0u32; 3];
    bin_counts(&[2.0, 20.0, 200.0, 0.0, -5.0], 0.0, 3.0, true, &mut bins);
    assert_eq!(bins, [1, 1, 1]);
}

#[test]
fn busy_columns_and_gaps() {
    // A clock much faster than a pixel column is busy; columns past the data are left out.
    let x: Vec<f64> = (0..1000).map(|k| f64::from(k) * 1e-3).collect();
    let y: Vec<f64> = (0..1000).map(|k| f64::from(k % 2)).collect();
    let sx = Scale::new(0.0, 2.0, false, 0.0, 20.0);
    let mut runs = Vec::new();
    logic_runs(&x, &y, Some(0.5), &sx, &mut runs);
    assert_eq!(
        runs,
        vec![Run {
            c0: 0,
            c1: 10,
            value: None
        }]
    );
    // A constant level is one run, up to the column of its last sample (x = 1 starts column 10).
    let mut runs = Vec::new();
    logic_runs(&[0.0, 1.0], &[3.0, 3.0], None, &sx, &mut runs);
    assert_eq!(
        runs,
        vec![Run {
            c0: 0,
            c1: 11,
            value: Some(3)
        }]
    );
    let mut runs = Vec::new();
    logic_runs(&[0.5], &[1.0], Some(0.5), &sx, &mut runs);
    assert_eq!(runs.len(), 1, "the sample's own column");
    logic_runs(&[1.0, 0.5], &[1.0, 1.0], Some(0.5), &sx, &mut runs);
    assert_eq!(runs.len(), 1, "a descending step draws nothing");
}
