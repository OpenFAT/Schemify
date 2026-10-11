//! The subset of the line-record syntax (format spec §2) that events and outputs use:
//! words, bare and quoted values, indented attribute lines, `|` verbatim lines and
//! whole-line comments.
//!
//! Lexing never panics on any input; every failure is [`Code::Parse`].

// ponytail: a subset of the format codec kept here so `abi` stays a dependency-free leaf for
// guests (every plugin links it). The host validates contribution sets and spawn argv with
// `schemify-format` itself (`schemify_plugin::intake`); this lexer only encodes and decodes
// events and output records. Re-export the format's once it has a public value lexer.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::Code;

const fn is_bchar(c: char) -> bool {
    !matches!(
        c,
        ' ' | '\t' | '\r' | '\n' | '"' | '\\' | '\u{0}'..='\u{1f}' | '\u{7f}'
    )
}

/// Returns whether `s` is a legal word: `ALPHA *(ALPHA / DIGIT / "_" / "-" / "." / ":")`.
pub fn is_word(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphabetic()
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}

/// Returns whether `s` can be written as a bare (unquoted) value.
pub fn is_bare(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        None => false,
        Some(c) => is_bchar(c) && c != '#' && c != '|' && cs.all(is_bchar),
    }
}

/// Splits `line` (no leading whitespace) into its leading word and the text after it.
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
    Ok((word, rest.trim_start_matches([' ', '\t'])))
}

/// Iterates the whitespace-separated values of `s`, unescaping quoted ones.
///
/// Bare values and quoted values without escapes borrow from `s`.
pub fn values(s: &str) -> Values<'_> {
    Values { rest: s }
}

/// Iterator returned by [`values`]. Stops after the first error.
#[derive(Clone, Debug)]
pub struct Values<'a> {
    rest: &'a str,
}

impl<'a> Values<'a> {
    /// Returns the unconsumed text, leading whitespace removed.
    pub fn rest(&self) -> &'a str {
        self.rest.trim_start_matches([' ', '\t'])
    }

    /// Returns the next value.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left or the next one is malformed.
    pub fn need(&mut self) -> Result<Cow<'a, str>, Code> {
        self.next().unwrap_or(Err(Code::Parse))
    }

    /// Returns the next value, which must borrow from the input (no escapes): identifiers
    /// such as command names, dialects and bench names.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left, it is malformed, or it needed unescaping.
    pub fn need_plain(&mut self) -> Result<&'a str, Code> {
        match self.need()? {
            Cow::Borrowed(s) => Ok(s),
            Cow::Owned(_) => Err(Code::Parse),
        }
    }

    /// Returns the next value parsed as a number (`u32`, `i32`, …).
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when no value is left or it is not a number of that type.
    pub fn need_num<T: std::str::FromStr>(&mut self) -> Result<T, Code> {
        self.need()?.parse().map_err(|_| Code::Parse)
    }

    /// Succeeds only when every value has been consumed (arity check).
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] when any value is left.
    pub fn end(&mut self) -> Result<(), Code> {
        match self.next() {
            None => Ok(()),
            Some(_) => Err(Code::Parse),
        }
    }
}

impl<'a> Iterator for Values<'a> {
    type Item = Result<Cow<'a, str>, Code>;

    fn next(&mut self) -> Option<Self::Item> {
        let s = self.rest();
        if s.is_empty() {
            self.rest = s;
            return None;
        }
        let lexed = lex_value(s).and_then(|(v, after)| {
            if after.is_empty() || after.starts_with([' ', '\t']) {
                Ok((v, after))
            } else {
                Err(Code::Parse)
            }
        });
        match lexed {
            Ok((v, after)) => {
                debug_assert!(after.len() < s.len());
                self.rest = after;
                Some(Ok(v))
            }
            Err(code) => {
                self.rest = "";
                Some(Err(code))
            }
        }
    }
}

fn lex_value(s: &str) -> Result<(Cow<'_, str>, &str), Code> {
    debug_assert!(!s.is_empty());
    if let Some(quoted) = s.strip_prefix('"') {
        return lex_quoted(quoted);
    }
    let end = s.find(|c: char| !is_bchar(c)).unwrap_or(s.len());
    let v = &s[..end];
    if v.is_empty() || v.starts_with(['#', '|']) {
        return Err(Code::Parse);
    }
    Ok((Cow::Borrowed(v), &s[end..]))
}

/// Lexes the body of a quoted value; `s` starts just after the opening quote.
fn lex_quoted(s: &str) -> Result<(Cow<'_, str>, &str), Code> {
    let mut owned: Option<String> = None;
    let mut chars = s.char_indices();
    let mut run_start = 0;
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                let value = match owned {
                    None => Cow::Borrowed(&s[..i]),
                    Some(mut o) => {
                        o.push_str(&s[run_start..i]);
                        Cow::Owned(o)
                    }
                };
                return Ok((value, &s[i + 1..]));
            }
            '\r' | '\n' => return Err(Code::Parse),
            '\\' => {
                let o = owned.get_or_insert_with(String::new);
                o.push_str(&s[run_start..i]);
                let unescaped = match chars.next().map(|(_, e)| e) {
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('u') => lex_unicode(&mut chars)?,
                    _ => return Err(Code::Parse),
                };
                o.push(unescaped);
                run_start = chars.clone().next().map_or(s.len(), |(j, _)| j);
            }
            _ => {}
        }
    }
    Err(Code::Parse)
}

/// Lexes `{1*6HEXDIG}` after `\u`.
fn lex_unicode(chars: &mut std::str::CharIndices<'_>) -> Result<char, Code> {
    if chars.next().map(|(_, c)| c) != Some('{') {
        return Err(Code::Parse);
    }
    let mut value = 0u32;
    for digits in 0..=6 {
        match chars.next().map(|(_, c)| c) {
            Some('}') if digits > 0 => return char::from_u32(value).ok_or(Code::Parse),
            Some(c) if digits < 6 => value = value * 16 + c.to_digit(16).ok_or(Code::Parse)?,
            _ => return Err(Code::Parse),
        }
    }
    Err(Code::Parse)
}

/// Appends `v` as one value: bare when legal, otherwise quoted with escapes. Control
/// characters are always escaped, so the output never contains CR, LF or NUL.
pub fn push_value(out: &mut String, v: &str) {
    let before = out.len();
    if is_bare(v) {
        out.push_str(v);
        return;
    }
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{0}'..='\u{1f}' | '\u{7f}' => {
                write!(out, "\\u{{{:x}}}", c as u32).expect("writing to a String");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    debug_assert!(!out[before..].contains(['\n', '\r', '\0']));
}

/// Appends `body` as verbatim lines (`  | text`, or `  |` for an empty line). A trailing
/// newline in `body` adds no extra line; a lone `\r` before a newline is dropped.
pub fn push_verbatim(out: &mut String, body: &str) {
    for line in body.lines() {
        if line.is_empty() {
            out.push_str("  |\n");
        } else {
            out.push_str("  | ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// One line after a record's first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyLine<'a> {
    /// Only whitespace.
    Blank,
    /// `# …`, ignored.
    Comment,
    /// An indented attribute: its word and the values after it.
    Attr(&'a str, &'a str),
    /// A `|` line's content.
    Verbatim(&'a str),
}

/// Classifies one body line (no line terminator).
///
/// # Errors
///
/// [`Code::Parse`] for a non-blank line in column 0, a `|` not followed by a space or end,
/// or an attribute that does not start with a word.
pub fn body_line(line: &str) -> Result<BodyLine<'_>, Code> {
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
    if let Some(v) = trimmed.strip_prefix('|') {
        return match v.strip_prefix(' ') {
            Some(content) => Ok(BodyLine::Verbatim(content)),
            None if v.is_empty() => Ok(BodyLine::Verbatim("")),
            None => Err(Code::Parse),
        };
    }
    let (word, rest) = split_word(trimmed)?;
    Ok(BodyLine::Attr(word, rest))
}

/// Iterates the lines of `body`, splitting on LF and dropping one trailing CR per line.
pub fn lines(body: &str) -> impl Iterator<Item = &str> {
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
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
            BodyLine::Attr(..) => return Err(Code::Parse),
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
pub fn chunks(bytes: &[u8]) -> Chunks<'_> {
    Chunks { rest: bytes }
}

/// Iterator returned by [`chunks`].
#[derive(Clone, Debug)]
pub struct Chunks<'a> {
    rest: &'a [u8],
}

fn split_line(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|&c| c == b'\n') {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[]),
    }
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(|&c| matches!(c, b' ' | b'\t' | b'\r'))
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let all = self.rest;
        loop {
            if self.rest.is_empty() {
                return None;
            }
            let (line, after) = split_line(self.rest);
            if is_blank(line) || line[0] == b'#' {
                self.rest = after;
                continue;
            }
            let start = all.len() - self.rest.len();
            let mut end = start + line.len();
            self.rest = after;
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
            debug_assert!(start < end && end <= all.len());
            return Some(&all[start..end]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn collect(s: &str) -> Result<Vec<String>, Code> {
        values(s).map(|v| v.map(Cow::into_owned)).collect()
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
        assert_eq!(body_line("  vary a 1"), Ok(BodyLine::Attr("vary", "a 1")));
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

    proptest! {
        #[test]
        fn push_value_round_trips(v in "\\PC{0,24}|[\\x00-\\x7f]{0,24}") {
            let mut out = String::new();
            push_value(&mut out, &v);
            let got = collect(&out).unwrap();
            prop_assert_eq!(got, vec![v]);
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
                if let Ok(s) = std::str::from_utf8(chunk) {
                    let _ = values(s).count();
                    for line in lines(s) {
                        let _ = body_line(line);
                    }
                    let _ = verbatim_body(s);
                }
            }
        }
    }
}
