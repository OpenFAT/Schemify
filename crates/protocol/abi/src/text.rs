//! Line-record text ↔ words, values, body lines and record chunks: the subset of the
//! line-record syntax that events and outputs use (bare and quoted values, indented attribute
//! lines, `|` verbatim lines, whole-line comments).
//!
//! Cites: format spec §2 (lexical syntax), expr spec §7 (canonical number text, for
//! [`push_num`]), plugin-abi §6–§7 (where events and outputs use it). Lexing never panics on
//! any input; every failure is [`Code::Parse`].
//!
//! Contents:
//! - `pub fn is_word(text: &str) -> bool`, `pub fn is_bare(text: &str) -> bool`
//! - `pub fn split_word(line: &str) -> Result<(&str, &str), Code>`
//! - `pub fn values(text: &str) -> Values<'_>`; `pub struct Values`: `rest`, `need`,
//!   `need_plain`, `need_num`, `end`, `Iterator<Item = Result<Cow<str>, Code>>`
//! - `pub fn push_value(out: &mut String, value: &str)`,
//!   `pub fn push_num(out: &mut String, x: f64)`,
//!   `pub fn push_verbatim(out: &mut String, body: &str)`
//! - `pub enum BodyLine` (`Attr { word, values }`),
//!   `pub fn body_line(line: &str) -> Result<BodyLine<'_>, Code>`
//! - `pub fn lines(body: &str) -> impl Iterator<Item = &str>`
//! - `pub fn verbatim_body(body: &str) -> Result<String, Code>`
//! - `pub fn chunks(bytes: &[u8]) -> Chunks<'_>`; `pub struct Chunks`: `Iterator<Item = &[u8]>`

// ponytail: a subset of the format codec kept here so `abi` stays a dependency-free leaf for
// guests (every plugin links it). The host validates contribution sets and spawn argv with
// `schemify-format` itself (`schemify_plugin::intake`); this lexer only encodes and decodes
// events and output records. Re-export the format's once it has a public value lexer.

use std::borrow::Cow;
use std::str::{CharIndices, FromStr};

use crate::Code;

/// Iterator returned by [`values`]. Stops after the first error.
#[derive(Clone, Debug)]
pub struct Values<'text> {
    rest: &'text str,
}

/// One line after a record's first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyLine<'line> {
    /// Only whitespace.
    Blank,
    /// `# …`, ignored.
    Comment,
    /// An indented attribute.
    Attr {
        /// The attribute's word (`vary`).
        word: &'line str,
        /// The values text after the word; iterate it with [`values`].
        values: &'line str,
    },
    /// A `|` line's content.
    Verbatim(&'line str),
}

/// Iterator returned by [`chunks`]; every chunk it yields is non-empty.
#[derive(Clone, Debug)]
pub struct Chunks<'bytes> {
    rest: &'bytes [u8],
}

/// One value lexed off the front of a values text, and the text after it.
struct Lexed<'text> {
    value: Cow<'text, str>,
    after: &'text str,
}

/// Longest `\u{…}` escape, in hex digits (format spec §2).
const UNICODE_DIGITS_MAX: u32 = 6;
// `lex_unicode` accumulates the digits in a `u32` without overflow checks.
const _: () = assert!(UNICODE_DIGITS_MAX * 4 < u32::BITS);

/// Returns whether `text` is a legal word: `ALPHA *(ALPHA / DIGIT / "_" / "-" / "." / ":")`.
#[must_use]
pub fn is_word(text: &str) -> bool {
    let Some((&first, tail)) = text.as_bytes().split_first() else {
        return false;
    };
    let word = first.is_ascii_alphabetic()
        && tail
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'));
    if word {
        debug_assert!(is_bare(text), "a word is always a legal bare value");
    }
    word
}

/// Returns whether `text` can be written as a bare (unquoted) value: non-empty, no
/// whitespace, quote, backslash or control character, and not starting with `#` or `|`.
#[must_use]
pub fn is_bare(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        None => false,
        Some(first) => is_bchar(first) && first != '#' && first != '|' && chars.all(is_bchar),
    }
}

/// Splits `line` (no leading whitespace) into its leading word and the text after it, with
/// the whitespace between them dropped.
///
/// # Errors
///
/// [`Code::Parse`] if the line does not start with a word followed by whitespace or end.
pub fn split_word(line: &str) -> Result<(&str, &str), Code> {
    let end = line.find([' ', '\t']).unwrap_or(line.len());
    let (word, rest) = line.split_at(end);
    if !is_word(word) {
        return Err(Code::Parse);
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    debug_assert!(word.len() + rest.len() <= line.len());
    Ok((word, rest))
}

/// Iterates the whitespace-separated values of `text`, unescaping quoted ones.
///
/// Bare values and quoted values without escapes borrow from `text`; only a quoted value
/// with an escape allocates.
#[must_use]
pub fn values(text: &str) -> Values<'_> {
    Values { rest: text }
}

/// Appends `value` as one value: bare when legal, otherwise quoted with escapes. Control
/// characters are always escaped, so the output never contains CR, LF or NUL.
pub fn push_value(out: &mut String, value: &str) {
    use std::fmt::Write as _;

    let before = out.len();
    if is_bare(value) {
        out.push_str(value);
    } else {
        out.push('"');
        for character in value.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\u{0}'..='\u{1f}' | '\u{7f}' => {
                    write!(out, "\\u{{{:x}}}", u32::from(character)).expect("writing to a String");
                }
                character => out.push(character),
            }
        }
        out.push('"');
    }
    let written = &out[before..];
    debug_assert!(!written.contains(['\n', '\r', '\0']));
    // The pair of this encoder is `values`: what it wrote must lex back to `value`, alone.
    let mut lexed = values(written);
    debug_assert!(
        matches!(lexed.next(), Some(Ok(back)) if back == value),
        "push_value wrote {written:?}, which does not lex back to {value:?}"
    );
    debug_assert!(
        lexed.next().is_none(),
        "push_value wrote {written:?}: two values"
    );
}

/// Appends the canonical number text of `x` (expr spec §7), byte-identical to
/// `schemify_expr::write_num`: what a guest function returns so the host reads the
/// same value back. An integer when `x` is integral with |x| < 1e15 (`-0` is `0`),
/// the shortest round-trip decimal when 1e-4 ≤ |x| < 1e15, else the shortest
/// round-trip mantissa with an exponent (`2.5e-15`).
///
/// # Panics
///
/// If `x` is NaN or infinite (never an expression value).
pub fn push_num(out: &mut String, x: f64) {
    // ponytail: a copy of expr's `write_num` so guests need not link expr; the abi test
    // `push_num_matches_expr` is the differential that keeps the two identical.
    use std::fmt::Write as _;

    assert!(x.is_finite(), "canonical text of a non-finite number");
    let before = out.len();
    let magnitude = x.abs();
    let written = if x.fract() == 0.0 && magnitude < 1e15 {
        write!(out, "{}", x as i64)
    } else if (1e-4..1e15).contains(&magnitude) {
        write!(out, "{x}")
    } else {
        write!(out, "{x:e}")
    };
    written.expect("writing to a String");
    // Every branch writes a shortest round-trip form, so the text reads back as `x` itself.
    let back: Result<f64, _> = out[before..].parse();
    debug_assert_eq!(
        back,
        Ok(x),
        "{:?} does not read back as {x:?}",
        &out[before..]
    );
}

/// Appends `body` as verbatim lines (`  | text`, or `  |` for an empty line). A trailing
/// newline in `body` adds no extra line; a lone `\r` before a newline is dropped.
pub fn push_verbatim(out: &mut String, body: &str) {
    let before = out.len();
    for line in body.lines() {
        if line.is_empty() {
            out.push_str("  |\n");
        } else {
            out.push_str("  | ");
            out.push_str(line);
            out.push('\n');
        }
    }
    // The pair of this encoder is `verbatim_body`: every line written is a verbatim line,
    // one per line of `body`.
    debug_assert!(verbatim_body(&out[before..]).is_ok());
    debug_assert_eq!(out[before..].lines().count(), body.lines().count());
}

/// Classifies one body line, as [`lines`] yields it: no LF inside (debug builds assert it).
///
/// # Errors
///
/// [`Code::Parse`] for a non-blank line in column 0, a `|` not followed by a space or end,
/// or an attribute that does not start with a word.
pub fn body_line(line: &str) -> Result<BodyLine<'_>, Code> {
    debug_assert!(
        !line.contains('\n'),
        "body_line takes one line, as `lines` yields it"
    );
    let trimmed = line.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Ok(BodyLine::Blank);
    }
    if trimmed.len() == line.len() {
        return Err(Code::Parse);
    }
    if trimmed.starts_with('#') {
        return Ok(BodyLine::Comment);
    }
    if let Some(after_bar) = trimmed.strip_prefix('|') {
        return match after_bar.strip_prefix(' ') {
            Some(content) => Ok(BodyLine::Verbatim(content)),
            None if after_bar.is_empty() => Ok(BodyLine::Verbatim("")),
            None => Err(Code::Parse),
        };
    }
    let (word, values) = split_word(trimmed)?;
    Ok(BodyLine::Attr { word, values })
}

/// Iterates the non-empty lines of `body`, splitting on LF and dropping one trailing CR per
/// line.
pub fn lines(body: &str) -> impl Iterator<Item = &str> {
    body.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
}

/// Joins the verbatim lines of `body` with `\n` (blank and comment lines are skipped).
///
/// # Errors
///
/// [`Code::Parse`] if any line is an attribute or malformed.
pub fn verbatim_body(body: &str) -> Result<String, Code> {
    let mut out = String::with_capacity(body.len());
    let mut first = true;
    for line in lines(body) {
        match body_line(line)? {
            BodyLine::Blank | BodyLine::Comment => {}
            BodyLine::Attr { .. } => return Err(Code::Parse),
            BodyLine::Verbatim(content) => {
                if !first {
                    out.push('\n');
                }
                first = false;
                out.push_str(content);
            }
        }
    }
    debug_assert!(out.len() <= body.len());
    Ok(out)
}

/// Splits a byte stream into top-level records: each chunk is a column-0 line plus the
/// indented or blank lines after it, without the final line terminator. Column-0 comment
/// and blank lines between records are skipped. A chunk starting with whitespace is an
/// orphan attribute and fails to parse downstream.
///
/// Works on bytes so one invalid UTF-8 record cannot poison its neighbours.
#[must_use]
pub fn chunks(bytes: &[u8]) -> Chunks<'_> {
    Chunks { rest: bytes }
}

impl<'text> Values<'text> {
    /// Returns the unconsumed text, leading whitespace removed.
    #[must_use]
    pub fn rest(&self) -> &'text str {
        self.rest.trim_start_matches([' ', '\t'])
    }

    /// Returns the next value.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left or the next one is malformed.
    pub fn need(&mut self) -> Result<Cow<'text, str>, Code> {
        self.next().unwrap_or(Err(Code::Parse))
    }

    /// Returns the next value, which must borrow from the input (no escapes): identifiers
    /// such as command names, dialects and bench names.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left, it is malformed, or it needed unescaping.
    pub fn need_plain(&mut self) -> Result<&'text str, Code> {
        match self.need()? {
            Cow::Borrowed(s) => Ok(s),
            Cow::Owned(_) => Err(Code::Parse),
        }
    }

    /// Returns the next value parsed as a number (`u32`, `i32`, `f64`, …) with `FromStr`.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left or it is not a number of that type.
    pub fn need_num<T: FromStr>(&mut self) -> Result<T, Code> {
        self.need()?.parse().map_err(|_| Code::Parse)
    }

    /// Succeeds only when every value has been consumed (arity check).
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when any value is left.
    pub fn end(&mut self) -> Result<(), Code> {
        match self.next() {
            None => {
                debug_assert!(self.rest.is_empty(), "`next` ends only on an empty rest");
                Ok(())
            }
            Some(_) => Err(Code::Parse),
        }
    }
}

impl<'text> Iterator for Values<'text> {
    type Item = Result<Cow<'text, str>, Code>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.rest();
        self.rest = rest;
        if rest.is_empty() {
            return None;
        }
        let lexed = lex_value(rest).and_then(|lexed| {
            if lexed.after.is_empty() || lexed.after.starts_with([' ', '\t']) {
                Ok(lexed)
            } else {
                Err(Code::Parse)
            }
        });
        match lexed {
            Ok(Lexed { value, after }) => {
                debug_assert!(after.len() < rest.len(), "a value consumes input");
                self.rest = after;
                Some(Ok(value))
            }
            Err(code) => {
                self.rest = "";
                Some(Err(code))
            }
        }
    }
}

impl<'bytes> Iterator for Chunks<'bytes> {
    type Item = &'bytes [u8];

    fn next(&mut self) -> Option<&'bytes [u8]> {
        let all = self.rest;
        // Skip column-0 blank and comment lines; each pass consumes one line.
        let (line, after) = loop {
            if self.rest.is_empty() {
                return None;
            }
            let (line, after) = split_line(self.rest);
            if is_blank(line) || line[0] == b'#' {
                self.rest = after;
                continue;
            }
            break (line, after);
        };
        let start = all.len() - self.rest.len();
        let mut end = start + line.len();
        self.rest = after;
        // Absorb indented and blank lines; the chunk ends at the last non-blank one.
        while !self.rest.is_empty() {
            let (next, after) = split_line(self.rest);
            if !(is_blank(next) || matches!(next[0], b' ' | b'\t')) {
                break;
            }
            if !is_blank(next) {
                end = all.len() - self.rest.len() + next.len();
            }
            self.rest = after;
        }
        debug_assert!(start < end, "a chunk is never empty");
        debug_assert!(end <= all.len());
        debug_assert!(self.rest.len() < all.len(), "each call consumes input");
        let chunk = &all[start..end];
        debug_assert_ne!(
            chunk[0], b'#',
            "column-0 comments are skipped, never chunked"
        );
        debug_assert_ne!(
            chunk[chunk.len() - 1],
            b'\n',
            "a chunk has no final terminator"
        );
        Some(chunk)
    }
}

// ---- private ----

/// Whether `c` may appear in a bare value (format spec §2 `bchar`).
const fn is_bchar(character: char) -> bool {
    !matches!(
        character,
        ' ' | '\t' | '\r' | '\n' | '"' | '\\' | '\u{0}'..='\u{1f}' | '\u{7f}'
    )
}

/// Lexes one value at the start of `text` (non-empty, no leading whitespace).
fn lex_value(text: &str) -> Result<Lexed<'_>, Code> {
    debug_assert!(!text.is_empty());
    debug_assert!(
        !text.starts_with([' ', '\t']),
        "`Values::rest` strips leading whitespace"
    );
    if let Some(quoted) = text.strip_prefix('"') {
        return lex_quoted(quoted);
    }
    let end = text
        .find(|character: char| !is_bchar(character))
        .unwrap_or(text.len());
    let value = &text[..end];
    if value.is_empty() || value.starts_with(['#', '|']) {
        return Err(Code::Parse);
    }
    Ok(Lexed {
        value: Cow::Borrowed(value),
        after: &text[end..],
    })
}

/// Lexes the body of a quoted value; `quoted` starts just after the opening quote.
fn lex_quoted(quoted: &str) -> Result<Lexed<'_>, Code> {
    let mut owned: Option<String> = None;
    let mut chars = quoted.char_indices();
    let mut run_start = 0;
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                let value = match owned {
                    None => Cow::Borrowed(&quoted[..i]),
                    Some(mut unescaped) => {
                        unescaped.push_str(&quoted[run_start..i]);
                        Cow::Owned(unescaped)
                    }
                };
                return Ok(Lexed {
                    value,
                    after: &quoted[i + 1..],
                });
            }
            '\r' | '\n' => return Err(Code::Parse),
            '\\' => {
                debug_assert!(
                    run_start <= i,
                    "a run starts before the escape that ends it"
                );
                let unescaped_so_far = owned.get_or_insert_with(String::new);
                unescaped_so_far.push_str(&quoted[run_start..i]);
                let unescaped = match chars.next().map(|(_, escape)| escape) {
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('u') => lex_unicode(&mut chars)?,
                    _ => return Err(Code::Parse),
                };
                unescaped_so_far.push(unescaped);
                run_start = chars.clone().next().map_or(quoted.len(), |(next, _)| next);
                debug_assert!(quoted.is_char_boundary(run_start));
            }
            _ => {}
        }
    }
    Err(Code::Parse)
}

/// Lexes `{1*6HEXDIG}` after `\u`.
fn lex_unicode(chars: &mut CharIndices<'_>) -> Result<char, Code> {
    if chars.next().map(|(_, c)| c) != Some('{') {
        return Err(Code::Parse);
    }
    let mut value = 0u32;
    for digits in 0..=UNICODE_DIGITS_MAX {
        match chars.next().map(|(_, c)| c) {
            Some('}') if digits > 0 => return char::from_u32(value).ok_or(Code::Parse),
            Some(digit) if digits < UNICODE_DIGITS_MAX => {
                value = value * 16 + digit.to_digit(16).ok_or(Code::Parse)?;
            }
            _ => return Err(Code::Parse),
        }
    }
    Err(Code::Parse)
}

/// Splits `bytes` at its first LF: the line without it, and the bytes after it.
fn split_line(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&byte| byte == b'\n') {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[]),
    }
}

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|&byte| matches!(byte, b' ' | b'\t' | b'\r'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::arbitrary::any;
    use proptest::{prop_assert, prop_assert_eq, prop_assume, proptest};

    fn collect(text: &str) -> Result<Vec<String>, Code> {
        values(text)
            .map(|value| value.map(Cow::into_owned))
            .collect()
    }

    #[test]
    fn values_lex_bare_quoted_and_escapes() {
        assert_eq!(
            collect("a b:c  \"d e\"\t-1.5").unwrap(),
            ["a", "b:c", "d e", "-1.5"]
        );
        assert_eq!(collect(r#""x\"y\\z\n\t\u{41}""#).unwrap(), ["x\"y\\z\n\tA"]);
        assert_eq!(collect("").unwrap(), Vec::<String>::new());
        assert_eq!(collect("   ").unwrap(), Vec::<String>::new());
        assert!(matches!(
            values("\"plain\"").next(),
            Some(Ok(Cow::Borrowed("plain")))
        ));
        for bad in [
            "\"open",
            "a\"b",
            "#x",
            "|x",
            r#""\q""#,
            r#""\u{}""#,
            r#""\u{1234567}""#,
        ] {
            assert_eq!(collect(bad), Err(Code::Parse), "{bad}");
        }
        assert_eq!(collect(r#""\u{d800}""#), Err(Code::Parse));
    }

    #[test]
    fn body_lines_classify() {
        assert_eq!(body_line("  | x y"), Ok(BodyLine::Verbatim("x y")));
        assert_eq!(body_line("  |"), Ok(BodyLine::Verbatim("")));
        assert_eq!(body_line("  |x"), Err(Code::Parse));
        assert_eq!(
            body_line("  vary a 1"),
            Ok(BodyLine::Attr {
                word: "vary",
                values: "a 1"
            })
        );
        assert_eq!(body_line("  # c"), Ok(BodyLine::Comment));
        assert_eq!(body_line("   "), Ok(BodyLine::Blank));
        assert_eq!(body_line("top"), Err(Code::Parse));
        assert_eq!(
            verbatim_body("  | a\n  |\n  | b\n"),
            Ok("a\n\nb".to_owned())
        );
        assert_eq!(verbatim_body("  attr 1"), Err(Code::Parse));
    }

    #[test]
    fn chunks_split_records() {
        let text = b"# c\nlog info a\n\ncontribute ui\n  | node x item\n\n  | y\nrun a b c\n  vary v 1\n  orphan\ntimer 1 2";
        let got: Vec<&[u8]> = chunks(text).collect();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], b"log info a");
        assert_eq!(got[1], b"contribute ui\n  | node x item\n\n  | y");
        assert_eq!(got[2], b"run a b c\n  vary v 1\n  orphan");
        assert_eq!(got[3], b"timer 1 2");
        assert_eq!(
            chunks(b"  orphan\nlog info x").next(),
            Some(&b"  orphan"[..])
        );
        assert_eq!(chunks(b"\n\n# only\n").next(), None);
    }

    fn num(x: f64) -> (String, String) {
        let (mut ours, mut theirs) = (String::new(), String::new());
        push_num(&mut ours, x);
        schemify_expr::write_num(&mut theirs, x);
        (ours, theirs)
    }

    /// Differential against expr's formatter at the special values and every branch
    /// boundary (integral cutoff 1e15, decimal window 1e-4, SI decades), each nudged one
    /// ulp either way and negated.
    #[test]
    fn push_num_matches_expr() {
        let mut xs = vec![
            0.0,
            -0.0,
            f64::MIN_POSITIVE,
            5e-324,
            f64::MAX,
            f64::EPSILON,
            0.1 + 0.2,
            1.0 / 3.0,
            2.5e-15,
            1.2e15,
            999_999_999_999_999.0,
            999_999_999_999_999.5,
            (1u64 << 53) as f64,
        ];
        for exponent in -24..=24 {
            let decade = 10f64.powi(exponent);
            for x in [decade, 1.5 * decade, 4.7 * decade, 999.0 * decade] {
                xs.extend([
                    x,
                    f64::from_bits(x.to_bits() + 1),
                    f64::from_bits(x.to_bits() - 1),
                ]);
            }
        }
        for x in xs {
            for x in [x, -x] {
                let (ours, theirs) = num(x);
                assert_eq!(ours, theirs, "{x:?}");
            }
        }
        assert_eq!(num(-0.0).0, "0");
        assert_eq!(num(1e-4).0, "0.0001");
        assert_eq!(num(1e15).0, "1e15");
        assert_eq!(num(2.5e-15).0, "2.5e-15");
    }

    proptest! {
        #[test]
        fn push_num_matches_expr_on_any_finite(bits in any::<u64>()) {
            let x = f64::from_bits(bits);
            prop_assume!(x.is_finite());
            let (ours, theirs) = num(x);
            prop_assert_eq!(ours, theirs);
        }

        #[test]
        fn push_value_round_trips(value in "\\PC{0,24}|[\\x00-\\x7f]{0,24}") {
            let mut out = String::new();
            push_value(&mut out, &value);
            let got = collect(&out).unwrap();
            prop_assert_eq!(got, vec![value]);
        }

        #[test]
        fn verbatim_round_trips(lines in proptest::collection::vec("[^\r\n]{0,16}", 1..6)) {
            // One trailing newline is a terminator, not a line (`str::lines` semantics).
            let body = lines.join("\n");
            let mut out = String::new();
            push_verbatim(&mut out, &body);
            prop_assert_eq!(verbatim_body(&out).unwrap(), body.strip_suffix('\n').unwrap_or(&body));
        }

        #[test]
        fn lexers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            for chunk in chunks(&bytes) {
                prop_assert!(!chunk.is_empty());
                if let Ok(text) = std::str::from_utf8(chunk) {
                    let _ = values(text).count();
                    for line in lines(text) {
                        let _ = body_line(line);
                    }
                    let _ = verbatim_body(text);
                }
            }
        }
    }
}
