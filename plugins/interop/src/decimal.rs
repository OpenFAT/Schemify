//! Exact decimal text → scaled integer, the fixed-point entry point for every reader
//! that stores integers (xschem milli-units D33, tech-file picometres).
//!
//! Works on the digits, never through `f64`, so `0.078125` scaled by 10^3 is known to be
//! `78.125` and therefore inexact; a float round trip could not tell.

/// Most significant digits kept from a mantissa; later digits only feed the rounding.
const MAX_DIGITS: u32 = 36;
/// Exponents beyond this magnitude are clamped (they overflow or underflow anyway).
const MAX_EXPONENT: i32 = 10_000;

/// A decimal converted to `round(x · 10^scale)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scaled {
    /// The rounded value (round half away from zero).
    pub value: i64,
    /// Rounding error `x · 10^scale − value` in thousandths of one scaled unit,
    /// within `[-500, 500]`; `0` with `exact == false` means the error is below 1/2000.
    pub error_milli: i16,
    /// True when no rounding happened.
    pub exact: bool,
}

/// Why a token is not a usable decimal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    /// Not `[+-]? (d+ [. d*] | . d+) ([eE] [+-]? d+)?` in its entirety.
    Syntax,
    /// The scaled value does not fit an `i64`.
    Overflow,
}

/// Parses C-style decimal `text` (what `%lf` accepts minus hex, `inf` and `nan`) and
/// returns it multiplied by `10^scale`, rounded half away from zero.
///
/// # Errors
///
/// [`DecimalError::Syntax`] unless the whole token is a decimal; [`DecimalError::Overflow`]
/// when the scaled value leaves `i64`.
///
/// # Examples
///
/// ```
/// use interop::decimal::parse_scaled;
/// let s = parse_scaled("-2.5e-1", 3)?;
/// assert_eq!((s.value, s.exact), (-250, true));
/// let r = parse_scaled("0.078125", 3)?;
/// assert_eq!((r.value, r.exact, r.error_milli), (78, false, 125));
/// # Ok::<(), interop::decimal::DecimalError>(())
/// ```
pub fn parse_scaled(text: &str, scale: i32) -> Result<Scaled, DecimalError> {
    assert!((-38..=38).contains(&scale), "scale {scale} out of range");
    let b = text.as_bytes();
    let mut i = 0;
    let negative = match b.first() {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut digits: u128 = 0;
    let mut kept: u32 = 0;
    let mut shift: i32 = 0; // value = digits · 10^shift
    let mut sticky = false; // a dropped non-zero digit exists
    let mut any_digit = false;
    while i < b.len() && b[i].is_ascii_digit() {
        let d = u128::from(b[i] - b'0');
        any_digit = true;
        if kept < MAX_DIGITS {
            if digits != 0 || d != 0 {
                digits = digits * 10 + d;
                kept += 1;
            }
        } else {
            shift += 1;
            sticky |= d != 0;
        }
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            let d = u128::from(b[i] - b'0');
            any_digit = true;
            if kept < MAX_DIGITS {
                if digits != 0 || d != 0 {
                    digits = digits * 10 + d;
                    kept += 1;
                }
                shift -= 1;
            } else {
                sticky |= d != 0;
            }
            i += 1;
        }
    }
    if !any_digit {
        return Err(DecimalError::Syntax);
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let exp_negative = match b.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let exp_start = i;
        let mut exp: i32 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            exp = (exp * 10 + i32::from(b[i] - b'0')).min(MAX_EXPONENT);
            i += 1;
        }
        if i == exp_start {
            return Err(DecimalError::Syntax);
        }
        shift += if exp_negative { -exp } else { exp };
    }
    if i != b.len() {
        return Err(DecimalError::Syntax);
    }
    let total = shift + scale;
    let (magnitude, error_milli, exact) = if digits == 0 {
        (0u128, 0i16, !sticky)
    } else if total >= 0 {
        if total > 38 {
            return Err(DecimalError::Overflow);
        }
        let m = digits
            .checked_mul(10u128.pow(total as u32))
            .ok_or(DecimalError::Overflow)?;
        (m, 0, !sticky)
    } else if total < -38 {
        (0, 0, false)
    } else {
        let div = 10u128.pow((-total) as u32);
        let (q, r) = (digits / div, digits % div);
        let up = r >= div - r; // r ≥ div/2 without overflow; ties round away from zero
        let frac = r as f64 / div as f64 - if up { 1.0 } else { 0.0 };
        let error = (frac * 1000.0).round() as i16;
        (q + u128::from(up), error, r == 0 && !sticky)
    };
    let value = i64::try_from(magnitude).map_err(|_| DecimalError::Overflow)?;
    let (value, error_milli) = if negative {
        (-value, -error_milli)
    } else {
        (value, error_milli)
    };
    debug_assert!((-500..=500).contains(&error_milli));
    debug_assert!(!exact || error_milli == 0);
    Ok(Scaled {
        value,
        error_milli,
        exact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(t: &str, s: i32) -> (i64, bool) {
        let v = parse_scaled(t, s).unwrap();
        (v.value, v.exact)
    }

    #[test]
    fn exact_and_rounded_forms() {
        assert_eq!(ok("0", 3), (0, true));
        assert_eq!(ok("-0.0", 3), (0, true));
        assert_eq!(ok("12.5", 3), (12500, true));
        assert_eq!(ok("+.5", 3), (500, true));
        assert_eq!(ok("5.", 3), (5000, true));
        assert_eq!(ok("1e3", 3), (1_000_000, true));
        assert_eq!(ok("1E-3", 3), (1, true));
        assert_eq!(ok("0.0005", 3), (1, false)); // tie rounds away from zero
        assert_eq!(ok("-0.0005", 3), (-1, false));
        assert_eq!(ok("0.00049", 3), (0, false));
        assert_eq!(ok("19.921875", 3), (19922, false));
        assert_eq!(ok("0.30000000000000004", 3), (300, false));
        assert_eq!(ok("0.001", 6), (1000, true)); // µm → pm
        assert_eq!(ok("1e-400", 3), (0, false));
    }

    #[test]
    fn rejects_non_decimals() {
        for t in [
            "", "-", ".", "e3", "1e", "1e+", "inf", "nan", "0x10", "1.2.3", "1 ", "1f",
        ] {
            assert_eq!(parse_scaled(t, 3), Err(DecimalError::Syntax), "{t:?}");
        }
        assert_eq!(parse_scaled("1e30", 3), Err(DecimalError::Overflow));
        assert_eq!(
            parse_scaled("9223372036854775808", 0),
            Err(DecimalError::Overflow)
        );
    }

    #[test]
    fn error_sign_matches_direction() {
        let v = parse_scaled("0.0004", 3).unwrap();
        assert_eq!((v.value, v.error_milli), (0, 400));
        let v = parse_scaled("0.0006", 3).unwrap();
        assert_eq!((v.value, v.error_milli), (1, -400));
    }
}
