//! Lexer (spec §2). Streams one token at a time: the parser holds the only
//! lookahead, so there is no token table. String literals are unescaped once,
//! straight into the caller's pool.

use crate::{Code, Error, MAX_STR_BYTES, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tok {
    Int,
    Num,
    Str,
    Ident,
    /// `ns.name`; the span covers both parts.
    NsIdent,
    True,
    False,
    Pi,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    Question,
    Colon,
    LParen,
    RParen,
    Comma,
    Eof,
}

/// Literal payload of a token. `Str` is a byte range in the caller's pool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Lit {
    None,
    Int(i64),
    Num(f64),
    Str { start: u32, len: u32 },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub kind: Tok,
    pub span: Span,
    pub lit: Lit,
}

/// SI suffix scale: a power of ten, or `mil` (25.4e-6, not a power of ten).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scale {
    Pow10(i32),
    Mil,
}

/// The spec §2 suffix table; lookup is ASCII-case-insensitive. Both micro
/// signs (U+00B5 and Greek U+03BC) are accepted.
pub(crate) const SUFFIXES: [(&str, Scale); 12] = [
    ("f", Scale::Pow10(-15)),
    ("p", Scale::Pow10(-12)),
    ("n", Scale::Pow10(-9)),
    ("u", Scale::Pow10(-6)),
    ("\u{b5}", Scale::Pow10(-6)),
    ("\u{3bc}", Scale::Pow10(-6)),
    ("m", Scale::Pow10(-3)),
    ("k", Scale::Pow10(3)),
    ("meg", Scale::Pow10(6)),
    ("g", Scale::Pow10(9)),
    ("t", Scale::Pow10(12)),
    ("mil", Scale::Mil),
];

/// Lexes the token starting at or after `pos` (whitespace skipped).
/// Spans are absolute offsets into `src`.
pub(crate) fn next_token(src: &str, pos: usize, pool: &mut String) -> Result<Token, Error> {
    assert!(
        pos <= src.len(),
        "lexer position {pos} past end {}",
        src.len()
    );
    let b = src.as_bytes();
    let mut i = pos;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    let simple = |kind: Tok, len: usize| {
        Ok(Token {
            kind,
            span: Span::new(i, i + len),
            lit: Lit::None,
        })
    };
    let Some(&c) = b.get(i) else {
        return simple(Tok::Eof, 0);
    };
    let next = b.get(i + 1).copied().unwrap_or(0);
    let tok = match c {
        b'0'..=b'9' => number_token(src, i)?,
        b'.' if next.is_ascii_digit() => number_token(src, i)?,
        b'"' => string(src, i, pool)?,
        b'A'..=b'Z' | b'a'..=b'z' | b'_' => ident(src, i),
        b'+' => return simple(Tok::Plus, 1),
        b'-' => return simple(Tok::Minus, 1),
        b'*' => return simple(Tok::Star, 1),
        b'/' => return simple(Tok::Slash, 1),
        b'%' => return simple(Tok::Percent, 1),
        b'^' => return simple(Tok::Caret, 1),
        b'?' => return simple(Tok::Question, 1),
        b':' => return simple(Tok::Colon, 1),
        b'(' => return simple(Tok::LParen, 1),
        b')' => return simple(Tok::RParen, 1),
        b',' => return simple(Tok::Comma, 1),
        b'=' if next == b'=' => return simple(Tok::EqEq, 2),
        b'!' if next == b'=' => return simple(Tok::NotEq, 2),
        b'!' => return simple(Tok::Bang, 1),
        b'<' if next == b'=' => return simple(Tok::Le, 2),
        b'<' => return simple(Tok::Lt, 1),
        b'>' if next == b'=' => return simple(Tok::Ge, 2),
        b'>' => return simple(Tok::Gt, 1),
        b'&' if next == b'&' => return simple(Tok::AndAnd, 2),
        b'|' if next == b'|' => return simple(Tok::OrOr, 2),
        _ => {
            let ch = src[i..]
                .chars()
                .next()
                .expect("index is on a char boundary");
            let hint = match ch {
                '=' => " (comparison is `==`)",
                '&' => " (logical and is `&&`)",
                '|' => " (logical or is `||`)",
                _ => "",
            };
            return Err(Error::new(
                Code::X009,
                Span::new(i, i + ch.len_utf8()),
                format!("unexpected character `{ch}`{hint}"),
            ));
        }
    };
    debug_assert!(tok.span.start as usize == i && tok.span.end > tok.span.start);
    Ok(tok)
}

fn ident(src: &str, start: usize) -> Token {
    let b = src.as_bytes();
    let word_end = |mut i: usize| {
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        i
    };
    let end = word_end(start + 1);
    let ns_name = b
        .get(end + 1)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_');
    if b.get(end) == Some(&b'.') && ns_name {
        let end2 = word_end(end + 2);
        return Token {
            kind: Tok::NsIdent,
            span: Span::new(start, end2),
            lit: Lit::None,
        };
    }
    let kind = match &src[start..end] {
        "true" => Tok::True,
        "false" => Tok::False,
        "pi" => Tok::Pi,
        _ => Tok::Ident,
    };
    Token {
        kind,
        span: Span::new(start, end),
        lit: Lit::None,
    }
}

fn number_token(src: &str, start: usize) -> Result<Token, Error> {
    let (lit, end) = number(src, start).map_err(|(end, why)| {
        let end = end.max(start + 1);
        Error::new(
            Code::X001,
            Span::new(start, end),
            format!("{why}: `{}`", &src[start..end]),
        )
    })?;
    let kind = if matches!(lit, Lit::Int(_)) {
        Tok::Int
    } else {
        Tok::Num
    };
    Ok(Token {
        kind,
        span: Span::new(start, end),
        lit,
    })
}

/// Parses the number literal at `start` (spec §2):
/// `digits[.digits][(e|E)[+-]digits][suffix]` or `.digits…`. Digits only, in
/// i64 range, is an `Int`; everything else is a `Num`. Any identifier
/// character after the literal (after the suffix, if any) is an error.
///
/// Returns the literal and the end offset, or the end of the offending text
/// and the reason.
pub(crate) fn number(src: &str, start: usize) -> Result<(Lit, usize), (usize, &'static str)> {
    let b = src.as_bytes();
    assert!(start < b.len(), "number lexing starts past the end");
    let digits = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = digits(start);
    let mut plain = true;
    if b.get(i) == Some(&b'.') {
        let frac = i + 1;
        i = digits(frac);
        if i == frac {
            return Err((i, "digit expected after `.`"));
        }
        plain = false;
    }
    if i == start {
        return Err((i + 1, "digit expected"));
    }
    let mant_end = i;
    let mut exp: i64 = 0;
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        let neg = b.get(j) == Some(&b'-');
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            let exp_end = digits(j);
            for &d in &b[j..exp_end] {
                // Saturate far outside f64's exponent range; the value then
                // overflows to inf (rejected below) or underflows to 0.
                exp = (exp * 10 + i64::from(d - b'0')).min(1_000_000);
            }
            if neg {
                exp = -exp;
            }
            i = exp_end;
            plain = false;
        }
        // Otherwise `e` is not an exponent; the suffix check rejects it.
    }
    let tail_start = i;
    for ch in src[i..].chars() {
        if !(ch.is_alphanumeric() || ch == '_') {
            break;
        }
        i += ch.len_utf8();
    }
    let tail = &src[tail_start..i];
    let scale = if tail.is_empty() {
        Scale::Pow10(0)
    } else {
        match SUFFIXES.iter().find(|(s, _)| s.eq_ignore_ascii_case(tail)) {
            Some(&(_, scale)) => scale,
            None => return Err((i, "bad SI suffix (trailing letters are not accepted)")),
        }
    };
    let mant = &src[start..mant_end];
    if plain
        && tail.is_empty()
        && let Ok(v) = mant.parse::<i64>()
    {
        return Ok((Lit::Int(v), i));
    }
    let v = match scale {
        Scale::Pow10(p) => decimal(mant, exp + i64::from(p)),
        Scale::Mil => decimal(mant, exp) * 25.4e-6,
    };
    if !v.is_finite() {
        return Err((i, "number out of range"));
    }
    debug_assert!(v >= 0.0, "literals are unsigned");
    Ok((Lit::Num(v), i))
}

/// Correctly rounded `mant × 10^exp` (a power-of-ten suffix is folded into
/// the decimal exponent, so `2.5p` is exactly the f64 nearest 2.5e-12).
fn decimal(mant: &str, exp: i64) -> f64 {
    debug_assert!(!mant.is_empty() && mant.bytes().all(|c| c.is_ascii_digit() || c == b'.'));
    let text = format!("{mant}e{exp}");
    text.parse::<f64>()
        .expect("digits and exponent always parse")
}

/// Lexes `"…"` at `start`, unescaping into `pool`.
fn string(src: &str, start: usize, pool: &mut String) -> Result<Token, Error> {
    assert_eq!(src.as_bytes()[start], b'"');
    let pool_start = pool.len();
    let mut chars = src[start + 1..].char_indices();
    let err =
        |at: usize, len: usize, msg: &str| Error::new(Code::X009, Span::new(at, at + len), msg);
    let end = loop {
        let Some((off, ch)) = chars.next() else {
            return Err(err(start, src.len() - start, "unterminated string"));
        };
        let at = start + 1 + off;
        match ch {
            '"' => break at + 1,
            '\\' => {
                let esc = match chars.next() {
                    Some((_, '"')) => '"',
                    Some((_, '\\')) => '\\',
                    Some((_, 'n')) => '\n',
                    Some((_, 't')) => '\t',
                    Some((_, 'u')) => unicode_escape(src, at, &mut chars)?,
                    _ => return Err(err(at, 1, "bad escape (allowed: \\\" \\\\ \\n \\t \\u{…})")),
                };
                pool.push(esc);
            }
            _ => pool.push(ch),
        }
        if pool.len() - pool_start > MAX_STR_BYTES {
            return Err(Error::new(
                Code::X002,
                Span::new(start, at),
                "string literal longer than 1 MiB",
            ));
        }
    };
    let lit = Lit::Str {
        start: pool_start as u32,
        len: (pool.len() - pool_start) as u32,
    };
    Ok(Token {
        kind: Tok::Str,
        span: Span::new(start, end),
        lit,
    })
}

/// `\u{X…}` after the `u`: 1–6 hex digits naming a Unicode scalar value.
fn unicode_escape(
    src: &str,
    at: usize,
    chars: &mut core::str::CharIndices<'_>,
) -> Result<char, Error> {
    let bad = |len: usize| {
        Error::new(
            Code::X009,
            Span::new(at, (at + len).min(src.len())),
            "bad \\u{…} escape",
        )
    };
    if chars.next().map(|(_, c)| c) != Some('{') {
        return Err(bad(2));
    }
    let mut v: u32 = 0;
    for n in 0..7 {
        match chars.next() {
            Some((_, '}')) if n > 0 => return char::from_u32(v).ok_or_else(|| bad(n + 4)),
            Some((_, c)) if n < 6 && c.is_ascii_hexdigit() => {
                v = v * 16 + c.to_digit(16).expect("hex digit")
            }
            _ => return Err(bad(n + 3)),
        }
    }
    Err(bad(10))
}
