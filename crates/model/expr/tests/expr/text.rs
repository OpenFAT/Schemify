//! Spec §2 suffix table (exhaustive) and §7 canonical number text
//! (round-trip property over random f64, plus the spec's examples).

use proptest::prelude::*;
use schemify_expr::{Code, Mode, Program, Scratch, Value, parse_num, write_num};

use crate::Owned::{Num, Str};

const TABLE: [(&str, i32); 11] = [
    ("f", -15),
    ("p", -12),
    ("n", -9),
    ("u", -6),
    ("\u{b5}", -6),
    ("\u{3bc}", -6),
    ("m", -3),
    ("k", 3),
    ("meg", 6),
    ("g", 9),
    ("t", 12),
];

// ---- private ----

fn canon(x: f64) -> String {
    let mut s = String::new();
    write_num(&mut s, x);
    s
}

#[test]
fn canonical_examples() {
    let cases: [(f64, &str); 16] = [
        (2.0, "2"),
        (-40.0, "-40"),
        (-0.0, "0"),
        (0.0, "0"),
        (0.15, "0.15"),
        (2.5e-3, "0.0025"),
        (1e-4, "0.0001"),
        (9.99e-5, "9.99e-5"),
        (1e-12, "1e-12"),
        (2.5e-15, "2.5e-15"),
        (1.2e15, "1.2e15"),
        (1e15, "1e15"),
        (999_999_999_999_999.0, "999999999999999"),
        (123_456.789, "123456.789"),
        (-1.5e300, "-1.5e300"),
        (5e-324, "5e-324"),
    ];
    for (x, want) in cases {
        assert_eq!(canon(x), want, "{x:e}");
    }
    let mut s = String::new();
    schemify_expr::write_int(&mut s, i64::MIN);
    assert_eq!(s, "-9223372036854775808");
    assert_eq!(
        crate::eval("str(1000000000000000000)").unwrap(),
        Str("1000000000000000000".into()),
        "ints are always plain"
    );
    assert_eq!(crate::eval("str(1e18)").unwrap(), Str("1e18".into()));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20_000, ..ProptestConfig::default() })]

    /// `num(str(x)) == x` for every finite f64, through the language itself.
    #[test]
    fn num_str_round_trips(bits in any::<u64>()) {
        let x = f64::from_bits(bits);
        prop_assume!(x.is_finite());
        let text = canon(x);
        prop_assert_eq!(parse_num(&text).unwrap(), x, "{}", text);
        prop_assert!(!text.contains('+'), "{text}");
        prop_assert!(!text.contains(".e"), "{text}");
        prop_assert!(!text.ends_with('.'), "{text}");
        let p = Program::compile("num(str(x)) == x", Mode::Scalar).unwrap();
        let mut s = Scratch::default();
        prop_assert_eq!(p.eval(&[Value::Num(x)], &mut s).unwrap(), Value::Bool(true));
    }

    #[test]
    fn eng_reads_back(x in -1e20f64..1e20, digits in 1u32..=17) {
        let r = crate::eval(&format!("num(eng({}, {digits}))", canon(x))).unwrap();
        let Num(y) = r else { panic!("{r:?}") };
        let tol = x.abs() * 10f64.powi(1 - digits as i32) + 1e-300;
        prop_assert!((y - x).abs() <= tol, "eng({x}, {digits}) read back as {y}");
    }

    /// Every finite f64: `eng()` is at most `digits + 7` bytes and `num()` reads it
    /// back within one unit of its last digit (half the smallest subnormal more).
    #[test]
    fn eng_is_narrow_and_reads_back_over_all_f64(bits in any::<u64>(), digits in 1usize..=17) {
        let x = f64::from_bits(bits);
        prop_assume!(x.is_finite());
        let text = eng(x, digits);
        prop_assert!(text.len() <= digits + 7, "eng({x:e}, {digits}) = {text}");
        let y = parse_num(&text).unwrap();
        let tol = x.abs() * 10f64.powi(1 - digits as i32) + f64::from_bits(1);
        prop_assert!((y - x).abs() <= tol, "eng({x:e}, {digits}) = {text} read back as {y:e}");
    }
}

fn eng(x: f64, digits: usize) -> String {
    let r = crate::eval(&format!("eng({}, {digits})", canon(x))).expect("eng is total");
    let Str(text) = r else { panic!("{r:?}") };
    text
}

/// Regression (with `wave::format_si`): past the suffix table the mantissa grows
/// three digits at most (`0.001f`, `999999t`), then the text is scientific; the
/// edges are decided after rounding, and `f64::MAX` truncates rather than overflow.
#[test]
fn eng_beyond_the_suffixes_is_scientific() {
    let cases: [(f64, usize, &str); 14] = [
        (1e-20, 3, "1e-20"),
        (-1.234e-19, 4, "-1.234e-19"),
        (1e-18, 4, "0.001f"),
        (9.99e-19, 3, "9.99e-19"),
        (9.9996e-19, 4, "0.001f"),
        (1.5e15, 3, "1500t"),
        (999.94e15, 4, "999900t"),
        (999.95e15, 4, "1e18"),
        (1e18, 4, "1e18"),
        (1e300, 3, "1e300"),
        (f64::from_bits(1), 4, "4.941e-324"),
        (f64::MAX, 4, "1.797e308"),
        (f64::MAX, 1, "1e308"),
        (-f64::MAX, 17, "-1.7976931348623157e308"),
    ];
    for (x, digits, want) in cases {
        assert_eq!(eng(x, digits), want, "eng({x:e}, {digits})");
    }
}

/// Every casing of an ASCII suffix.
fn casings(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for ch in s.chars() {
        let variants: Vec<char> = if ch.is_ascii_alphabetic() {
            vec![ch.to_ascii_lowercase(), ch.to_ascii_uppercase()]
        } else {
            vec![ch]
        };
        out = out
            .iter()
            .flat_map(|p| variants.iter().map(move |c| format!("{p}{c}")))
            .collect();
    }
    out
}

fn literal(src: &str) -> Result<f64, Code> {
    let p = Program::compile(src, Mode::Scalar).map_err(|e| e.code)?;
    match p.eval(&[], &mut Scratch::default()).map_err(|e| e.code)? {
        Value::Num(x) => Ok(x),
        Value::Int(x) => Ok(x as f64),
        v => panic!("{src}: {v:?}"),
    }
}

#[test]
fn si_suffix_table_exhaustive() {
    let mantissas = [
        ("1", 0),
        ("2.5", 0),
        (".5", 0),
        ("3e2", 0),
        ("7E-1", 0),
        ("12.75e+3", 0),
    ];
    for (suffix, pow) in TABLE {
        for spelled in casings(suffix) {
            for (mant, _) in mantissas {
                let (m, e) = match mant.find(['e', 'E']) {
                    Some(i) => (&mant[..i], mant[i + 1..].parse::<i32>().unwrap()),
                    None => (mant, 0),
                };
                let want: f64 = format!("{m}e{}", e + pow).parse().unwrap();
                let src = format!("{mant}{spelled}");
                assert_eq!(literal(&src), Ok(want), "`{src}`");
                assert_eq!(parse_num(&src).unwrap(), want, "num(`{src}`)");
            }
            // Strict: anything identifier-like after a suffix is X001.
            for tail in ["a", "x", "F", "V", "0", "9", "_", "\u{b5}", "meg", "s"] {
                let src = format!("1{spelled}{tail}");
                let allowed = TABLE
                    .iter()
                    .any(|(s, _)| s.eq_ignore_ascii_case(&format!("{spelled}{tail}")));
                if !allowed {
                    assert_eq!(literal(&src), Err(Code::X001), "`{src}`");
                }
            }
        }
    }
    for spelled in casings("mil") {
        assert_eq!(literal(&format!("10{spelled}")), Ok(10.0 * 25.4e-6), "mil");
        assert_eq!(literal(&format!("1{spelled}s")), Err(Code::X001));
    }
    // Every single ASCII letter after a number: a suffix or X001.
    for c in ('a'..='z').chain('A'..='Z') {
        let src = format!("1{c}");
        let ok = TABLE
            .iter()
            .any(|(s, _)| s.eq_ignore_ascii_case(&c.to_string()));
        assert_eq!(literal(&src).is_ok(), ok, "`{src}`");
    }
    // `M` is milli (SPICE), mega is `meg`.
    assert_eq!(literal("1M"), Ok(1e-3));
    assert_eq!(literal("1MEG"), Ok(1e6));
    // Suffix scaling is correctly rounded, not a multiply.
    assert_eq!(literal("2.5p"), Ok(2.5e-12));
    assert_eq!(literal("0.3u"), Ok(0.3e-6));
    assert_eq!(literal("1e-3k"), Ok(1.0));
}
