//! Calculator mode on analytic signals, with tolerances from the grid.

use std::f64::consts::PI;
use std::ops::Range;

use schemify_expr::{
    CalcResult, Code, ColKind, ColOut, Column, Mode, Plot, Program, Scratch, Value,
};

/// Evaluates `src` over signals `v(a)`, `v(b)`… (named by position) on `x`.
fn calc(
    src: &str,
    x: &[f64],
    steps: &[Range<u32>],
    sigs: &[(&[f64], &[f64])],
) -> Result<CalcResult<'static>, Code> {
    let p = Program::compile(src, Mode::Calc).map_err(|e| e.code)?;
    let names = ["a", "b", "h"];
    let cols: Vec<Column> = p
        .signals()
        .map(|(_, n)| {
            let k = names.iter().position(|m| *m == n).expect("known signal");
            Column {
                re: sigs[k].0,
                im: sigs[k].1,
            }
        })
        .collect();
    let plot = Plot {
        x,
        steps,
        signals: &cols,
    };
    let mut s = Scratch::default();
    let inputs: Vec<Value> = p.idents().map(|_| Value::Num(0.5)).collect();
    Ok(
        match p
            .eval_calc_plain(&inputs, &plot, &mut s)
            .map_err(|e| e.code)?
        {
            CalcResult::Column(c) => CalcResult::Column(c),
            CalcResult::Scalar(Value::Str(t)) => panic!("unexpected string {t:?}"),
            CalcResult::Scalar(Value::Int(v)) => CalcResult::Scalar(Value::Int(v)),
            CalcResult::Scalar(Value::Num(v)) => CalcResult::Scalar(Value::Num(v)),
            CalcResult::Scalar(Value::Bool(v)) => CalcResult::Scalar(Value::Bool(v)),
            CalcResult::Scalar(Value::Cnum { re, im }) => {
                CalcResult::Scalar(Value::Cnum { re, im })
            }
        },
    )
}

fn col(r: Result<CalcResult<'static>, Code>) -> ColOut {
    match r {
        Ok(CalcResult::Column(c)) => c,
        other => panic!("expected a column, got {other:?}"),
    }
}

fn grid(n: usize, t_end: f64) -> Vec<f64> {
    (0..n).map(|i| t_end * i as f64 / (n - 1) as f64).collect()
}

const F: f64 = 3.0;

fn sine(x: &[f64]) -> Vec<f64> {
    x.iter().map(|t| (2.0 * PI * F * t).sin()).collect()
}

fn max_err(a: &[f64], b: impl Fn(usize) -> f64) -> f64 {
    a.iter()
        .enumerate()
        .map(|(i, v)| (v - b(i)).abs())
        .fold(0.0, f64::max)
}

#[test]
fn deriv_and_integ_of_sine() {
    let x = grid(4001, 1.0);
    let y = sine(&x);
    let w = 2.0 * PI * F;
    let d = col(calc("deriv(v(a))", &x, &[], &[(&y, &[])]));
    // Central differences: O(h²) inside, O(h) at the two ends.
    let inner = max_err(&d.re[1..x.len() - 1], |i| w * (w * x[i + 1]).cos());
    assert!(inner < 1e-3 * w, "deriv error {inner}");
    let integ = col(calc("integ(v(a))", &x, &[], &[(&y, &[])]));
    let err = max_err(&integ.re, |i| (1.0 - (w * x[i]).cos()) / w);
    assert!(err < 1e-5, "integ error {err}");
    // Metamorphic: integ(deriv(y)) ≈ y - y0.
    let back = col(calc(
        "integ(deriv(v(a))) - (v(a) - at(v(a), 0))",
        &x,
        &[],
        &[(&y, &[])],
    ));
    assert!(max_err(&back.re, |_| 0.0) < 2e-3, "integ(deriv(y)) drift");
    // Linearity: deriv(2a + 3) = 2 deriv(a).
    let lin = col(calc(
        "deriv(2 * v(a) + 3) - 2 * deriv(v(a))",
        &x,
        &[],
        &[(&y, &[])],
    ));
    assert!(max_err(&lin.re, |_| 0.0) < 1e-9);
}

#[test]
fn reductions() {
    let x = grid(10_001, 1.0);
    let y: Vec<f64> = sine(&x).iter().map(|v| 2.0 * v + 0.5).collect();
    let one = |src: &str| col(calc(src, &x, &[], &[(&y, &[])])).re[0];
    assert!((one("avg(v(a))") - 0.5).abs() < 1e-6);
    assert!((one("rms(v(a) - avg(v(a)))") - 2.0 / 2f64.sqrt()).abs() < 1e-6);
    assert!((one("ymax(v(a))") - 2.5).abs() < 1e-6);
    assert!((one("ymin(v(a))") + 1.5).abs() < 1e-6);
    assert!((one("pp(v(a))") - 4.0).abs() < 1e-6);
    let r = col(calc("avg(v(a))", &x, &[], &[(&y, &[])]));
    assert_eq!(
        r.axis.as_ref().map(|a| a.x.len()),
        Some(1),
        "one value per step, on the per-step axis"
    );
}

#[test]
fn at_cross_delay_settle() {
    let x = grid(101, 10.0);
    let ramp: Vec<f64> = x.iter().map(|t| 2.0 * t - 5.0).collect();
    let one = |src: &str, sigs: &[(&[f64], &[f64])]| col(calc(src, &x, &[], sigs)).re[0];
    assert!((one("at(v(a), 3.33)", &[(&ramp, &[])]) - (2.0 * 3.33 - 5.0)).abs() < 1e-12);
    assert!((one("cross(v(a), 0)", &[(&ramp, &[])]) - 2.5).abs() < 1e-12);
    assert_eq!(
        calc("cross(v(a), 0, -1)", &x, &[], &[(&ramp, &[])]).unwrap_err(),
        Code::X010,
        "no falling crossing"
    );
    assert_eq!(
        calc("at(v(a), 11)", &x, &[], &[(&ramp, &[])]).unwrap_err(),
        Code::X010,
        "outside the axis"
    );
    let shifted: Vec<f64> = x.iter().map(|t| 2.0 * (t - 1.5) - 5.0).collect();
    assert!((one("delay(v(a), 0, v(b), 0)", &[(&ramp, &[]), (&shifted, &[])]) - 1.5).abs() < 1e-12);

    let xs = grid(20_001, 1.0);
    let s = sine(&xs);
    let ones = |src: &str| col(calc(src, &xs, &[], &[(&s, &[])])).re[0];
    // sin(2π·3t) crosses 0.5 rising at t = (1/12 + k)/3.
    for (n, want) in [(1, 1.0 / 36.0), (2, 13.0 / 36.0), (3, 25.0 / 36.0)] {
        assert!(
            (ones(&format!("cross(v(a), 0.5, 1, {n})")) - want).abs() < 1e-6,
            "crossing {n}"
        );
    }
    assert!((ones("cross(v(a), 0.5, -1)") - 5.0 / 36.0).abs() < 1e-6);
    assert!((ones("cross(v(a), 0.5)") - 1.0 / 36.0).abs() < 1e-6);

    // First-order step: 1 - e^(-t/τ) settles within tol at τ·ln(1/tol).
    let tau = 0.1;
    let xe = grid(100_001, 2.0);
    let e: Vec<f64> = xe.iter().map(|t| 1.0 - (-t / tau).exp()).collect();
    let r = col(calc("settle(v(a), 0.01)", &xe, &[], &[(&e, &[])])).re[0];
    let final_dev = (-2.0f64 / tau).exp();
    let want = -tau * (0.01 + final_dev).ln();
    assert!((r - want).abs() < 1e-4, "settle {r} vs {want}");
}

#[test]
fn fft_finds_the_tone() {
    let n = 256;
    let x: Vec<f64> = (0..n).map(|i| i as f64 / n as f64).collect();
    let amp = 1.5;
    let y: Vec<f64> = x.iter().map(|t| amp * (2.0 * PI * 8.0 * t).cos()).collect();
    let r = col(calc("mag(fft(v(a)))", &x, &[], &[(&y, &[])]));
    let axis = r.axis.expect("fft makes a frequency axis");
    let peak = (0..r.re.len())
        .max_by(|&i, &j| r.re[i].total_cmp(&r.re[j]))
        .unwrap();
    let span = x[n - 1] - x[0];
    assert!(
        (axis.x[peak] - 8.0 / span).abs() < 1e-9,
        "peak at {}",
        axis.x[peak]
    );
    assert!((r.re[peak] - amp).abs() < 0.05, "amplitude {}", r.re[peak]);
    // fft(a) and fft(b) of one plot share the frequency axis.
    let both = col(calc("fft(v(a)) - fft(v(a))", &x, &[], &[(&y, &[])]));
    assert!(both.re.iter().chain(&both.im).all(|v| *v == 0.0));
    assert_eq!(both.kind, ColKind::Complex);
}

/// Loop gain A / ((1 + s/p1)(1 + s/p2)(1 + s/p3)) on a dense log sweep.
fn loop_gain(f: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let (a, p) = (1000.0, [1e3, 1e5, 1e6]);
    let h: Vec<(f64, f64)> = f
        .iter()
        .map(|&f| {
            let (mut re, mut im) = (a, 0.0);
            for pk in p {
                // divide by (1 + j f/pk)
                let (dr, di) = (1.0, f / pk);
                let d = dr * dr + di * di;
                (re, im) = ((re * dr + im * di) / d, (im * dr - re * di) / d);
            }
            (re, im)
        })
        .collect();
    (
        h.iter().map(|z| z.0).collect(),
        h.iter().map(|z| z.1).collect(),
    )
}

fn analytic_margins() -> (f64, f64) {
    let (a, p) = (1000.0f64, [1e3f64, 1e5, 1e6]);
    let mag = |f: f64| {
        a / p
            .iter()
            .map(|pk| (1.0 + (f / pk).powi(2)).sqrt())
            .product::<f64>()
    };
    let ph = |f: f64| -p.iter().map(|pk| (f / pk).atan()).sum::<f64>().to_degrees();
    let solve = |g: &dyn Fn(f64) -> f64| {
        let (mut lo, mut hi) = (1.0f64, 1e9f64);
        for _ in 0..200 {
            let mid = (lo * hi).sqrt();
            if g(mid) > 0.0 { lo = mid } else { hi = mid }
        }
        lo
    };
    let fu = solve(&|f| mag(f) - 1.0);
    let f180 = solve(&|f| ph(f) + 180.0);
    (180.0 + ph(fu), -20.0 * mag(f180).log10())
}

#[test]
fn stability_margins() {
    let f: Vec<f64> = (0..4001)
        .map(|i| 10f64.powf(i as f64 * 8.0 / 4000.0))
        .collect();
    let (re, im) = loop_gain(&f);
    let (pm, gm) = analytic_margins();
    let got_pm = col(calc(
        "phase_margin(v(h))",
        &f,
        &[],
        &[(&[], &[]), (&[], &[]), (&re, &im)],
    ))
    .re[0];
    let got_gm = col(calc(
        "gain_margin(v(h))",
        &f,
        &[],
        &[(&[], &[]), (&[], &[]), (&re, &im)],
    ))
    .re[0];
    assert!((got_pm - pm).abs() < 0.05, "PM {got_pm} vs {pm}");
    assert!((got_gm - gm).abs() < 0.05, "GM {got_gm} vs {gm}");
    // A gain that never reaches unity has no phase margin.
    let small: Vec<f64> = re.iter().map(|v| v * 1e-6).collect();
    let im_small: Vec<f64> = im.iter().map(|v| v * 1e-6).collect();
    assert_eq!(
        calc(
            "phase_margin(v(h))",
            &f,
            &[],
            &[(&[], &[]), (&[], &[]), (&small, &im_small)]
        )
        .unwrap_err(),
        Code::X010
    );
}

#[test]
fn per_step_semantics() {
    // Two sweep steps; values jump between them.
    let x = [0.0, 1.0, 2.0, 0.0, 1.0, 2.0];
    let y = [0.0, 1.0, 2.0, 100.0, 103.0, 106.0];
    let steps = [0..3, 3..6];
    let d = col(calc("deriv(v(a))", &x, &steps, &[(&y, &[])]));
    assert_eq!(
        d.re,
        [1.0, 1.0, 1.0, 3.0, 3.0, 3.0],
        "no spike across the step boundary"
    );
    let i = col(calc("integ(v(a))", &x, &steps, &[(&y, &[])]));
    assert_eq!(i.re, [0.0, 0.5, 2.0, 0.0, 101.5, 206.0]);
    let a = col(calc("avg(v(a))", &x, &steps, &[(&y, &[])]));
    assert_eq!(a.re, [1.0, 103.0]);
    assert_eq!(a.axis.unwrap().x, [0.0, 1.0]);
    // A per-step value broadcasts over its own steps.
    let centered = col(calc("v(a) - avg(v(a))", &x, &steps, &[(&y, &[])]));
    assert_eq!(centered.re, [-1.0, 0.0, 1.0, -3.0, 0.0, 3.0]);
    assert!(centered.axis.is_none());
    // Scalars broadcast; inputs come from bench vars.
    let s = col(calc("v(a) * k + 1", &x, &steps, &[(&y, &[])]));
    assert_eq!(s.re[4], 103.0 * 0.5 + 1.0);
}

#[test]
fn column_conditions_select_per_element() {
    let x = [0.0, 1.0, 2.0, 3.0];
    let y = [-2.0, 1.0, -0.5, 4.0];
    let r = col(calc("v(a) > 0 ? v(a) : -v(a)", &x, &[], &[(&y, &[])]));
    assert_eq!(r.re, [2.0, 1.0, 0.5, 4.0]);
    let b = col(calc("v(a) > 0 && v(a) < 2", &x, &[], &[(&y, &[])]));
    assert_eq!(
        (b.kind, b.re.clone()),
        (ColKind::Bool, vec![0.0, 1.0, 0.0, 0.0])
    );
    let b = col(calc("!(v(a) > 0) || x() == 3", &x, &[], &[(&y, &[])]));
    assert_eq!(b.re, [1.0, 0.0, 1.0, 1.0]);
    // A scalar condition still short-circuits: the column arm never runs.
    let r = calc("1 > 2 ? deriv(v(a)) : 7", &x, &[], &[(&y, &[])]).unwrap();
    assert_eq!(r, CalcResult::Scalar(Value::Int(7)));
    // Element-wise builtins and complex signals.
    let re = [3.0, 0.0];
    let im = [4.0, 1.0];
    let m = col(calc("mag(v(a))", &[0.0, 1.0], &[], &[(&re, &im)]));
    assert_eq!(m.re, [5.0, 1.0]);
    let p = col(calc("ph(v(a))", &[0.0, 1.0], &[], &[(&re, &im)]));
    assert!((p.re[1] - 90.0).abs() < 1e-12);
    // Uppercase V() is accepted (SPICE habit).
    assert_eq!(col(calc("V(a) + 1", &x, &[], &[(&y, &[])])).re[0], -1.0);
}

#[test]
fn calc_type_errors() {
    let x = [0.0, 1.0];
    let y = [1.0, 2.0];
    for src in [
        "deriv(1)",
        "avg(2)",
        "cross(v(a), v(a))",
        "str(v(a))",
        "fft(v(a) > 0)",
        "cross(v(a), 0, 2)",
        "settle(v(a), -1)",
    ] {
        assert_eq!(
            calc(src, &x, &[], &[(&y, &[])]).unwrap_err(),
            Code::X003,
            "`{src}`"
        );
    }
    assert_eq!(
        calc("v(a) / (v(a) - 1)", &x, &[], &[(&y, &[])]).unwrap_err(),
        Code::X010
    );
}

/// `op(inst, param)` is one number from the operating point, broadcast like a scalar
/// over the plot's columns; its name is `<inst>[<param>]`; bad arguments are `X003`,
/// scalar mode `X006`, and a column bound to it `X011`.
#[test]
fn op_is_one_operating_point_number() {
    use schemify_expr::{SignalKind, op_parts};
    let p = Program::compile(
        r#"op("x1/m2", "gm") / op(M1, id) + v(a) - v(a)"#,
        Mode::Calc,
    )
    .unwrap();
    let sigs: Vec<_> = p.signals().collect();
    assert_eq!(
        sigs,
        [
            (SignalKind::Op, "x1/m2[gm]"),
            (SignalKind::Op, "M1[id]"),
            (SignalKind::V, "a")
        ]
    );
    assert_eq!(op_parts("x[3]/m2[gm]"), ("x[3]/m2", "gm"));
    // The span of the first call naming each signal.
    let s0 = p.signal_span(0);
    assert_eq!((s0.start, s0.end), (0, 17));
    let x = [0.0, 1.0, 2.0];
    let a = [5.0, 6.0, 7.0];
    let (gm, id) = ([2e-3], [1e-4]);
    let cols = [
        Column { re: &gm, im: &[] },
        Column { re: &id, im: &[] },
        Column { re: &a, im: &[] },
    ];
    let plot = Plot {
        x: &x,
        steps: &[],
        signals: &cols,
    };
    let c = match p.eval_calc_plain(&[], &plot, &mut Scratch::default()) {
        Ok(CalcResult::Column(c)) => c,
        other => panic!("{other:?}"),
    };
    assert!(c.re.iter().all(|v| (v - 20.0).abs() < 1e-12), "{c:?}");
    // Alone it stays a scalar.
    let q = Program::compile(r#"op("M1", "gm")"#, Mode::Calc).unwrap();
    let plot1 = Plot {
        x: &x,
        steps: &[],
        signals: &cols[..1],
    };
    assert_eq!(
        q.eval_calc_plain(&[], &plot1, &mut Scratch::default()),
        Ok(CalcResult::Scalar(Value::Num(2e-3)))
    );
    // A column where one number belongs.
    let wrong = [Column { re: &a, im: &[] }];
    let plot2 = Plot {
        x: &x,
        steps: &[],
        signals: &wrong,
    };
    let e = q
        .eval_calc_plain(&[], &plot2, &mut Scratch::default())
        .unwrap_err();
    assert_eq!(e.code, Code::X011);
    for bad in [
        r#"op("M1", 1)"#,
        r#"op("", "gm")"#,
        r#"op("M1", "g m")"#,
        r#"op(1+2, "gm")"#,
    ] {
        let e = Program::compile(bad, Mode::Calc).unwrap_err();
        assert_eq!(e.code, Code::X003, "{bad}");
    }
    assert_eq!(
        Program::compile(r#"op("M1", "gm")"#, Mode::Scalar)
            .unwrap_err()
            .code,
        Code::X006
    );
    assert_eq!(
        Program::compile(r#"op("M1")"#, Mode::Calc)
            .unwrap_err()
            .code,
        Code::X008
    );
}
