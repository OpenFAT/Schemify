//! Net expressions (netlist spec §4): labels, ports, pin names, `inherit` nets and
//! instance names expand to an ordered list of bit names.
//!
//! ```text
//! netexpr = term *("," term)
//! term    = [count "*"] base
//! base    = ident ["[" int [":" int] "]"]
//! ident   = [A-Za-z_][A-Za-z0-9_$.]* ["!"]   ; or the literal "0"
//! ```
//!
//! Text outside the grammar is one scalar name, verbatim (amendment: importers keep
//! odd names working instead of failing the whole cell).

use crate::names::{Name, Names};

/// Most bits one expression expands to (the plugin `Nets.width` column is `u16`).
pub const WIDTH_MAX: u32 = u16::MAX as u32;

/// Why an expression could not be expanded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooWide;

/// Returns whether a bit name is global (netlist spec §3): `0`, or ending in `!`.
pub fn is_global(bit: &str) -> bool {
    bit == "0" || bit.ends_with('!')
}

/// Appends the bit names of `text` to `out` and returns how many were appended.
/// `buf` is scratch for building `name[i]` texts.
///
/// Cost: O(bits). Plain identifiers (the common case) intern `text` itself.
pub(crate) fn expand(
    text: &str,
    names: &mut Names,
    out: &mut Vec<Name>,
    buf: &mut String,
) -> Result<u32, TooWide> {
    let before = out.len();
    if !text.bytes().any(|b| matches!(b, b'[' | b',' | b'*')) {
        out.push(names.intern(text));
        return Ok(1);
    }
    let Some(terms) = parse(text) else {
        out.push(names.intern(text));
        return Ok(1);
    };
    let total: u64 = terms.iter().map(Term::width).sum();
    if total > u64::from(WIDTH_MAX) {
        return Err(TooWide);
    }
    for t in &terms {
        for _ in 0..t.count {
            match t.range {
                None => out.push(names.intern(t.ident)),
                Some((a, b)) => {
                    let step: i64 = if a <= b { 1 } else { -1 };
                    let mut i = i64::from(a);
                    loop {
                        buf.clear();
                        buf.push_str(t.ident);
                        buf.push('[');
                        buf.push_str(&i.to_string());
                        buf.push(']');
                        out.push(names.intern(buf));
                        if i == i64::from(b) {
                            break;
                        }
                        i += step;
                    }
                }
            }
        }
    }
    let n = (out.len() - before) as u32;
    assert_eq!(u64::from(n), total);
    Ok(n)
}

struct Term<'a> {
    count: u32,
    ident: &'a str,
    /// `(first, last)` index in written order.
    range: Option<(u32, u32)>,
}

impl Term<'_> {
    fn width(&self) -> u64 {
        let per = self.range.map_or(1, |(a, b)| u64::from(a.abs_diff(b)) + 1);
        u64::from(self.count) * per
    }
}

/// Parses the grammar; `None` when `text` is not a net expression.
fn parse(text: &str) -> Option<Vec<Term<'_>>> {
    let b = text.as_bytes();
    let mut terms = Vec::new();
    let mut i = 0;
    loop {
        // [count "*"]
        let mut count = 1u32;
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && b.get(i + digits) == Some(&b'*') {
            count = int(&text[i..i + digits])?;
            if count == 0 {
                return None;
            }
            i += digits + 1;
        }
        // ident: "0" or [A-Za-z_][A-Za-z0-9_$.]* ["!"]
        let start = i;
        if b.get(i) == Some(&b'0') && !b.get(i + 1).is_some_and(|c| ident_char(*c)) {
            i += 1;
        } else {
            if !b
                .get(i)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                return None;
            }
            while b.get(i).is_some_and(|c| ident_char(*c)) {
                i += 1;
            }
            if b.get(i) == Some(&b'!') {
                i += 1;
            }
        }
        let ident = &text[start..i];
        let mut range = None;
        if b.get(i) == Some(&b'[') {
            let close = i + text[i..].find(']')?;
            let inner = &text[i + 1..close];
            range = Some(match inner.split_once(':') {
                Some((x, y)) => (int(x)?, int(y)?),
                None => {
                    let v = int(inner)?;
                    (v, v)
                }
            });
            i = close + 1;
        }
        terms.push(Term {
            count,
            ident,
            range,
        });
        match b.get(i) {
            None => break,
            Some(b',') => i += 1,
            Some(_) => return None,
        }
    }
    assert!(!terms.is_empty());
    Some(terms)
}

fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'.')
}

fn int(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(text: &str) -> Vec<String> {
        let mut names = Names::default();
        let (mut out, mut buf) = (Vec::new(), String::new());
        let n = expand(text, &mut names, &mut out, &mut buf).unwrap();
        assert_eq!(n as usize, out.len());
        out.iter().map(|x| names.get(*x).to_owned()).collect()
    }

    #[test]
    fn expansion_table() {
        let cases: &[(&str, &[&str])] = &[
            ("a", &["a"]),
            ("0", &["0"]),
            ("vdd!", &["vdd!"]),
            ("d[3:0]", &["d[3]", "d[2]", "d[1]", "d[0]"]),
            ("d[0:2]", &["d[0]", "d[1]", "d[2]"]),
            ("d[5]", &["d[5]"]),
            ("2*a,b", &["a", "a", "b"]),
            ("a,b[1:0],c", &["a", "b[1]", "b[0]", "c"]),
            ("2*d[1:0]", &["d[1]", "d[0]", "d[1]", "d[0]"]),
            ("x.y$z", &["x.y$z"]),
            ("X1[3:0]", &["X1[3]", "X1[2]", "X1[1]", "X1[0]"]),
            // Outside the grammar: one verbatim name.
            ("a[", &["a["]),
            ("a[x]", &["a[x]"]),
            ("1a,b", &["1a,b"]),
            ("a,,b", &["a,,b"]),
            ("0*a", &["0*a"]),
            ("a b", &["a b"]),
            ("a]", &["a]"]),
        ];
        for (text, want) in cases {
            assert_eq!(bits(text), *want, "{text}");
        }
        assert!(is_global("0") && is_global("vdd!") && !is_global("vdd") && !is_global("00"));
    }

    #[test]
    fn over_wide_expressions_are_refused() {
        let mut names = Names::default();
        let (mut out, mut buf) = (Vec::new(), String::new());
        assert_eq!(
            expand("d[65534:0]", &mut names, &mut out, &mut buf),
            Ok(65535)
        );
        out.clear();
        assert_eq!(
            expand("d[65535:0]", &mut names, &mut out, &mut buf),
            Err(TooWide)
        );
        assert_eq!(
            expand("70000*a", &mut names, &mut out, &mut buf),
            Err(TooWide)
        );
        let before = names.len();
        assert!(expand("2*d[40000:0]", &mut names, &mut out, &mut buf).is_err());
        assert!(
            out.is_empty() && names.len() == before,
            "a refused expression interns nothing"
        );
    }
}
