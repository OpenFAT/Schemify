//! File-level numbers, coordinates and orientation (spec §3).

use std::fmt::Write as _;

/// Milli-units per xschem unit: a coordinate `12.5` is stored as `12500` (D33).
pub const MILLI_PER_UNIT: i32 = 1000;

/// Largest integer part a representable coordinate can have: `2^31 / 1000`, rounded up.
/// Accumulation stops past it, so the i64 accumulator never overflows.
const COORD_WHOLE_MAX: i64 = 2_147_484;

/// Why [`parse_coord`] rejected its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordError {
    /// Not a file-level number literal (`E029`).
    NotNumber,
    /// More than 3 significant fractional digits, or outside the i32 milli-unit range (`E022`).
    NotRepresentable,
}

/// Returns whether `s` is a file-level number literal: `-?digits(.digits)?`, with no
/// exponent, no `+` and no SI suffix (those belong to expressions).
pub fn is_number(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (body, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    digits(whole) && frac.is_none_or(digits)
}

/// Parses a coordinate into exact i32 milli-units.
///
/// Fractional digits beyond the third are accepted only when they are zeros
/// (`1.5000` is exact; `0.0005` is not).
///
/// # Errors
///
/// [`CoordError::NotNumber`] when `s` is not a number literal; [`CoordError::NotRepresentable`]
/// when the value needs a fourth fractional digit or falls outside `[-2^31, 2^31 - 1]`
/// milli-units.
///
/// # Examples
///
/// ```
/// use schemify_format::{parse_coord, CoordError};
/// assert_eq!(parse_coord("-12.5"), Ok(-12_500));
/// assert_eq!(parse_coord("0.0005"), Err(CoordError::NotRepresentable));
/// assert_eq!(parse_coord("1e3"), Err(CoordError::NotNumber));
/// ```
pub fn parse_coord(s: &str) -> Result<i32, CoordError> {
    if !is_number(s) {
        return Err(CoordError::NotNumber);
    }
    let (negative, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let (whole_text, frac_text) = body.split_once('.').unwrap_or((body, ""));
    let frac_text = frac_text.trim_end_matches('0');
    if frac_text.len() > 3 {
        return Err(CoordError::NotRepresentable);
    }
    assert!(
        !whole_text.is_empty(),
        "is_number guarantees an integer part"
    );
    let mut whole: i64 = 0;
    for d in whole_text.bytes() {
        whole = whole * 10 + i64::from(d - b'0');
        if whole > COORD_WHOLE_MAX {
            return Err(CoordError::NotRepresentable);
        }
    }
    let mut milli = whole * i64::from(MILLI_PER_UNIT);
    for (d, scale) in frac_text.bytes().zip([100, 10, 1]) {
        milli += i64::from(d - b'0') * scale;
    }
    let value = if negative { -milli } else { milli };
    assert!(value.unsigned_abs() <= (COORD_WHOLE_MAX as u64 + 1) * 1000);
    i32::try_from(value).map_err(|_| CoordError::NotRepresentable)
}

/// Appends the shortest decimal for `milli` milli-units: `-60`, `12.5`, `0.156`.
///
/// The output always parses back to `milli` with [`parse_coord`].
pub fn format_coord(milli: i32, out: &mut String) {
    let len_before = out.len();
    let value = i64::from(milli);
    if value < 0 {
        out.push('-');
    }
    let magnitude = value.unsigned_abs();
    let (whole, frac) = (magnitude / 1000, magnitude % 1000);
    write!(out, "{whole}").expect("writing to a String cannot fail");
    if frac != 0 {
        let digits = [frac / 100, frac / 10 % 10, frac % 10];
        let len = 3 - digits.iter().rev().take_while(|&&d| d == 0).count();
        out.push('.');
        for d in &digits[..len] {
            out.push(char::from(b'0' + *d as u8));
        }
    }
    assert!(out.len() > len_before && !out.ends_with('.'));
}

/// Symbol placement: an optional mirror followed by a clockwise quarter-turn rotation
/// (spec §3). The discriminant is `quarter_turns + 4 * mirrored`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs)] // variant names are the file words
pub enum Orient {
    R0,
    R90,
    R180,
    R270,
    MR0,
    MR90,
    MR180,
    MR270,
}

const ORIENT_WORDS: [&str; 8] = ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"];
const ORIENTS: [Orient; 8] = [
    Orient::R0,
    Orient::R90,
    Orient::R180,
    Orient::R270,
    Orient::MR0,
    Orient::MR90,
    Orient::MR180,
    Orient::MR270,
];

impl Orient {
    /// Parses a file word; case-sensitive, so `r90` is `None`.
    pub fn parse(word: &str) -> Option<Orient> {
        let i = ORIENT_WORDS.iter().position(|w| *w == word)?;
        let o = ORIENTS[i];
        assert_eq!(o as usize, i, "table order is discriminant order");
        Some(o)
    }

    /// Returns the file word.
    pub fn as_str(self) -> &'static str {
        ORIENT_WORDS[self as usize]
    }

    /// Returns whether x is negated before rotating.
    pub fn mirrored(self) -> bool {
        self as u8 >= 4
    }

    /// Returns the clockwise rotation in quarter turns, `0..=3`.
    pub fn quarter_turns(self) -> u8 {
        self as u8 % 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(m: i32) -> String {
        let mut s = String::new();
        format_coord(m, &mut s);
        s
    }

    #[test]
    fn coordinates_parse_exactly_and_format_shortest() {
        let cases = [
            ("0", 0, "0"),
            ("-0", 0, "0"),
            ("-60", -60_000, "-60"),
            ("12.5", 12_500, "12.5"),
            ("0.156", 156, "0.156"),
            ("1.5000", 1_500, "1.5"),
            ("007.010", 7_010, "7.01"),
            ("2147483.647", i32::MAX, "2147483.647"),
            ("-2147483.648", i32::MIN, "-2147483.648"),
        ];
        for (text, milli, canonical) in cases {
            assert_eq!(parse_coord(text), Ok(milli), "{text}");
            assert_eq!(fmt(milli), canonical);
            assert_eq!(parse_coord(canonical), Ok(milli));
        }
    }

    #[test]
    fn coordinates_reject_negative_space() {
        for bad in [
            "", "-", "+1", "1.", ".5", "1e3", "1u", "0x10", " 1", "1 ", "--1", "1.2.3",
        ] {
            assert_eq!(parse_coord(bad), Err(CoordError::NotNumber), "{bad:?}");
            assert!(!is_number(bad));
        }
        for bad in [
            "0.0005",
            "1.2345",
            "2147483.648",
            "-2147483.649",
            "99999999999999999999999",
        ] {
            assert_eq!(
                parse_coord(bad),
                Err(CoordError::NotRepresentable),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn coordinate_format_round_trips_on_a_sweep() {
        let mut m: i64 = i64::from(i32::MIN);
        while m <= i64::from(i32::MAX) {
            let v = m as i32;
            assert_eq!(parse_coord(&fmt(v)), Ok(v));
            m += 7_919_993; // prime stride hits every digit pattern of the fraction
        }
    }

    #[test]
    fn orientations_round_trip() {
        for (i, o) in ORIENTS.iter().enumerate() {
            assert_eq!(Orient::parse(o.as_str()), Some(*o));
            assert_eq!(o.mirrored(), i >= 4);
            assert_eq!(usize::from(o.quarter_turns()), i % 4);
        }
        assert_eq!(Orient::parse("r90"), None);
        assert_eq!(Orient::parse("R45"), None);
    }
}
