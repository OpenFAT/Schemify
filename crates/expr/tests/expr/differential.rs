//! Differential oracle: the crate (lexer + parser + compiler with constant
//! folding + VM) against the naive tree-walking reference, over generated
//! expressions printed with minimal parentheses (so precedence is tested
//! too). Plus a metamorphic check: calculator mode over a column equals
//! scalar mode per element.

use proptest::prelude::*;
use schemify_expr::{CalcResult, Code, ColKind, Column, Mode, Plot, Program, Scratch, Value};

use crate::reference::{self, E, RV, VARS};

#[derive(Debug)]
enum Outcome {
    CompileErr(Code),
    RunErr(Code),
    Ok(RV),
}

fn to_rv(v: Value<'_>) -> RV {
    match v {
        Value::Int(x) => RV::Int(x),
        Value::Num(x) => RV::Num(x),
        Value::Bool(b) => RV::Bool(b),
        Value::Cnum { re, im } => RV::C(re, im),
        Value::Str(s) => RV::Str(s.to_string()),
    }
}

fn crate_eval(src: &str, env: &[RV]) -> Outcome {
    let p = match Program::compile(src, Mode::Scalar) {
        Ok(p) => p,
        Err(e) => return Outcome::CompileErr(e.code),
    };
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| {
        VARS.iter().position(|v| *v == n).map(|i| env[i].as_value())
    })
    .expect("generated names are bound");
    let mut s = Scratch::default();
    match p.eval(&inputs, &mut s) {
        Ok(v) => Outcome::Ok(to_rv(v)),
        Err(e) => Outcome::RunErr(e.code),
    }
}

fn same(a: &RV, b: &RV) -> bool {
    match (a, b) {
        (RV::Num(x), RV::Num(y)) => x.to_bits() == y.to_bits(),
        (RV::C(a, b), RV::C(c, d)) => a.to_bits() == c.to_bits() && b.to_bits() == d.to_bits(),
        _ => a == b,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 6000, ..ProptestConfig::default() })]

    /// Ok values agree bitwise; a runtime error has the reference's code;
    /// a compile-time error (a constant subexpression that always runs)
    /// means the reference fails too.
    #[test]
    fn crate_matches_reference(e in reference::mixed(VARS.len()), env in reference::env()) {
        let src = reference::print(&e, &|i| VARS[i].to_string());
        let want = reference::eval(&e, &env);
        match (crate_eval(&src, &env), &want) {
            (Outcome::Ok(got), Ok(w)) => prop_assert!(same(&got, w), "{src}: crate {got:?}, reference {w:?}"),
            (Outcome::RunErr(c), Err(w)) => prop_assert_eq!(c, *w, "{}", src),
            (Outcome::CompileErr(c), Err(_)) => {
                // Only static checks of constant subexpressions fail at compile time here.
                prop_assert!(matches!(c, Code::X001 | Code::X003 | Code::X010), "{src}: compile error {c:?}");
            }
            (got, want) => prop_assert!(false, "{src}: crate {got:?}, reference {want:?}"),
        }
    }

    /// Element-wise calculator evaluation equals scalar evaluation per point.
    #[test]
    fn calc_matches_scalar_per_element(e in reference::mixed(1), xs in prop::collection::vec(-100f64..100.0, 1..6)) {
        // Column elements are nums: int exactness beyond 2^53 is not kept.
        prop_assume!(!has_big_int(&e));
        // `atan2` is IEEE on signed zeros (atan2(-0, -1) = -pi), and column
        // elements lose the int-ness that keeps `-(0)` unsigned in scalar mode.
        prop_assume!(!reference::print(&e, &|_| String::new()).contains("atan2"));
        let scalar_src = reference::print(&e, &|_| "a".to_string());
        let calc_src = reference::print(&e, &|_| "v(a)".to_string());
        let scalar = Program::compile(&scalar_src, Mode::Scalar);
        let calc = Program::compile(&calc_src, Mode::Calc);
        prop_assert_eq!(scalar.is_ok(), calc.is_ok(), "{}", scalar_src);
        let (Ok(scalar), Ok(calc)) = (scalar, calc) else { return Ok(()) };

        let mut s = Scratch::default();
        let per_point: Vec<Result<RV, Code>> = xs.iter().map(|&x| {
            let inputs: Vec<Value> = scalar.idents().map(|_| Value::Num(x)).collect();
            scalar.eval(&inputs, &mut s).map(to_rv).map_err(|e| e.code)
        }).collect();

        let axis: Vec<f64> = (0..xs.len()).map(|i| i as f64).collect();
        let cols = [Column { re: &xs, im: &[] }];
        let plot = Plot { x: &axis, steps: &[], signals: &cols[..calc.signals().len()] };
        let mut cs = Scratch::default();
        match calc.eval_calc_plain(&[], &plot, &mut cs) {
            Ok(CalcResult::Scalar(v)) => {
                for p in &per_point {
                    prop_assert!(p.as_ref().is_ok_and(|p| *p == to_rv(v)), "{scalar_src}: calc {v:?}, scalar {p:?}");
                }
            }
            Ok(CalcResult::Column(col)) => {
                prop_assert_eq!(col.re.len(), xs.len());
                for (i, p) in per_point.iter().enumerate() {
                    let got = match col.kind {
                        ColKind::Real => RV::Num(col.re[i]),
                        ColKind::Bool => RV::Bool(col.re[i] != 0.0),
                        ColKind::Complex => RV::C(col.re[i], col.im[i]),
                    };
                    let want = match p {
                        // A column has one element kind: real elements of a
                        // complex column (mixed arms of a column condition)
                        // went through complex functions (spec §5).
                        Ok(RV::Int(_) | RV::Num(_)) | Err(_) if col.kind == ColKind::Complex => continue,
                        Ok(RV::Int(x)) => RV::Num(*x as f64),
                        Ok(v) => v.clone(),
                        Err(c) => return Err(TestCaseError::fail(format!("{scalar_src}: calc ok, scalar {c:?} at {i}"))),
                    };
                    // Column elements are nums, so an int-valued scalar path
                    // may differ in the sign of zero: compare by value.
                    prop_assert!(got == want, "{scalar_src} at {i}: calc {got:?}, scalar {want:?}");
                }
            }
            Err(err) => {
                // Both arms of a column condition run, and strings cannot form
                // columns, so only eager, string-free expressions must succeed.
                let eager = !has_lazy(&e) && !calc_src.contains("str(");
                let all_ok = per_point.iter().all(|p| matches!(p, Ok(v) if !matches!(v, RV::Str(_))));
                prop_assert!(!(eager && all_ok), "{scalar_src}: calc failed with {err} but every point succeeds");
            }
        }
    }
}

/// Guards the oracle against vacuity: enough generated cases must evaluate
/// to a value (not just to errors), and every error code must stay reachable.
#[test]
fn generator_reaches_values_and_errors() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (mut ok, mut codes) = (0, std::collections::BTreeSet::new());
    let n = 3000;
    for _ in 0..n {
        let e = reference::mixed(VARS.len())
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let env = reference::env().new_tree(&mut runner).unwrap().current();
        match reference::eval(&e, &env) {
            Ok(_) => ok += 1,
            Err(c) => {
                codes.insert(c);
            }
        }
    }
    assert!(
        ok * 5 > n * 2,
        "only {ok} of {n} generated cases produce a value"
    );
    for c in [Code::X001, Code::X003, Code::X010] {
        assert!(
            codes.contains(&c),
            "generator never reaches {c:?}: {codes:?}"
        );
    }
}

fn has_big_int(e: &E) -> bool {
    match e {
        E::Int(x) => x.unsigned_abs() > 1 << 53,
        E::Neg(x) | E::Not(x) => has_big_int(x),
        E::Bin(_, a, b) => has_big_int(a) || has_big_int(b),
        E::Tern(a, b, c) => has_big_int(a) || has_big_int(b) || has_big_int(c),
        E::Call(_, args) => args.iter().any(has_big_int),
        _ => false,
    }
}

fn has_lazy(e: &E) -> bool {
    match e {
        E::Tern(..) | E::Call("if", _) => true,
        E::Bin(reference::Op::And | reference::Op::Or, ..) => true,
        E::Neg(x) | E::Not(x) => has_lazy(x),
        E::Bin(_, a, b) => has_lazy(a) || has_lazy(b),
        E::Call(_, args) => args.iter().any(has_lazy),
        _ => false,
    }
}
