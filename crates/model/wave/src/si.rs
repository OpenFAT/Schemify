//! Engineering-prefix formatting for axis ticks and readouts (`3.3n`, `12k`, `-4.7µ`).
//!
//! Display-only: the prefixes are the SI ones (`M` is mega), unlike SPICE input where `M` is
//! milli. Parsing user input goes through `schemify_expr::parse_num` (spec expr §2).

/// Formats `value` with `digits` significant digits and an engineering prefix: `3.3e-9` →
/// `"3.3n"`, `1.2e4` → `"12k"`, `999.96e3` → `"1M"` (rounding carries into the next prefix).
/// Zero (either sign) is `"0"`; NaN and infinities print as Rust does (`"NaN"`, `"inf"`).
pub fn format_si(value: f64, digits: usize) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if !value.is_finite() {
        return format!("{value}");
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
    let digits = digits.clamp(1, 17);
    let mut k = PREFIXES
        .iter()
        .position(|(s, _)| value.abs() >= *s)
        .unwrap_or(PREFIXES.len());
    // Two passes at most: the second runs only when rounding reached 1000 of a prefix.
    for _ in 0..2 {
        let (scale, prefix) = PREFIXES.get(k).copied().unwrap_or((1e-15, "f"));
        let scaled = value / scale;
        let int_digits = (scaled.abs().log10().floor() as i32 + 1).max(1) as usize;
        let decimals = digits.saturating_sub(int_digits);
        let s = format!("{scaled:.decimals$}");
        let rounded: f64 = s.parse().unwrap_or(scaled);
        if rounded.abs() >= 1000.0 && k > 0 {
            k -= 1;
            continue;
        }
        return format!("{}{}", trim_zeros(&s), prefix);
    }
    unreachable!("a carry moves up one prefix, then fits")
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
    }
}
