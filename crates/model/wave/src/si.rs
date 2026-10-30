//! Engineering-prefix formatting for axis ticks and readouts (`3.3n`, `12k`, `-4.7µ`).
//!
//! Display-only: the prefixes are the SI ones (`M` is mega), unlike SPICE input where `M` is
//! milli. Parsing user input goes through `schemify_expr::parse_num` (spec expr §2). The
//! range rule is `eng()`'s (spec expr §5), so a label and an `eng()` value agree.
//!
//! Contents:
//! - `pub fn format_si(value: f64, digits: usize) -> String`

use core::ops::Range;

/// Prefixes from the largest down; values under `p` fall through to `f` (femto).
const PREFIXES: [(f64, &str); 9] = [
    (1e12, "T"),
    (1e9, "G"),
    (1e6, "M"),
    (1e3, "k"),
    (1.0, ""),
    (1e-3, "m"),
    (1e-6, "µ"),
    (1e-9, "n"),
    (1e-12, "p"),
];

/// Decimal exponents (of the value rounded to its digits) shown with a prefix: the mantissa
/// grows at most three digits past the table's ends (`0.001f`, `999999T`); outside it is
/// scientific notation, so a label stays narrow.
const PREFIX_EXPONENTS: Range<i32> = -18..18;

/// Formats `value` with `digits` significant digits (clamped to 1..=17) and an engineering
/// prefix: `3.3e-9` → `"3.3n"`, `1.2e4` → `"12k"`, `999.96e3` → `"1M"` (rounding carries into
/// the next prefix). Past the table's ends the mantissa grows up to three digits (`0.001f`,
/// `999999T`), then the text is scientific (`1e-20`, `4.941e-324`, `1e18`), and it rounds
/// toward zero where rounding up would pass `f64::MAX`. Every finite value is at most
/// `digits + 7` bytes. Zero (either sign) is `"0"`; NaN and infinities print as Rust does
/// (`"NaN"`, `"inf"`).
#[must_use]
pub fn format_si(value: f64, digits: usize) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if !value.is_finite() {
        return format!("{value}");
    }
    let digits = digits.clamp(1, 17);
    if !PREFIX_EXPONENTS.contains(&rounded_exponent(value, digits)) {
        let mut text = String::new();
        schemify_expr::write_scientific(&mut text, value, digits);
        return text;
    }
    let mut k = PREFIXES
        .iter()
        .position(|(s, _)| value.abs() >= *s)
        .unwrap_or(PREFIXES.len());
    // Two passes at most: the second runs only when rounding reached 1000 of a prefix.
    for _ in 0..2 {
        debug_assert!(k <= PREFIXES.len(), "a prefix, or femto past the table");
        let (scale, prefix) = PREFIXES.get(k).copied().unwrap_or((1e-15, "f"));
        let scaled = value / scale;
        // Below femto `scaled` is under 1: its leading zeros are decimals too (`0.001f`).
        let lead = scaled.abs().log10().floor() as i32;
        let decimals = (digits as i32 - 1 - lead).max(0) as usize;
        let s = format!("{scaled:.decimals$}");
        let rounded: f64 = s.parse().unwrap_or(scaled);
        if rounded.abs() >= 1000.0 && k > 0 {
            k -= 1;
            continue;
        }
        let text = format!("{}{prefix}", trim_zeros(&s));
        debug_assert!(
            text.bytes().any(|b| (b'1'..=b'9').contains(&b)),
            "a nonzero value keeps a nonzero digit: {text}"
        );
        debug_assert!(text.len() <= digits + 7, "{text} is wider than its bound");
        return text;
    }
    unreachable!("a carry moves up one prefix, then fits")
}

// ---- private ----

/// The decimal exponent of `value` rounded to `digits` significant digits, so a carry
/// (999.95e15 → 1e18) counts.
fn rounded_exponent(value: f64, digits: usize) -> i32 {
    assert!((1..=17).contains(&digits), "digits {digits} outside 1..=17");
    let rounded = format!("{:.*e}", digits - 1, value.abs());
    let (_, exponent) = rounded.split_once('e').expect("LowerExp has an exponent");
    exponent.parse().expect("LowerExp exponent is an integer")
}

/// Strips trailing fractional zeros ("1.500" → "1.5", "2.000" → "2").
fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_engineering() {
        assert_eq!(format_si(3.3e-9, 3), "3.3n");
        assert_eq!(format_si(12_000.0, 3), "12k");
        assert_eq!(format_si(0.0, 3), "0");
        assert_eq!(format_si(1.0, 3), "1");
        assert_eq!(format_si(-4.7e-6, 3), "-4.7µ");
        assert_eq!(format_si(33.33e9, 4), "33.33G");
        assert_eq!(format_si(999.96e3, 3), "1M");
        assert_eq!(format_si(-0.0, 3), "0");
        assert_eq!(format_si(4e-16, 2), "0.4f");
        assert_eq!(format_si(9.9996e-16, 4), "1f", "rounding carries to 1");
        assert_eq!(format_si(1.5e15, 3), "1500T", "nothing above tera");
        assert_eq!(format_si(f64::NAN, 3), "NaN");
        assert_eq!(format_si(f64::NEG_INFINITY, 3), "-inf");
        assert_eq!(format_si(1234.5, 0), "1k", "digits clamp to at least one");
    }

    /// Regression: below femto the mantissa once lost every digit ("0f"), and a fix that let
    /// it grow ran to hundreds of characters. Three digits past either end of the table
    /// (`0.001f`, `999999T`), then scientific notation; the edges are decided after rounding.
    #[test]
    fn beyond_the_prefixes_is_scientific() {
        let cases: [(f64, usize, &str); 16] = [
            (1e-20, 4, "1e-20"),
            (-1.234e-19, 4, "-1.234e-19"),
            (1e-18, 4, "0.001f"),
            (-1e-18, 1, "-0.001f"),
            (9.99e-19, 3, "9.99e-19"),
            (9.9996e-19, 4, "0.001f"),
            (999.94e15, 4, "999940T"), // the integer part keeps its digits, as `1500T`
            (999.95e15, 4, "1e18"),
            (1e18, 4, "1e18"),
            (1e300, 4, "1e300"),
            (f64::from_bits(1), 4, "4.941e-324"),
            (f64::MIN_POSITIVE, 3, "2.23e-308"),
            (f64::MAX, 4, "1.797e308"),
            (f64::MAX, 1, "1e308"),
            (-f64::MAX, 17, "-1.7976931348623157e308"),
            (f64::MAX, 3, "1.79e308"),
        ];
        for (x, digits, want) in cases {
            assert_eq!(format_si(x, digits), want, "{x:e} at {digits} digits");
        }
    }

    /// Every finite value at every digit count: at most `digits + 7` bytes, and the text reads
    /// back through `schemify_expr::parse_num` (SPICE's `meg` for the SI `M`) within half a
    /// unit of its last digit (a whole unit where `f64::MAX` truncates), plus half the
    /// smallest subnormal.
    #[test]
    fn every_finite_value_is_narrow_and_reads_back() {
        let edges = [1e-18, 1e18, f64::MIN_POSITIVE, f64::MAX, f64::from_bits(1)];
        let neighbours = edges.iter().flat_map(|x| {
            let b = x.to_bits();
            [b - 1, b, b + 1].map(f64::from_bits)
        });
        let sweep =
            (-324..=308).flat_map(|e| [1.0, 1.5, 4.7, 9.99949, 9.9996].map(|m| m * 10f64.powi(e)));
        let values: Vec<f64> = sweep
            .chain(neighbours)
            .filter(|v| v.is_finite() && *v != 0.0)
            .collect();
        for v in values {
            for digits in 1..=17 {
                for x in [v, -v] {
                    let text = format_si(x, digits);
                    assert!(text.len() <= digits + 7, "{x:e} at {digits}: {text}");
                    let spice = match text.strip_suffix('M') {
                        Some(m) => format!("{m}meg"),
                        None => text.clone(),
                    };
                    let back = schemify_expr::parse_num(&spice).expect("the text is a number");
                    let unit = if v > 1e308 { 1.0 } else { 0.5 };
                    let tol = unit * 10f64.powi(1 - digits as i32) * v + 4.0 * f64::EPSILON * v;
                    assert!(
                        (back - x).abs() <= tol + f64::from_bits(1),
                        "{x:e} at {digits} digits: {text} read back as {back:e}"
                    );
                }
            }
        }
    }
}
