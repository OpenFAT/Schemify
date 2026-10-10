//! Negative space: every spec §8 code, with spans where they matter.

use schemify_expr::{Code, Column, Cycle, Mode, Plot, Program, Scratch, Span, Template, Value};

use crate::code;

fn compile_err(src: &str, mode: Mode) -> schemify_expr::Error {
    match Program::compile(src, mode) {
        Ok(_) => panic!("`{src}` compiled, expected an error"),
        Err(e) => e,
    }
}

fn span(e: &schemify_expr::Error) -> (u32, u32) {
    (e.span.start, e.span.end)
}

#[test]
fn x001_number_literals() {
    for src in [
        "1x",
        "2.5megs",
        "1.",
        "1e",
        "1e+",
        "1E-x",
        "10nF",
        "1kk",
        "1_0",
        "3meg2",
        "1µF",
        "4mi",
        "1e400",
        "1e99999999999999999999",
    ] {
        assert_eq!(compile_err(src, Mode::Scalar).code, Code::X001, "`{src}`");
    }
    let e = compile_err("a + 10nF", Mode::Scalar);
    assert_eq!(span(&e), (4, 8));
    assert!(e.msg.contains("10nF"), "{e}");
    // num() of bad text is a runtime X001 spanned at the call.
    let e = crate::eval(r#"1 + num(s)"#).err();
    assert!(e.is_some_and(|e| e.code == Code::X004));
    let e = crate::eval_with("1 + num(s)", &[("s", Value::Str("12abc"))]).unwrap_err();
    assert_eq!((e.code, span(&e)), (Code::X001, (4, 10)));
    for s in ["", "-", "abc", " 1", "1 ", "1e", "--1", "1k2", "0x10"] {
        assert_eq!(
            schemify_expr::parse_num(s).unwrap_err().code,
            Code::X001,
            "{s:?}"
        );
    }
}

#[test]
fn x002_limits() {
    let deep_ok = format!("{}1{}", "(".repeat(255), ")".repeat(255));
    assert!(Program::compile(&deep_ok, Mode::Scalar).is_ok());
    let deep = format!("{}1{}", "(".repeat(256), ")".repeat(256));
    assert_eq!(compile_err(&deep, Mode::Scalar).code, Code::X002);
    assert_eq!(
        compile_err(&format!("{}1", "-".repeat(300)), Mode::Scalar).code,
        Code::X002
    );
    assert_eq!(
        compile_err(&format!("2{}", "^2".repeat(300)), Mode::Scalar).code,
        Code::X002,
        "`^` recurses"
    );
    // k terms of `x+…` make 2k-1 nodes; a leading `-` adds one.
    let at_limit = format!("-x{}", "+x".repeat(32_767));
    assert!(
        Program::compile(&at_limit, Mode::Scalar).is_ok(),
        "65 536 nodes is allowed"
    );
    let over = format!("-x{}", "+x".repeat(32_768));
    assert_eq!(compile_err(&over, Mode::Scalar).code, Code::X002);
    // Strings over 1 MiB, literal or computed.
    let big = format!("\"{}\"", "a".repeat((1 << 20) + 1));
    assert_eq!(compile_err(&big, Mode::Scalar).code, Code::X002);
    let half = "b".repeat((1 << 19) + 1);
    let e = crate::eval_with("s + s", &[("s", Value::Str(&half))]).unwrap_err();
    assert_eq!(e.code, Code::X002);
}

#[test]
fn x003_types() {
    for src in [
        r#"1 + "a""#,
        "!1",
        "true + 1",
        "1 ? 2 : 3",
        r#""a" < 1"#,
        "cplx(1, 2) < 1",
        "cplx(1, 2) == 1",
        "cplx(1, 2) % 2",
        "str(cplx(1, 0))",
        "len(3)",
        "if(1, 2, 3)",
        "1 && true",
        "true && 1",
        "false || 2",
        "-true",
        r#""a" * 2"#,
        r#"floor("x")"#,
        "eng(1, 0)",
        "eng(1, 2.5)",
        "min(1, true)",
    ] {
        assert_eq!(code(src), Code::X003, "`{src}`");
    }
    // Runtime type errors carry the operator's span.
    let e = crate::eval_with("1 + x * 2", &[("x", Value::Bool(true))]).unwrap_err();
    assert_eq!((e.code, span(&e)), (Code::X003, (4, 9)));
    // `v()` takes a name, not an expression.
    assert_eq!(compile_err("v(1 + 2)", Mode::Calc).code, Code::X003);
}

#[test]
fn x004_unknown_identifier() {
    let p = Program::compile("1 + width * 2", Mode::Scalar).unwrap();
    let e = p.bind(&mut Vec::new(), |_| None).unwrap_err();
    assert_eq!((e.code, span(&e)), (Code::X004, (4, 9)));
    assert!(e.msg.contains("width"));
    assert_eq!(compile_err("phys.nope", Mode::Scalar).code, Code::X004);
}

#[test]
fn x005_cycle() {
    let cycle = Cycle { nodes: vec![2, 0] };
    let names = ["a", "b", "c"];
    let e = cycle.error(|i| names[i as usize]);
    assert_eq!(e.code, Code::X005);
    assert_eq!(e.msg, "dependency cycle: c -> a -> c");
}

#[test]
fn x006_unknown_function() {
    assert_eq!(compile_err("foo(1)", Mode::Scalar).code, Code::X006);
    assert_eq!(
        compile_err("Sin(1)", Mode::Scalar).code,
        Code::X006,
        "names are case-sensitive"
    );
    let e = compile_err("1 + deriv(x)", Mode::Scalar);
    assert_eq!((e.code, span(&e)), (Code::X006, (4, 12)));
    assert_eq!(compile_err("v(out)", Mode::Scalar).code, Code::X006);
    assert_eq!(compile_err("phys.k(1)", Mode::Scalar).code, Code::X006);
    assert_eq!(code("gmid.lookup(1)"), Code::X006);
    assert!(Program::compile("deriv(v(out))", Mode::Calc).is_ok());
}

#[test]
fn x007_is_a_warning() {
    assert!(Code::X007.is_warning());
    assert!(!Code::X004.is_warning());
}

#[test]
fn x008_arity() {
    for src in [
        "sin(1, 2)",
        "min(1)",
        "atan2(1)",
        "if(true, 1)",
        "clamp(1, 2)",
        "eng()",
        "pow(1, 2, 3)",
        "x(1)",
    ] {
        let mode = if src.starts_with("x(") {
            Mode::Calc
        } else {
            Mode::Scalar
        };
        assert_eq!(compile_err(src, mode).code, Code::X008, "`{src}`");
    }
    assert_eq!(compile_err("cross(v(a))", Mode::Calc).code, Code::X008);
    let many = format!("p.f({})", vec!["1"; 17].join(","));
    assert_eq!(compile_err(&many, Mode::Scalar).code, Code::X008);
    assert!(Program::compile(&format!("p.f({})", vec!["1"; 16].join(",")), Mode::Scalar).is_ok());
}

#[test]
fn x009_parse() {
    for src in [
        "",
        "   ",
        "1 +",
        "(1",
        "1)",
        "1 2",
        "a < b < c",
        "a = b",
        "a & b",
        "a | b",
        r#""abc"#,
        r#""\q""#,
        r#""\u{d800}""#,
        r#""\u{}""#,
        "+1",
        "1 ? 2",
        "f(1,)",
        "(,)",
        "a.b.c",
        "$x",
        "1 ? 2 : ",
        "true(1)",
        "3(4)",
    ] {
        assert_eq!(compile_err(src, Mode::Scalar).code, Code::X009, "`{src}`");
    }
    let e = compile_err("1 + * 2", Mode::Scalar);
    assert_eq!((e.code, span(&e)), (Code::X009, (4, 5)));
    let e = compile_err("a == b == c", Mode::Scalar);
    assert!(e.msg.contains("chain"), "{e}");
    for t in ["{", "{}", "{ }", "a}b", "{1 +}", "x {a"] {
        assert_eq!(
            Template::parse(t).unwrap_err().code,
            Code::X009,
            "template `{t}`"
        );
    }
}

#[test]
fn x010_non_finite() {
    for src in [
        "1 / 0",
        "0 / 0",
        "ln(0)",
        "sqrt(-1)",
        "exp(1000)",
        "1e300 * 1e300",
        "int(1e300)",
        "asin(2)",
        "(-8) ^ (1/3)",
        "db(0)",
        "snap(1, 0)",
        "0 ^ -1",
        "1 % 0",
    ] {
        assert_eq!(code(src), Code::X010, "`{src}`");
    }
    let e = crate::eval_with("2 * (1 / x)", &[("x", Value::Int(0))]).unwrap_err();
    assert_eq!((e.code, span(&e)), (Code::X010, (5, 10)));
    assert_eq!(
        crate::eval_with("x", &[("x", Value::Num(f64::NAN))])
            .unwrap_err()
            .code,
        Code::X010
    );
    assert_eq!(
        crate::eval_with(
            "x",
            &[(
                "x",
                Value::Cnum {
                    re: 0.0,
                    im: f64::INFINITY
                }
            )]
        )
        .unwrap_err()
        .code,
        Code::X010
    );
}

#[test]
fn x011_axes() {
    let p = Program::compile("v(a) + v(b)", Mode::Calc).unwrap();
    let x = [0.0, 1.0, 2.0];
    let (a, b) = ([1.0, 2.0, 3.0], [1.0, 2.0]);
    let cols = [Column { re: &a, im: &[] }, Column { re: &b, im: &[] }];
    let plot = Plot {
        x: &x,
        steps: &[],
        signals: &cols,
    };
    let mut s = Scratch::default();
    assert_eq!(
        p.eval_calc_plain(&[], &plot, &mut s).unwrap_err().code,
        Code::X011
    );
    let cols = [Column { re: &a, im: &[] }, Column { re: &a, im: &[] }];
    let (short, late) = (
        std::iter::once(0..2).collect::<Vec<_>>(),
        std::iter::once(1..3).collect::<Vec<_>>(),
    );
    for steps in [&[0..1, 2..3][..], &short, &late] {
        let plot = Plot {
            x: &x,
            steps,
            signals: &cols,
        };
        assert_eq!(
            p.eval_calc_plain(&[], &plot, &mut s).unwrap_err().code,
            Code::X011
        );
    }
    let p = Program::compile("fft(v(a)) + v(a)", Mode::Calc).unwrap();
    let x: Vec<f64> = (0..8).map(f64::from).collect();
    let cols = [Column { re: &x, im: &[] }];
    let plot = Plot {
        x: &x,
        steps: &[],
        signals: &cols,
    };
    assert_eq!(
        p.eval_calc_plain(&[], &plot, &mut s).unwrap_err().code,
        Code::X011
    );
    let p = Program::compile("fft(v(a))", Mode::Calc).unwrap();
    let one = [1.0];
    let cols = [Column { re: &one, im: &[] }];
    let plot = Plot {
        x: &one,
        steps: &[],
        signals: &cols,
    };
    assert_eq!(
        p.eval_calc_plain(&[], &plot, &mut s).unwrap_err().code,
        Code::X011
    );
}

#[test]
fn error_display_names_code_and_span() {
    let e = compile_err("1 +", Mode::Scalar);
    assert_eq!(e.to_string(), "X009 at 3..3: unexpected end of expression");
    assert_eq!(e.span, Span { start: 3, end: 3 });
}
