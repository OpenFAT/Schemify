//! Engineering-prefix formatting for axis ticks and readouts (`3.3n`, `12k`, `-4.7µ`).
//!
//! Display-only: the prefixes are the SI ones (`M` is mega), unlike SPICE input where `M` is
//! milli. Parsing user input goes through `schemify_expr::parse_num` (spec expr §2).
//!
//! Contents:
//! - `pub fn format_si(value: f64, digits: usize) -> String`

/// Prefixes from the largest down; values under `p` fall through to `f` (femto), where the
/// mantissa grows (`0.001f`) as in `eng()` (spec expr §5).
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

/// Formats `value` with `digits` significant digits (clamped to 1..=17) and an engineering
/// prefix: `3.3e-9` → `"3.3n"`, `1.2e4` → `"12k"`, `999.96e3` → `"1M"` (rounding carries into
/// the next prefix). Below femto the mantissa grows and keeps its digits (`1e-20` →
/// `"0.00001f"`). Zero (either sign) is `"0"`; NaN and infinities print as Rust does
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
    let mut k = PREFIXES
        .iter()
        .position(|(s, _)| value.abs() >= *s)
        .unwrap_or(PREFIXES.len());
    // Two passes at most: the second runs only when rounding reached 1000 of a prefix.
    for _ in 0..2 {
        debug_assert!(k <= PREFIXES.len(), "a prefix, or femto past the table");
        let (scale, prefix) = PREFIXES.get(k).copied().unwrap_or((1e-15, "f"));
        let scaled = value / scale;
        // Below femto `scaled` is under 1: its leading zeros are decimals too (`0.00001f`).
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
        return text;
    }
    unreachable!("a carry moves up one prefix, then fits")
}

// ---- private ----

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
        assert_eq!(format_si(1.5e15, 3), "1500T", "nothing above tera");
        assert_eq!(format_si(f64::NAN, 3), "NaN");
        assert_eq!(format_si(f64::NEG_INFINITY, 3), "-inf");
        assert_eq!(format_si(1234.5, 0), "1k", "digits clamp to at least one");
    }

    /// Regression: below femto the mantissa grows and keeps its digits, as `eng()` does
    /// (spec expr §5: `0.001f`); it once rounded to "0.000" and lost every digit ("0f").
    #[test]
    fn below_femto_keeps_its_digits() {
        assert_eq!(format_si(1e-20, 4), "0.00001f");
        assert_eq!(format_si(-1.234e-19, 4), "-0.0001234f");
        assert_eq!(format_si(9.9996e-16, 4), "1f", "rounding carries to 1");
        let tiny = format_si(f64::from_bits(1), 4);
        assert!(
            tiny.starts_with("0.000") && tiny.ends_with("4941f"),
            "{tiny}"
        );
    }

    /// Round trip over the whole normal range: the text reads back (mantissa times the SI
    /// prefix's spec value) within half a unit of the last kept digit.
    #[test]
    fn every_finite_value_reads_back() {
        let prefix = |c: char| match c {
            'T' => 1e12,
            'G' => 1e9,
            'M' => 1e6,
            'k' => 1e3,
            'm' => 1e-3,
            'µ' => 1e-6,
            'n' => 1e-9,
            'p' => 1e-12,
            'f' => 1e-15,
            _ => 1.0,
        };
        for e in -307..=18 {
            for m in [1.0, 1.5, 4.7, 9.99949, 9.9996] {
                for digits in 1..=6 {
                    let v = m * 10f64.powi(e);
                    let text = format_si(-v, digits);
                    let last = text.chars().last().expect("never empty");
                    let scale = prefix(last);
                    let mantissa = text.trim_end_matches(|c: char| !c.is_ascii_digit());
                    let back: f64 = mantissa.parse().expect("the mantissa is a number");
                    let tol = 0.5001 * 10f64.powi(1 - digits as i32) * v;
                    assert!(
                        (back * scale + v).abs() <= tol,
                        "{v:e} at {digits} digits: {text}"
                    );
                }
            }
        }
    }
}
