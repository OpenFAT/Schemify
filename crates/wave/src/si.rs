//! SI / SPICE engineering-suffix parsing and formatting.
//!
//! Parsing follows SPICE convention: suffixes are case-insensitive, so `M`
//! is milli and mega is spelled `meg`. Formatting emits display-style
//! prefixes (`m`, `k`, `M`, `G`, …).

/// Parse a number with an optional SPICE suffix: `5n`, `1k`, `2.5meg`,
/// `100`, `1e-9`, `3.3m`. Trailing unit letters after the suffix are
/// ignored (`10nF` parses as `10e-9`), matching SPICE.
pub fn parse_si(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Split numeric head from alpha tail. The head may contain an exponent
    // (`1e-9`), so 'e'/'E' followed by a digit or sign stays numeric.
    let bytes = s.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let b = bytes[end];
        let exponent = (b == b'e' || b == b'E')
            && end + 1 < bytes.len()
            && (bytes[end + 1].is_ascii_digit()
                || bytes[end + 1] == b'+'
                || bytes[end + 1] == b'-');
        if !(b.is_ascii_digit() || b == b'.' || b == b'+' || b == b'-' || exponent) {
            break;
        }
        end += 1;
    }
    let num: f64 = s[..end].parse().ok()?;
    let tail = s[end..].trim();
    if tail.is_empty() {
        return Some(num);
    }
    Some(num * suffix_multiplier(tail))
}

/// Multiplier for a SPICE suffix (with possible trailing unit letters).
fn suffix_multiplier(tail: &str) -> f64 {
    let t = tail.to_ascii_lowercase();
    // `meg` / `mil` are the only multi-letter suffixes; check them first so
    // `meg` doesn't parse as milli.
    if t.starts_with("meg") {
        return 1e6;
    }
    if t.starts_with("mil") {
        return 25.4e-6;
    }
    match t.as_bytes()[0] {
        b'f' => 1e-15,
        b'p' => 1e-12,
        b'n' => 1e-9,
        b'u' => 1e-6,
        b'm' => 1e-3,
        b'k' => 1e3,
        b'g' => 1e9,
        b't' => 1e12,
        // Bare unit letter ("10V") — no scaling.
        _ => 1.0,
    }
}

/// Format with an engineering prefix: `3.3e-9` → `"3.3n"`, `1.2e4` → `"12k"`.
/// `digits` = significant digits.
pub fn format_si(value: f64, digits: usize) -> String {
    if value == 0.0 || !value.is_finite() {
        return trim_zeros(&format!("{value:.*}", digits.saturating_sub(1)));
    }
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
    let mag = value.abs();
    let (scale, prefix) = PREFIXES
        .iter()
        .find(|(s, _)| mag >= *s)
        .copied()
        .unwrap_or((1e-15, "f"));
    let scaled = value / scale;
    // Significant digits: decimals = digits - integer digits of |scaled|.
    let int_digits = (scaled.abs().log10().floor() as i32 + 1).max(1) as usize;
    let decimals = digits.saturating_sub(int_digits);
    let s = format!("{scaled:.decimals$}");
    format!("{}{}", trim_zeros(&s), prefix)
}

/// Strip trailing fractional zeros ("1.500" → "1.5", "2.000" → "2").
fn trim_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_and_exponent() {
        assert_eq!(parse_si("100"), Some(100.0));
        assert_eq!(parse_si("1e-9"), Some(1e-9));
        assert_eq!(parse_si("-2.5"), Some(-2.5));
    }

    #[test]
    fn parse_suffixes() {
        assert_eq!(parse_si("5n"), Some(5e-9));
        assert_eq!(parse_si("1k"), Some(1e3));
        assert_eq!(parse_si("2.5meg"), Some(2.5e6));
        assert_eq!(parse_si("3.3m"), Some(3.3e-3));
        assert_eq!(parse_si("3.3M"), Some(3.3e-3)); // SPICE: M = milli
        assert_eq!(parse_si("10nF"), Some(10e-9)); // trailing unit ignored
        assert_eq!(parse_si("1u"), Some(1e-6));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_si(""), None);
        assert_eq!(parse_si("abc"), None);
    }

    #[test]
    fn format_engineering() {
        assert_eq!(format_si(3.3e-9, 3), "3.3n");
        assert_eq!(format_si(12_000.0, 3), "12k");
        assert_eq!(format_si(0.0, 3), "0");
        assert_eq!(format_si(1.0, 3), "1");
        assert_eq!(format_si(-4.7e-6, 3), "-4.7µ");
        assert_eq!(format_si(33.33e9, 4), "33.33G");
    }
}
