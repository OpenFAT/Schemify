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
    if !cfg!(debug_assertions) {
        assert!(dt.as_millis() < 200, "scalar evaluation too slow: {dt:?}");
    }
}
