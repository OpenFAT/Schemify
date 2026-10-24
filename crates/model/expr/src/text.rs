//! Number text: canonical form (spec §7), `eng()` and `num()`.
//!
//! Built only on core's float formatting (shortest round-trip digits, exact
//! with a precision), which is platform independent, so the bytes are the
//! same on every target.

use core::fmt::Write;

use crate::lex::{self, Tok};
use crate::{Code, Error, Span};

/// Appends the canonical text of `x` (spec §7): an integer when `x` is
/// integral with |x| < 1e15 (`-0` is `0`), the shortest round-trip decimal
/// when 1e-4 ≤ |x| < 1e15, otherwise the shortest round-trip mantissa with an
/// exponent (`2.5e-15`, `1.2e15`). [`parse_num`] reads it back exactly.
///
/// # Panics
/// If `x` is NaN or infinite; those are never values (`X010`).
pub fn write_num(out: &mut String, x: f64) {
    assert!(x.is_finite(), "canonical text of a non-finite number");
    let a = x.abs();
    let r = if x.fract() == 0.0 && a < 1e15 {
        write!(out, "{}", x as i64)
    } else if (1e-4..1e15).contains(&a) {
        write!(out, "{x}")
    } else {
        write!(out, "{x:e}")
    };
    r.expect("writing to a String cannot fail");
}

/// Appends an `int` as plain decimal (spec §7).
pub fn write_int(out: &mut String, x: i64) {
    write!(out, "{x}").expect("writing to a String cannot fail");
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
    let (tok, end) = lex::number(body, 0).map_err(|(_, why)| bad(why))?;
    if end != body.len() {
        return Err(bad("trailing text"));
    }
    let v = match tok {
        Tok::Int(i) => i as f64,
        Tok::Num(v) => v,
        _ => unreachable!("number() returns numeric literals"),
    };
    debug_assert!(v.is_finite());
    Ok(if s.starts_with('-') { -v } else { v })
}

/// Engineering notation with a spec §2 suffix, rounded to `digits`
/// significant digits: `4.7k`, `1.5meg`, `220n`. Beyond the suffix range the
/// mantissa grows (`0.001f`, `1000t`), so `num()` always reads it back.
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
    let sig: String = mant.chars().filter(char::is_ascii_digit).collect();
    let e3 = (exp.div_euclid(3) * 3).clamp(-15, 12);
    let int_len = 1 + exp - e3;
    let mut num = String::new();
    if int_len <= 0 {
        num.push_str("0.");
        num.extend(core::iter::repeat_n('0', (-int_len) as usize));
        num.push_str(&sig);
    } else if int_len as usize >= sig.len() {
        num.push_str(&sig);
        num.extend(core::iter::repeat_n('0', int_len as usize - sig.len()));
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
}
