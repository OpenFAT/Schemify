//! Oracle 2 (docs/dev/testing.md): calculator metamorphic relations on real simulator output, through
//! the raw binding (names, steps, complex columns), for both ngspice and Xyce data.

use schemify_expr::{CalcResult, ColKind, ColOut, Mode, Program, Scratch};
use schemify_wave::{RawPlot, calc, group_datasets};

use crate::{close, fixture};

fn col(src: &str, p: &RawPlot) -> ColOut {
    let prog = Program::compile(src, Mode::Calc).unwrap_or_else(|e| panic!("{src}: {e}"));
    let mut s = Scratch::default();
    match calc::eval(&prog, p, |_| None, &mut s).unwrap_or_else(|e| panic!("{src}: {e}")) {
        CalcResult::Column(c) => c,
        other => panic!("{src}: expected a column, got {other:?}"),
    }
}

/// Largest |y| per step, so tolerances scale with each step's amplitude.
fn step_scale(y: &[f64], p: &RawPlot) -> Vec<f64> {
    p.steps()
        .iter()
        .map(|s| {
            y[s.start as usize..s.end as usize]
                .iter()
                .fold(0.0f64, |m, v| m.max(v.abs()))
        })
        .collect()
}

/// `integ(deriv(y)) ≈ y − y0` per sweep step. The residue is the schemes' truncation error:
/// first order at a step's ends (one-sided differences there), second order inside, so it is
/// bounded by the coarsest grid spacing times the steepest slope of the step.
fn integ_deriv(p: &RawPlot, y: &str) {
    let back = col(&format!("integ(deriv({y}))"), p);
    let slope = col(&format!("deriv({y})"), p);
    let orig = col(y, p);
    let x = p.scale();
    for (k, s) in p.steps().iter().enumerate() {
        let (a, b) = (s.start as usize, s.end as usize);
        let y0 = orig.re[a];
        let worst = (a..b)
            .map(|i| (back.re[i] - (orig.re[i] - y0)).abs())
            .fold(0.0f64, f64::max);
        let h = (a + 1..b)
            .map(|i| (x[i] - x[i - 1]).abs())
            .fold(0.0f64, f64::max);
        let d = slope.re[a..b].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let amp = step_scale(&orig.re, p)[k];
        assert!(
            worst <= h * d && worst <= 0.05 * amp,
            "{y} step {k}: residue {worst:e} > h·max|y'| = {:e} (amplitude {amp:e})",
            h * d
        );
    }
}

#[test]
fn integ_of_deriv_recovers_the_signal() {
    // Smooth diode curves, five and three sweep steps (grouped blocks).
    let ng = group_datasets(fixture("dc_step.bin.raw"));
    integ_deriv(&ng[0], "v(a)");
    integ_deriv(&ng[0], "i(v1)");
    let xy = group_datasets(fixture("dc_step.xyce.bin.raw"));
    integ_deriv(&xy[0], "v(a)");
    // RC transient: 10 ns edges on a 50 ns grid.
    integ_deriv(&fixture("rc_tran.bin.raw")[0], "v(out)");
    integ_deriv(&fixture("rc_tran.xyce.bin.raw")[0], "v(out)");
}

#[test]
fn linear_combinations_commute_with_evaluation() {
    for (file, a, b) in [
        ("rc_tran.bin.raw", "v(in)", "v(out)"),
        ("rc_tran.xyce.ascii.raw", "v(in)", "i(v1)"),
        ("dc_step.ascii.raw", "v(a)", "i(v1)"),
    ] {
        let p = &fixture(file)[0];
        let (ya, yb) = (col(a, p), col(b, p));
        let lin = col(&format!("2.5 * {a} - 3 * {b} + 1"), p);
        let d = col(&format!("deriv({a} + {b})"), p);
        let (da, db) = (
            col(&format!("deriv({a})"), p),
            col(&format!("deriv({b})"), p),
        );
        // Rounding of `a + b` before differencing is amplified by 1/Δx.
        let mag = ya
            .re
            .iter()
            .chain(&yb.re)
            .fold(0.0f64, |m, v| m.max(v.abs()));
        let x = p.scale();
        for i in 0..p.len() {
            let want = 2.5 * ya.re[i] - 3.0 * yb.re[i] + 1.0;
            let scale = 2.5 * ya.re[i].abs() + 3.0 * yb.re[i].abs() + 1.0;
            assert!(
                (lin.re[i] - want).abs() <= 1e-15 * scale,
                "{file} point {i}"
            );
            let dx = x[(i + 1).min(p.len() - 1)] - x[i.saturating_sub(1)];
            let tol = 8.0 * f64::EPSILON * mag / dx.abs();
            let dsum = da.re[i] + db.re[i];
            assert!((d.re[i] - dsum).abs() <= tol, "{file} deriv {i}");
        }
    }
}

#[test]
fn kirchhoff_holds_through_the_name_binding() {
    // Current into V1's + terminal is minus the resistor current: (v(in) - v(out))/1k + i(v1) = 0.
    // ngspice names the branch `i(v1)`, Xyce `V1#branch`; both resolve from `i(v1)`.
    for file in [
        "rc_tran.bin.raw",
        "rc_tran.xyce.bin.raw",
        "rc_tran.xyce.ascii.raw",
    ] {
        let p = &fixture(file)[0];
        let kcl = col("(v(in) - v(out)) / 1k + i(v1)", p);
        let worst = kcl.re.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        // ASCII Xyce keeps 9 digits of a ~1 mA current.
        assert!(worst < 1e-10, "{file}: KCL residue {worst:e}");
    }
}

#[test]
fn complex_columns_bind_and_reduce() {
    for file in ["rlc_ac.bin.raw", "rlc_ac.xyce.ascii.raw"] {
        let p = &fixture(file)[0];
        let h = col("v(out) / v(in)", p);
        assert_eq!(h.kind, ColKind::Complex);
        let db = col("db(v(out))", p);
        let mag = col("mag(v(out))", p);
        let re = col("re(v(out) / v(in))", p);
        for i in 0..p.len() {
            assert!(
                close(db.re[i], 20.0 * mag.re[i].log10(), 1e-9),
                "{file} {i}"
            );
            assert!(close(h.re[i], re.re[i], 1e-12), "{file} {i}");
        }
        // Q = 1, so the capacitor voltage peaks at f0·√(1 − 1/2Q²) = 0.707 · 159 kHz.
        let peak = (0..p.len())
            .max_by(|&a, &b| mag.re[a].total_cmp(&mag.re[b]))
            .unwrap();
        let f = p.scale()[peak];
        let f_peak = 0.5f64.sqrt() / (2.0 * std::f64::consts::PI * (10e-6f64 * 100e-9).sqrt());
        assert!(
            (f / f_peak).log10().abs() < 0.5 / 20.0,
            "{file}: peak at {f} Hz, not {f_peak}"
        );
    }
}
