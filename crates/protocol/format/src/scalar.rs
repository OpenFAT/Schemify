//! File-level numbers, coordinates and orientation: text ↔ i32 milli-units and
//! [`Orient`] (spec §3, D33).
//!
//! Contents:
//! - `pub enum CoordError`
//! - `pub enum Orient`, `Orient::parse`, `Orient::as_str`, `Orient::mirrored`,
//!   `Orient::quarter_turns`
//! - `pub fn is_number(s: &str) -> bool`
//! - `pub fn parse_coord(s: &str) -> Result<i32, CoordError>`
//! - `pub fn format_coord(milli: i32, out: &mut String)`

/// Why [`parse_coord`] rejected its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordError {
    /// Not a file-level number literal (`E029`).
    NotNumber,
    /// More than 3 significant fractional digits, or outside the i32 milli-unit range (`E022`).
    NotRepresentable,
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

/// Milli-units per xschem unit: a coordinate `12.5` is stored as `12500` (D33).
const MILLI_PER_UNIT: i64 = 1000;
/// Largest integer part a representable coordinate can have: `2^31 / 1000`, rounded up.
/// Accumulation stops past it, so the i64 accumulator never overflows.
const COORD_WHOLE_MAX: i64 = 2_147_484;
/// File words and values of [`Orient`], in discriminant order.
const ORIENTS: [(&str, Orient); 8] = [
    ("R0", Orient::R0),
    ("R90", Orient::R90),
    ("R180", Orient::R180),
    ("R270", Orient::R270),
    ("MR0", Orient::MR0),
    ("MR90", Orient::MR90),
    ("MR180", Orient::MR180),
    ("MR270", Orient::MR270),
];

/// Returns whether `s` is a file-level number literal: `-?digits(.digits)?`, with no
/// exponent, no `+` and no SI suffix (those belong to expressions). Any number of
/// digits is a number, representable as a coordinate or not.
pub fn is_number(s: &str) -> bool {
    parse_coord(s) != Err(CoordError::NotNumber)
}

/// Parses a coordinate into exact i32 milli-units, in one pass over the bytes.
///
/// Fractional digits beyond the third are accepted only when they are zeros
/// (`1.5000` is exact; `0.0005` is not).
///
/// # Errors
///
/// [`CoordError::NotNumber`] when `s` is not a number literal (checked over the whole
/// text first, so `99999999999x` is `NotNumber`); [`CoordError::NotRepresentable`]
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
    let b = s.as_bytes();
    let negative = b.first() == Some(&b'-');
    let body = &b[usize::from(negative)..];
    let whole_len = body.iter().take_while(|c| c.is_ascii_digit()).count();
    if whole_len == 0 {
        return Err(CoordError::NotNumber);
    }
    // Saturates one past the limit, so the i64 never overflows on long digit runs.
    let whole = body[..whole_len].iter().fold(0i64, |w, d| {
        (w * 10 + i64::from(d - b'0')).min(COORD_WHOLE_MAX + 1)
    });
    let (mut frac, mut exact) = (0i64, true);
    if let Some(rest) = body.get(whole_len..).filter(|r| !r.is_empty()) {
        let digits = &rest[1..];
        if rest[0] != b'.' || digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return Err(CoordError::NotNumber);
        }
        for (i, d) in digits.iter().enumerate() {
            match [100, 10, 1].get(i) {
                Some(scale) => frac += i64::from(d - b'0') * scale,
                None => exact &= *d == b'0',
            }
        }
    }
    if !exact || whole > COORD_WHOLE_MAX {
        return Err(CoordError::NotRepresentable);
    }
    let milli = whole * MILLI_PER_UNIT + frac;
    let value = if negative { -milli } else { milli };
    assert!(value.unsigned_abs() <= (COORD_WHOLE_MAX as u64 + 1) * 1000);
    i32::try_from(value).map_err(|_| CoordError::NotRepresentable)
}

/// Appends the shortest decimal for `milli` milli-units: `-60`, `12.5`, `0.156`.
///
/// The output always parses back to `milli` with [`parse_coord`]. Digit by digit, without
/// the `fmt` machinery: a save formats ~3 coordinates per object.
pub fn format_coord(milli: i32, out: &mut String) {
    let before = out.len();
    if milli < 0 {
        out.push('-');
    }
    let mag = i64::from(milli).unsigned_abs();
    let (whole, mut frac) = (mag / 1000, (mag % 1000) as u32);
    let mut digits = [0u8; 10];
    let (mut n, mut w) = (0, whole);
    loop {
        digits[n] = (w % 10) as u8;
        n += 1;
        w /= 10;
        if w == 0 {
            break;
        }
    }
    for &d in digits[..n].iter().rev() {
        out.push(char::from(b'0' + d));
    }
    if frac != 0 {
        out.push('.');
        let mut scale = 100;
        while frac != 0 {
            out.push(char::from(b'0' + (frac / scale) as u8));
            frac %= scale;
            scale /= 10;
        }
    }
    assert!(out.len() > before && !out.ends_with('.'));
}

impl Orient {
    /// Parses a file word; case-sensitive, so `r90` is `None`.
    pub fn parse(word: &str) -> Option<Orient> {
        let (_, o) = ORIENTS.iter().find(|(w, _)| *w == word)?;
        assert_eq!(o.as_str(), word, "table order is discriminant order");
        Some(*o)
    }

    /// Returns the file word.
    pub fn as_str(self) -> &'static str {
        ORIENTS[self as usize].0
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

    /// A split-then-convert implementation, independent of the one-pass scanner: the
    /// oracle for `parse_coord`.
    fn oracle(s: &str) -> Result<i32, CoordError> {
        let body = s.strip_prefix('-').unwrap_or(s);
        let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || (body.contains('.') && !digits(frac)) {
            return Err(CoordError::NotNumber);
        }
        let frac = frac.trim_end_matches('0');
        if frac.len() > 3 || whole.trim_start_matches('0').len() > 7 {
            return Err(CoordError::NotRepresentable);
        }
        let padded = format!("{whole}{frac:0<3}");
        let milli: i64 = padded.parse().expect("digits");
        let value = if s.starts_with('-') { -milli } else { milli };
        i32::try_from(value).map_err(|_| CoordError::NotRepresentable)
    }

    #[test]
    fn one_pass_parse_matches_the_oracle_exhaustively() {
        // Every string of length 0..=6 over the bytes that matter, plus long digit runs.
        let alphabet = b"-.0159x";
        let mut checked = 0u32;
        for len in 0..=6u32 {
            for mut n in 0..7usize.pow(len) {
                let mut s = String::new();
                for _ in 0..len {
                    s.push(char::from(alphabet[n % 7]));
                    n /= 7;
                }
                assert_eq!(parse_coord(&s), oracle(&s), "{s:?}");
                checked += 1;
            }
        }
        for s in [
            "2147483.647",
            "-2147483.648",
            "2147483.6470",
            "0002147483.648",
            "99999999999x",
        ] {
            assert_eq!(parse_coord(s), oracle(s), "{s:?}");
        }
        assert_eq!(checked, (0..=6).map(|l| 7u32.pow(l)).sum::<u32>());
    }

    /// Oracles: `parse_coord`, and `f64`'s `Display` (the shortest decimal that reads back,
    /// exact for ten significant digits) for the shortest form.
    #[test]
    fn coordinate_format_round_trips_on_a_sweep() {
        let mut m: i64 = i64::from(i32::MIN);
        let edges = [
            0,
            1,
            -1,
            5,
            50,
            999,
            1000,
            -1000,
            1001,
            1010,
            i32::MAX,
            i32::MIN,
        ];
        let sweep = std::iter::from_fn(|| {
            let v = (m <= i64::from(i32::MAX)).then_some(m as i32);
            m += 7_919_993; // prime stride hits every digit pattern of the fraction
            v
        });
        for v in sweep.chain(edges) {
            assert_eq!(parse_coord(&fmt(v)), Ok(v));
            assert_eq!(fmt(v), format!("{}", f64::from(v) / 1000.0), "{v}");
        }
    }

    #[test]
    fn orientations_round_trip() {
        for (i, (_, o)) in ORIENTS.iter().enumerate() {
            assert_eq!(Orient::parse(o.as_str()), Some(*o));
            assert_eq!(o.mirrored(), i >= 4);
            assert_eq!(usize::from(o.quarter_turns()), i % 4);
        }
        assert_eq!(Orient::parse("r90"), None);
        assert_eq!(Orient::parse("R45"), None);
    }
}
