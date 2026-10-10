//! Fuzz target: any UTF-8 text through the lexer, parser and compiler in
//! both modes and as a template, then evaluation with every identifier and
//! signal bound. The oracle is "no panic": every failure must be a declared
//! `X0xx` error (the crate's internal asserts fire otherwise).
//!
//! Run (nightly): `cargo fuzz run compile` from `crates/expr`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use schemify_expr::{Column, Mode, Plot, Program, Scratch, Template, Value};

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let mut s = Scratch::default();
    if let Ok(p) = Program::compile(src, Mode::Scalar) {
        let inputs: Vec<Value> = p.idents().map(|_| Value::Num(1.5)).collect();
        let _ = p.eval(&inputs, &mut s);
    }
    if let Ok(p) = Program::compile(src, Mode::Calc) {
        let inputs: Vec<Value> = p.idents().map(|_| Value::Int(2)).collect();
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [1.0, -2.0, 0.5, 4.0];
        let cols: Vec<Column> = p.signals().map(|_| Column { re: &y, im: &[] }).collect();
        let plot = Plot {
            x: &x,
            steps: &[0..2, 2..4],
            signals: &cols,
        };
        let _ = p.eval_calc_plain(&inputs, &plot, &mut s);
    }
    if let Ok(t) = Template::parse(src) {
        let inputs: Vec<Value> = t.program().idents().map(|_| Value::Str("n1")).collect();
        let _ = t.render(&inputs, &mut s, &mut String::new());
    }
});
