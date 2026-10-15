//! Oracle-backed tests for `schemify-expr` (docs/dev/testing.md, expr row). One test
//! binary; one module per oracle.

mod calc;
mod differential;
mod errors;
mod graph;
mod ngspice;
mod perf;
mod reference;
mod semantics;
mod template;
mod text;

use schemify_expr::{Code, Mode, Program, Scratch, Value};

/// Compiles and evaluates a closed scalar expression; the value is owned.
pub(crate) fn eval(src: &str) -> Result<Owned, schemify_expr::Error> {
    eval_with(src, &[])
}

/// Compiles and evaluates with `vars` as the scope.
pub(crate) fn eval_with(
    src: &str,
    vars: &[(&str, Value<'_>)],
) -> Result<Owned, schemify_expr::Error> {
    let p = Program::compile(src, Mode::Scalar)?;
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| {
        vars.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)
    })?;
    let mut s = Scratch::default();
    p.eval(&inputs, &mut s).map(Owned::from)
}

/// The error code of `src`, whether at compile or at evaluation.
pub(crate) fn code(src: &str) -> Code {
    match eval(src) {
        Ok(v) => panic!("`{src}` evaluated to {v:?}, expected an error"),
        Err(e) => e.code,
    }
}

/// An owned [`Value`] for assertions.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Owned {
    Int(i64),
    Num(f64),
    Bool(bool),
    Cnum(f64, f64),
    Str(String),
}

impl From<Value<'_>> for Owned {
    fn from(v: Value<'_>) -> Owned {
        match v {
            Value::Int(x) => Owned::Int(x),
            Value::Num(x) => Owned::Num(x),
            Value::Bool(b) => Owned::Bool(b),
            Value::Cnum { re, im } => Owned::Cnum(re, im),
            Value::Str(s) => Owned::Str(s.to_string()),
        }
    }
}
