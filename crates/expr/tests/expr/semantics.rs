//! Spec §3–§5 by example: precedence, promotion, laziness, builtins, scopes,
//! plugins. The differential test covers the space; these pin the corners.

use schemify_expr::{Code, Error, Mode, Program, Scratch, Span, Value};

use crate::Owned::{self, Bool, Cnum, Int, Num, Str};
use crate::{eval, eval_with};

fn ok(src: &str) -> Owned {
    eval(src).unwrap_or_else(|e| panic!("`{src}`: {e}"))
}

#[test]
fn precedence_and_associativity() {
    assert_eq!(ok("2 + 3 * 4 ^ 2"), Num(50.0));
    assert_eq!(ok("-2 ^ 2"), Num(-4.0));
    assert_eq!(ok("2 ^ 3 ^ 2"), Num(512.0));
    assert_eq!(ok("2 ^ -1"), Num(0.5));
    assert_eq!(ok("10 - 4 - 3"), Int(3));
    assert_eq!(ok("2 * 3 % 4"), Int(2));
    assert_eq!(ok("1 + 2 == 3 && 2 < 3 || false"), Bool(true));
    assert_eq!(ok("!true == false"), Bool(true));
    assert_eq!(ok("true ? 1 : false ? 2 : 3"), Int(1));
    assert_eq!(ok("false ? 1 : false ? 2 : 3"), Int(3));
    assert_eq!(ok("(1 < 2) == true"), Bool(true));
    assert_eq!(ok("--3"), Int(3));
}

/// Libm-backed results: within a few ulps, not bitwise.
fn approx(src: &str, want: f64) {
    match ok(src) {
        Num(x) => assert!(
            (x - want).abs() <= 4.0 * f64::EPSILON * want.abs().max(1.0),
            "`{src}` = {x}, want {want}"
        ),
        o => panic!("`{src}` = {o:?}"),
    }
}

#[test]
fn int_promotion() {
    assert_eq!(ok("7 / 2"), Num(3.5));
    assert_eq!(ok("4 / 2"), Num(2.0));
    assert_eq!(ok("7 % 3"), Int(1));
    assert_eq!(ok("-7 % 3"), Int(-1));
    assert_eq!(ok("7 % -3"), Int(1));
    assert_eq!(ok("7.5 % 2"), Num(1.5));
    assert_eq!(
        ok("9223372036854775807 + 1"),
        Num(9_223_372_036_854_775_808.0)
    );
    assert_eq!(
        ok("3037000500 * 3037000500"),
        Num(3_037_000_500f64 * 3_037_000_500f64)
    );
    assert_eq!(ok("-9223372036854775807 - 1"), Int(i64::MIN));
    assert_eq!(ok("99999999999999999999"), Num(1e20));
    assert_eq!(ok("2 + 0.5"), Num(2.5));
    assert_eq!(ok("1 == 1.0"), Bool(true));
    assert_eq!(ok("2 < 2.5"), Bool(true));
}

#[test]
fn laziness() {
    assert_eq!(ok("false && 1 / 0 > 0"), Bool(false));
    assert_eq!(ok("true || 1 / 0 > 0"), Bool(true));
    assert_eq!(ok("true ? 1 : 1 / 0"), Int(1));
    assert_eq!(ok("if(false, ln(0), 2)"), Int(2));
    // Runtime-lazy with an input condition: no error unless the arm runs.
    let p = Program::compile("c ? 1 : 1 / 0", Mode::Scalar).unwrap();
    let mut s = Scratch::default();
    assert_eq!(p.eval(&[Value::Bool(true)], &mut s).unwrap(), Value::Int(1));
    assert_eq!(
        p.eval(&[Value::Bool(false)], &mut s).unwrap_err().code,
        Code::X010
    );
}

#[test]
fn strings() {
    assert_eq!(ok(r#""ab" + "cd""#), Str("abcd".into()));
    assert_eq!(ok(r#""a\"b\\c\n\t\u{e9}""#), Str("a\"b\\c\n\té".into()));
    assert_eq!(ok(r#""abc" < "abd""#), Bool(true));
    assert_eq!(ok(r#""B" < "a""#), Bool(true));
    assert_eq!(ok(r#"len("héllo")"#), Int(5));
    assert_eq!(ok(r#"str(1.5k)"#), Str("1500".into()));
    assert_eq!(ok(r#"str(0.1 + 0.2)"#), Str("0.30000000000000004".into()));
    assert_eq!(ok(r#"str(true) + str(7)"#), Str("true7".into()));
    assert_eq!(ok(r#"num("2.5meg") / 1k"#), Num(2500.0));
    assert_eq!(ok(r#"num("-1e-3")"#), Num(-1e-3));
    assert_eq!(ok(r#"eng(4700)"#), Str("4.7k".into()));
    assert_eq!(ok(r#"eng(-0.000123456, 4)"#), Str("-123.5u".into()));
    assert_eq!(ok(r#"eng(2.2e6)"#), Str("2.2meg".into()));
}

#[test]
fn builtins() {
    assert_eq!(ok("abs(-3)"), Int(3));
    assert_eq!(ok("abs(-2.5)"), Num(2.5));
    assert_eq!(ok("abs(cplx(3, 4))"), Num(5.0));
    assert_eq!(ok("sqrt(16)"), Num(4.0));
    assert_eq!(ok("round(2.5)"), Num(3.0));
    assert_eq!(ok("round(-2.5)"), Num(-3.0));
    assert_eq!(ok("floor(-2.5)"), Num(-3.0));
    assert_eq!(ok("ceil(2.1)"), Num(3.0));
    assert_eq!(ok("int(-2.7)"), Int(-2));
    assert_eq!(ok("int(2.7)"), Int(2));
    assert_eq!(ok("sign(-0.0)"), Num(0.0));
    assert_eq!(ok("sign(-5)"), Int(-1));
    assert_eq!(ok("min(3, 1, 2)"), Int(1));
    assert_eq!(ok("max(1, 2.5)"), Num(2.5));
    assert_eq!(ok("clamp(5, 0, 3)"), Int(3));
    assert_eq!(ok("clamp(-1.5, 0, 3)"), Num(0.0));
    assert_eq!(ok("snap(7, 5)"), Num(5.0));
    assert_eq!(ok("snap(7.5, 5)"), Num(10.0));
    assert_eq!(ok("snap(-7.5, 5)"), Num(-10.0));
    assert_eq!(ok("pow(2, 10)"), Num(1024.0));
    approx("atan2(1, 1) * 4", std::f64::consts::PI);
    approx("log10(1k)", 3.0);
    approx("ln(exp(2))", 2.0);
    approx("phys.kelvin0 + 27", 300.15);
    assert_eq!(ok("phys.q"), Num(1.602_176_634e-19));
}

#[test]
fn complex() {
    assert_eq!(ok("cplx(1, 2) + 1"), Cnum(2.0, 2.0));
    assert_eq!(ok("cplx(1, 2) * cplx(3, -1)"), Cnum(5.0, 5.0));
    assert_eq!(ok("cplx(0, 1) ^ 2"), Cnum(-1.0, 1.2246467991473532e-16));
    assert_eq!(ok("conj(cplx(1, 2))"), Cnum(1.0, -2.0));
    assert_eq!(ok("re(cplx(1, 2)) + im(cplx(1, 2))"), Num(3.0));
    assert_eq!(ok("mag(cplx(3, 4))"), Num(5.0));
    assert_eq!(ok("ph(cplx(0, 1))"), Num(90.0));
    assert_eq!(ok("ph(-1)"), Num(180.0));
    assert_eq!(ok("db(cplx(0, 10))"), Num(20.0));
    assert_eq!(ok("cplx(1, 2) == cplx(1, 2)"), Bool(true));
    match ok("sqrt(cplx(-4, 0))") {
        Cnum(re, im) => assert!(re.abs() < 1e-15 && (im - 2.0).abs() < 1e-15),
        o => panic!("{o:?}"),
    }
}

#[test]
fn scopes_and_identifiers() {
    let p = Program::compile("w * l + w + nf.x", Mode::Scalar).unwrap();
    assert_eq!(p.idents().collect::<Vec<_>>(), ["w", "l", "nf.x"]);
    // Scope layers are chained by the caller: params shadow vars.
    let params = [("w", Value::Num(2.0))];
    let vars = [
        ("w", Value::Num(100.0)),
        ("l", Value::Int(3)),
        ("nf.x", Value::Int(1)),
    ];
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| {
        let find = |t: &[(&str, Value<'static>)]| t.iter().find(|(k, _)| *k == n).map(|(_, v)| *v);
        find(&params).or_else(|| find(&vars))
    })
    .unwrap();
    let mut s = Scratch::default();
    assert_eq!(p.eval(&inputs, &mut s).unwrap(), Value::Num(9.0));
    // The same program, re-evaluated with other inputs (per instance).
    assert_eq!(
        p.eval(&[Value::Int(1), Value::Int(1), Value::Int(0)], &mut s)
            .unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        eval_with("w + 1", &[("w", Value::Str("x"))])
            .unwrap_err()
            .code,
        Code::X003
    );
}

#[test]
fn constant_folding_keeps_semantics() {
    // Constant parts fold; the program still reads its inputs.
    let p = Program::compile("x + (1 + \"a\")", Mode::Scalar);
    assert_eq!(
        p.unwrap_err().code,
        Code::X003,
        "a constant num + str is a static type error"
    );
    let e = Program::compile("x * 2 + 1 / 0", Mode::Scalar).unwrap_err();
    assert_eq!((e.code, e.span), (Code::X010, Span { start: 8, end: 13 }));
    // Not when the constant sits under a lazy arm.
    assert!(Program::compile("x > 0 ? 1 : 1 / 0", Mode::Scalar).is_ok());
    let p = Program::compile("x > 0 ? x : -x", Mode::Scalar).unwrap();
    let mut s = Scratch::default();
    assert_eq!(
        p.eval(&[Value::Num(-3.0)], &mut s).unwrap(),
        Value::Num(3.0)
    );
}

#[test]
fn plugins() {
    let p = Program::compile("gmid.id(w, \"nfet\") * 2 + gmid.vt()", Mode::Scalar).unwrap();
    assert_eq!(
        p.plugin_fns().collect::<Vec<_>>(),
        [("gmid", "id"), ("gmid", "vt")]
    );
    let mut s = Scratch::default();
    let mut calls = Vec::new();
    let mut plugin = |slot: u32, args: &[Value<'_>]| -> Result<Value<'static>, Error> {
        calls.push((slot, args.len()));
        match (slot, args) {
            (0, [Value::Num(w), Value::Str("nfet")]) => Ok(Value::Num(w * 10.0)),
            (1, []) => Ok(Value::Num(0.4)),
            _ => Err(Error::new(Code::X006, Span::default(), "no such plugin")),
        }
    };
    let v = p
        .eval_with(&[Value::Num(1.5)], &mut plugin, &mut s)
        .unwrap();
    assert_eq!(v, Value::Num(30.4));
    assert_eq!(calls, [(0, 2), (1, 0)]);
    // Without plugins: X006 naming the function, spanned at the call.
    let e = p.eval(&[Value::Num(1.5)], &mut s).unwrap_err();
    assert_eq!(e.code, Code::X006);
    assert!(e.msg.contains("gmid.id"), "{e}");
    assert_eq!(e.span, Span { start: 0, end: 18 });
    // A plugin returning a non-finite value is X010.
    let mut bad = |_: u32, _: &[Value<'_>]| Ok(Value::Num(f64::INFINITY));
    assert_eq!(
        p.eval_with(&[Value::Num(1.0)], &mut bad, &mut s)
            .unwrap_err()
            .code,
        Code::X010
    );
}

#[test]
fn scratch_reuse_has_no_cross_talk() {
    let p = Program::compile("s + \"!\"", Mode::Scalar).unwrap();
    let mut s = Scratch::default();
    let a = Owned::from(p.eval(&[Value::Str("one")], &mut s).unwrap());
    let b = Owned::from(p.eval(&[Value::Str("two")], &mut s).unwrap());
    assert_eq!((a, b), (Str("one!".into()), Str("two!".into())));
}
