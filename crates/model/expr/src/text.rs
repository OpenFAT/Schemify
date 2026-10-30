//! Number text: canonical form (spec §7), `eng()` and `num()`.
//!
//! Built only on core's float formatting (shortest round-trip digits, exact
//! with a precision), which is platform independent, so the bytes are the
//! same on every target.
//!
//! Contents:
//! - `pub fn write_num(out: &mut String, x: f64)`
//! - `pub fn write_int(out: &mut String, x: i64)`
//! - `pub fn parse_num(s: &str) -> Result<f64, Error>`
//! - `pub(crate) fn write_eng(out: &mut String, x: f64, digits: u32)`

use core::iter;
use core::ops::Range;

use crate::lex::{self, TokenKind};
use crate::{Code, Error, Span};

/// Appends the canonical text of `x` (spec §7): an integer when `x` is
/// integral with |x| < 1e15 (`-0` is `0`), the shortest round-trip decimal
/// when 1e-4 ≤ |x| < 1e15, otherwise the shortest round-trip mantissa with an
/// exponent (`2.5e-15`, `1.2e15`). [`parse_num`] reads it back exactly.
///
/// # Panics
/// If `x` is NaN or infinite; those are never values (`X010`).
pub fn write_num(out: &mut String, x: f64) {
    use core::fmt::Write;

    assert!(x.is_finite(), "canonical text of a non-finite number {x}");
    let start = out.len();
    let magnitude = x.abs();
    let written = if x.fract() == 0.0 && magnitude < 1e15 {
        write!(out, "{}", x as i64)
    } else if (1e-4..1e15).contains(&magnitude) {
        write!(out, "{x}")
    } else {
        write!(out, "{x:e}")
    };
    written.expect("writing to a String cannot fail");
    debug_assert!(out.len() > start, "a number has text");
    // `-0` is `0`: a minus sign always precedes a non-zero digit.
    debug_assert!(x != 0.0 || &out[start..] == "0", "zero writes as `0`");
}

/// Appends an `int` as plain decimal (spec §7).
pub fn write_int(out: &mut String, x: i64) {
    use core::fmt::Write;

    let start = out.len();
    write!(out, "{x}").expect("writing to a String cannot fail");
    debug_assert!(out.len() > start, "an int has text");
}

/// Parses a number with an optional sign and SI suffix (the `num()` builtin):
/// `-2.5meg`, `1e-12`, `10n`. Exactly the literal grammar of spec §2 plus a
/// leading `+`/`-`; no surrounding whitespace, no trailing letters.
///
/// # Errors
/// `X001` (with an empty span) when `s` is not such a number or overflows.
pub fn parse_num(s: &str) -> Result<f64, Error> {
    let bad = |why: &str| {
        Error::new(
            Code::X001,
            Span::default(),
            format!("not a number: {s:?} ({why})"),
        )
    };
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    if body.is_empty() {
        return Err(bad("empty"));
    }
    let literal = lex::number(body, 0).map_err(|error| bad(error.reason))?;
    if literal.end != body.len() {
        return Err(bad("trailing text"));
    }
    let v = match literal.kind {
        TokenKind::Int(i) => i as f64,
        TokenKind::Num(v) => v,
        _ => unreachable!("number() returns numeric literals"),
    };
    debug_assert!(v.is_finite(), "number() rejects out-of-range literals");
    debug_assert!(v >= 0.0, "the sign was stripped above");
    Ok(if s.starts_with('-') { -v } else { v })
}

/// Decimal exponents (of the value rounded to its digits) `eng()` writes with a
/// suffix: the mantissa grows at most three digits past `f` and `t` (`0.001f`,
/// `999999t`); outside it is scientific notation, so the text stays narrow.
const SUFFIX_EXPONENTS: Range<i32> = -18..18;

/// Engineering notation with a spec §2 suffix, rounded to `digits`
/// significant digits: `4.7k`, `1.5meg`, `220n`. Beyond the suffix range the
/// mantissa grows up to three digits (`0.001f`, `999999t`), then the text is
/// scientific (`1e-20`, `4.941e-324`; toward zero where rounding up would pass
/// `f64::MAX`), so `num()` always reads it back and it is at most `digits + 7`
/// bytes.
pub(crate) fn write_eng(out: &mut String, x: f64, digits: u32) {
    assert!(x.is_finite(), "eng of a non-finite number");
    assert!(
        (1..=17).contains(&digits),
        "eng digits {digits} outside 1..=17"
    );
    if x == 0.0 {
        out.push('0');
        return;
    }
    let sci = format!("{:.*e}", digits as usize - 1, x.abs());
    let (mant, exp) = sci
        .split_once('e')
        .expect("LowerExp always has an exponent");
    let exp: i32 = exp.parse().expect("LowerExp exponent is an integer");
    if !SUFFIX_EXPONENTS.contains(&exp) {
        write_scientific(out, x, digits as usize);
        return;
    }
    let sig: String = mant.chars().filter(char::is_ascii_digit).collect();
    debug_assert_eq!(
        sig.len(),
        digits as usize,
        "LowerExp keeps `digits` significant digits"
    );
    let e3 = (exp.div_euclid(3) * 3).clamp(-15, 12);
    let int_len = 1 + exp - e3;
    let mut num = String::new();
    if int_len <= 0 {
        num.push_str("0.");
        num.extend(iter::repeat_n('0', (-int_len) as usize));
        num.push_str(&sig);
    } else if int_len as usize >= sig.len() {
        num.push_str(&sig);
        num.extend(iter::repeat_n('0', int_len as usize - sig.len()));
    } else {
        num.push_str(&sig[..int_len as usize]);
        num.push('.');
        num.push_str(&sig[int_len as usize..]);
    }
    if num.contains('.') {
        num.truncate(num.trim_end_matches('0').trim_end_matches('.').len());
    }
    let suffix = match e3 {
        -15 => "f",
        -12 => "p",
        -9 => "n",
        -6 => "u",
        -3 => "m",
        0 => "",
        3 => "k",
        6 => "meg",
        9 => "g",
        _ => "t",
    };
    if x < 0.0 {
        out.push('-');
    }
    out.push_str(&num);
    out.push_str(suffix);
    debug_assert!(!num.is_empty(), "a mantissa has digits");
}

// ---- private ----

/// Appends `x` in scientific notation with `digits` significant digits and
/// trailing mantissa zeros trimmed (`4.941e-324`, `-1e20`); truncated where
/// rounding would pass `f64::MAX`. `wave::format_si` writes the same text.
fn write_scientific(out: &mut String, x: f64, digits: usize) {
    assert!((1..=17).contains(&digits), "digits {digits} outside 1..=17");
    let start = out.len();
    let mut text = format!("{x:.*e}", digits - 1);
    if text.parse::<f64>().is_ok_and(f64::is_infinite) {
        let full = format!("{x:.16e}");
        let (mant, exp) = full.split_once('e').expect("LowerExp has an exponent");
        let keep = digits + 1 + usize::from(x < 0.0);
        text = format!("{}e{exp}", &mant[..keep]);
    }
    let (mant, exp) = text.split_once('e').expect("LowerExp has an exponent");
    let mant = if mant.contains('.') {
        mant.trim_end_matches('0').trim_end_matches('.')
    } else {
        mant
    };
    out.push_str(mant);
    out.push('e');
    out.push_str(exp);
    debug_assert!(
        out[start..].parse::<f64>().is_ok_and(f64::is_finite),
        "{} reads back finite",
        &out[start..]
    );
}
