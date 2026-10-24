//! Hot-path budget: a ~25-node scalar expression evaluated 1M times in
//! under 200 ms (release). Ignored by default because timing needs release:
//! `cargo test -p schemify-expr --release -- --ignored perf`.

use std::hint::black_box;
use std::time::Instant;

use schemify_expr::{Mode, Program, Scratch, Value};

#[test]
#[ignore = "timing; run in release with --ignored"]
fn perf_scalar_eval_1m() {
    let src = "w * l * 2 + (a - b) / c + sqrt(x * x + y * y) - min(a, b) * 1.5k";
    let p = Program::compile(src, Mode::Scalar).unwrap();
    assert_eq!(p.idents().count(), 7);
    let mut inputs = [1e-6, 0.15e-6, 3.0, 1.0, 2.0, 0.3, 0.4].map(Value::Num);
    let mut s = Scratch::default();
    let t = Instant::now();
    let mut acc = 0.0;
    for i in 0..1_000_000 {
        inputs[0] = Value::Num(1e-6 + i as f64 * 1e-12);
        match p.eval(black_box(&inputs), &mut s) {
            Ok(Value::Num(v)) => acc += v,
            other => panic!("{other:?}"),
        }
    }
    let dt = t.elapsed();
    black_box(acc);
    eprintln!(
        "1M evaluations of `{src}`: {dt:?} ({:.1} ns each)",
        dt.as_nanos() as f64 / 1e6
    );
    eprintln!("PERF expr.scalar_eval_1m_ms {:.3}", dt.as_secs_f64() * 1e3);
    if !cfg!(debug_assertions) {
        assert!(dt.as_millis() < 200, "scalar evaluation too slow: {dt:?}");
    }
}

/// Calculator column arithmetic (`v(a)*2 + v(b)/3 - 1`) over 1M points, best of 20.
/// No budget (D39 names none); it prints the number so the column fast path is measured.
#[test]
#[ignore = "timing; run in release with --ignored"]
fn perf_calc_column_arith_1m() {
    use schemify_expr::{CalcResult, Column, Plot};
    let n = 1_000_000;
    let x: Vec<f64> = (0..n).map(|i| i as f64 * 1e-9).collect();
    let a: Vec<f64> = (0..n).map(|i| (i as f64 * 1e-3).sin()).collect();
    let b: Vec<f64> = (0..n).map(|i| (i as f64 * 7e-4).cos()).collect();
    let p = Program::compile("v(a) * 2 + v(b) / 3 - 1", Mode::Calc).unwrap();
    let signals = [Column { re: &a, im: &[] }, Column { re: &b, im: &[] }];
    let plot = Plot {
        x: &x,
        steps: &[],
        signals: &signals,
    };
    let mut s = Scratch::default();
    let mut best = f64::INFINITY;
    for _ in 0..20 {
        let t = Instant::now();
        match p.eval_calc_plain(&[], black_box(&plot), &mut s) {
            Ok(CalcResult::Column(c)) => black_box(c.re[n / 2]),
            other => panic!("{other:?}"),
        };
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("PERF expr.calc_column_arith_1m_ms {best:.3}");
}
