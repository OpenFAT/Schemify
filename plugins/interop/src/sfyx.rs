//! The `.sfyx` exchange file (spec `interop.md` §4): one SKILL list per line, built only of
//! strings, integers and lists, so Virtuoso's `lineread` parses it without evaluating
//! anything and without SKILL's scale suffixes, octal zeros or 7-digit floats touching it.
//!
//! This module is the codec only: [`read`] text into [`Record`]s and [`write()`] them back,
//! byte-identically to the SKILL dumper. What the records mean is `virtuoso`'s business.

use std::fmt::Write as _;

/// One element of a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// A string (any bytes of valid UTF-8).
    Str(String),
    /// A 32-bit integer (SKILL fixnums are 32-bit in the documented releases).
    Int(i32),
    /// A list of elements, possibly empty.
    List(Vec<Value>),
}

impl Value {
    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The list items; empty for a non-list.
    pub fn items(&self) -> &[Value] {
        match self {
            Value::List(v) => v,
            _ => &[],
        }
    }

    /// A *text* field (spec §4.1): a string, or a list of string chunks concatenated.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Str(s) => Some(s.clone()),
            Value::List(v) => {
                let mut out = String::new();
                for x in v {
                    out.push_str(x.as_str()?);
                }
                Some(out)
            }
            Value::Int(_) => None,
        }
    }
}

/// A record: its tag and fields (the line `("tag" f1 f2 …)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The first element.
    pub tag: String,
    /// Every element after the tag.
    pub fields: Vec<Value>,
    /// 1-based source line (0 for records built in memory).
    pub line: u32,
}

impl Record {
    /// A record built in memory.
    pub fn new(tag: &str, fields: Vec<Value>) -> Record {
        Record {
            tag: tag.to_owned(),
            fields,
            line: 0,
        }
    }

    /// Field `i` as a string, or an error naming the record.
    ///
    /// # Errors
    ///
    /// The field is missing or not a string.
    pub fn str(&self, i: usize) -> Result<&str, Error> {
        self.fields
            .get(i)
            .and_then(Value::as_str)
            .ok_or_else(|| self.bad(i, "a string"))
    }

    /// Field `i` as an integer.
    ///
    /// # Errors
    ///
    /// The field is missing or not an integer.
    pub fn int(&self, i: usize) -> Result<i32, Error> {
        self.fields
            .get(i)
            .and_then(Value::as_int)
            .ok_or_else(|| self.bad(i, "an integer"))
    }

    /// Field `i` as a list.
    ///
    /// # Errors
    ///
    /// The field is missing or not a list.
    pub fn list(&self, i: usize) -> Result<&[Value], Error> {
        match self.fields.get(i) {
            Some(Value::List(v)) => Ok(v),
            _ => Err(self.bad(i, "a list")),
        }
    }

    /// Field `i` as a *text* (string or chunk list).
    ///
    /// # Errors
    ///
    /// The field is missing or not text.
    pub fn text(&self, i: usize) -> Result<String, Error> {
        self.fields
            .get(i)
            .and_then(Value::as_text)
            .ok_or_else(|| self.bad(i, "text"))
    }

    /// An error on this record's line, its message prefixed with the tag (`"inst: …"`): the
    /// one shape every reader of records reports in.
    pub fn err(&self, msg: impl Into<String>) -> Error {
        Error {
            line: self.line,
            msg: format!("{}: {}", self.tag, msg.into()),
        }
    }

    /// Checks that the record has exactly `n` fields.
    ///
    /// # Errors
    ///
    /// Any other field count, as `"<tag>: <k> fields, expected <n>"`.
    pub fn arity(&self, n: usize) -> Result<(), Error> {
        if self.fields.len() == n {
            Ok(())
        } else {
            Err(self.err(format!("{} fields, expected {n}", self.fields.len())))
        }
    }

    fn bad(&self, i: usize, what: &str) -> Error {
        self.err(format!("field {} is not {what}", i + 1))
    }
}

/// A malformed line, with its 1-based line number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// 1-based line (0 when not from text).
    pub line: u32,
    /// What is wrong.
    pub msg: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

/// Longest string element a writer emits whole; longer *text* is chunked. SKILL's parser
/// refuses string literals over 8191 characters and an escape takes up to 4 characters per
/// byte, so 2000 bytes always fit.
pub const CHUNK_MAX: usize = 2000;

/// Records per file, a bound on the reader's work.
pub const RECORDS_MAX: usize = 1 << 24;

/// List nesting depth the reader accepts.
const DEPTH_MAX: u32 = 32;

/// Parses a whole `.sfyx` text. Empty lines and lines starting with `;` are skipped.
///
/// # Errors
///
/// The first malformed line: not a list starting with a string tag, a bare word, a float,
/// a zero-padded or out-of-range integer, a bad escape, or more than [`RECORDS_MAX`] records.
/// A missing `("end")` is not an error here; [`require_complete`] checks it.
pub fn read(text: &str) -> Result<Vec<Record>, Error> {
    let mut out = Vec::new();
    for (k, raw) in text.split('\n').enumerate() {
        let line = u32::try_from(k + 1).unwrap_or(u32::MAX);
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        let t = l.trim_start();
        if t.is_empty() || t.starts_with(';') {
            continue;
        }
        if out.len() >= RECORDS_MAX {
            return Err(Error {
                line,
                msg: format!("more than {RECORDS_MAX} records"),
            });
        }
        let mut p = Parser {
            s: t.as_bytes(),
            i: 0,
            line,
        };
        let v = p.value(0)?;
        p.skip_ws();
        if p.i != p.s.len() {
            return Err(p.err("text after the record's closing parenthesis"));
        }
        let Value::List(mut items) = v else {
            return Err(p.err("a record is a parenthesised list"));
        };
        if items.is_empty() {
            return Err(p.err("empty record"));
        }
        let Value::Str(tag) = items.remove(0) else {
            return Err(p.err("a record starts with a string tag"));
        };
        out.push(Record {
            tag,
            fields: items,
            line,
        });
    }
    Ok(out)
}

/// Checks the header and trailer: the first record is `("sfyx" 1 producer)` and the last
/// `("end")`.
///
/// # Errors
///
/// A missing or unknown-version header, or a missing `("end")` (a truncated file).
pub fn require_complete(records: &[Record]) -> Result<(), Error> {
    let Some(first) = records.first() else {
        return Err(Error {
            line: 0,
            msg: "empty file".into(),
        });
    };
    if first.tag != "sfyx" || first.int(0)? != 1 {
        return Err(Error {
            line: first.line,
            msg: "not an .sfyx v1 file (header `(\"sfyx\" 1 …)`)".into(),
        });
    }
    match records.last() {
        Some(r) if r.tag == "end" && r.fields.is_empty() => Ok(()),
        Some(r) => Err(Error {
            line: r.line,
            msg: "missing (\"end\"): the file is truncated".into(),
        }),
        None => unreachable!(),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    line: u32,
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> Error {
        Error {
            line: self.line,
            msg: format!("column {}: {msg}", self.i + 1),
        }
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t') {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, Error> {
        if depth > DEPTH_MAX {
            return Err(self.err("lists nested too deeply"));
        }
        self.skip_ws();
        match self.s.get(self.i) {
            Some(b'(') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.s.get(self.i) {
                        Some(b')') => {
                            self.i += 1;
                            return Ok(Value::List(items));
                        }
                        None => return Err(self.err("unclosed (")),
                        _ => items.push(self.value(depth + 1)?),
                    }
                }
            }
            Some(b'"') => self.string(),
            Some(b'-' | b'0'..=b'9') => self.int(),
            Some(c) => Err(self.err(&format!(
                "`{}`: only strings, integers and lists are allowed",
                *c as char
            ))),
            None => Err(self.err("missing value")),
        }
    }

    fn string(&mut self) -> Result<Value, Error> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err(self.err("unterminated string"));
            };
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return Err(self.err("unterminated string"));
                    };
                    self.i += 1;
                    match e {
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'0'..=b'3' => {
                            let d = self
                                .s
                                .get(self.i..self.i + 2)
                                .ok_or_else(|| self.err("short octal escape"))?;
                            if !d.iter().all(|c| (b'0'..=b'7').contains(c)) {
                                return Err(self.err("octal escape needs three digits"));
                            }
                            self.i += 2;
                            out.push((e - b'0') * 64 + (d[0] - b'0') * 8 + (d[1] - b'0'));
                        }
                        _ => return Err(self.err(&format!("unknown escape \\{}", e as char))),
                    }
                }
                _ => out.push(b),
            }
        }
        String::from_utf8(out)
            .map(Value::Str)
            .map_err(|_| self.err("string is not UTF-8"))
    }

    fn int(&mut self) -> Result<Value, Error> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        let n = self.i - digits;
        if n == 0 {
            return Err(self.err("`-` without digits"));
        }
        if n > 1 && self.s[digits] == b'0' {
            return Err(self.err("leading zero (SKILL reads it as octal)"));
        }
        if let Some(&c) = self.s.get(self.i)
            && !matches!(c, b' ' | b'\t' | b')')
        {
            return Err(self.err("a number is followed by a separator (no floats, no suffixes)"));
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ascii");
        if text == "-0" {
            return Err(self.err("`-0`"));
        }
        text.parse::<i32>()
            .map(Value::Int)
            .map_err(|_| self.err("integer outside the 32-bit range"))
    }
}

/// Writes records as `.sfyx` text, one line each, LF-terminated.
pub fn write(records: &[Record]) -> String {
    let mut out = String::new();
    for r in records {
        write_record(r, &mut out);
    }
    out
}

/// Appends one record line.
pub fn write_record(r: &Record, out: &mut String) {
    out.push('(');
    write_str(&r.tag, out);
    for f in &r.fields {
        out.push(' ');
        write_value(f, out);
    }
    out.push_str(")\n");
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Str(s) => write_str(s, out),
        Value::Int(i) => write!(out, "{i}").expect("string write"),
        Value::List(items) => {
            out.push('(');
            for (k, x) in items.iter().enumerate() {
                if k > 0 {
                    out.push(' ');
                }
                write_value(x, out);
            }
            out.push(')');
        }
    }
}

/// Writes a quoted string with the §4.1 escapes.
pub fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // ponytail: a SKILL string cannot hold NUL (§4.1); it is dropped, not escaped.
            '\0' => {}
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                write!(out, "\\{:03o}", c as u32).expect("string write");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A *text* value: one string, or chunks of at most [`CHUNK_MAX`] bytes cut on char
/// boundaries when longer.
pub fn text(s: &str) -> Value {
    if s.len() <= CHUNK_MAX {
        return Value::Str(s.to_owned());
    }
    Value::List(chunks(s, CHUNK_MAX).into_iter().map(self::s).collect())
}

/// Splits `text` into pieces of at most `max` bytes, cut on char boundaries; no piece is
/// empty, so `""` gives none. Every chunked string of the exchange (text fields, the stash
/// properties) is cut by this.
///
/// # Panics
///
/// When `max` < 4: a piece must hold any char.
pub fn chunks(text: &str, max: usize) -> Vec<&str> {
    assert!(max >= 4, "chunks must hold any char");
    let mut out = Vec::with_capacity(text.len().div_ceil(max));
    let mut rest = text;
    while !rest.is_empty() {
        let mut cut = rest.len().min(max);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        out.push(head);
        rest = tail;
    }
    debug_assert_eq!(out.iter().map(|c| c.len()).sum::<usize>(), text.len());
    out
}

/// Shorthand: a string value.
pub fn s(text: &str) -> Value {
    Value::Str(text.to_owned())
}

/// Shorthand: an integer value.
pub fn i(v: i32) -> Value {
    Value::Int(v)
}

/// Shorthand: a list of `(key value)` string pairs.
pub fn pairs<'a>(kv: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    Value::List(
        kv.into_iter()
            .map(|(k, v)| Value::List(vec![s(k), s(v)]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// `chunks` is the greedy cut: the pieces concatenate to the input, none is empty or
        /// longer than `max`, and each but the last is as long as a char boundary allows
        /// (the next char would not fit). Multi-byte chars sit on every boundary somewhere.
        #[test]
        fn chunks_are_greedy_char_boundary_cuts(t in "[a\u{e9}\u{20ac}\u{1d11e}]{0,40}", max in 4usize..12) {
            let pieces = chunks(&t, max);
            prop_assert_eq!(pieces.concat(), t.clone());
            for (k, p) in pieces.iter().enumerate() {
                prop_assert!(!p.is_empty() && p.len() <= max);
                if let Some(next) = pieces.get(k + 1) {
                    let c = next.chars().next().unwrap();
                    prop_assert!(p.len() + c.len_utf8() > max, "{:?} could take {:?}", p, c);
                }
            }
            // A text field reads back whole, chunked or not.
            prop_assert_eq!(text(&t).as_text(), Some(t.clone()));
        }
    }

    #[test]
    fn chunks_of_nothing_and_exact_fits() {
        assert!(chunks("", 4).is_empty());
        assert_eq!(chunks("abcd", 4), ["abcd"]);
        assert_eq!(chunks("abcde", 4), ["abcd", "e"]);
        // A 4-byte char never splits, and a 3-byte one waits for the next piece.
        assert_eq!(chunks("a\u{1d11e}", 4), ["a", "\u{1d11e}"]);
        assert_eq!(chunks("ab\u{20ac}", 4), ["ab", "\u{20ac}"]);
        assert_eq!(
            text(&"x".repeat(CHUNK_MAX)),
            Value::Str("x".repeat(CHUNK_MAX))
        );
        assert!(matches!(text(&"x".repeat(CHUNK_MAX + 1)), Value::List(v) if v.len() == 2));
    }

    #[test]
    #[should_panic(expected = "chunks must hold any char")]
    fn chunks_refuse_a_max_below_one_char() {
        chunks("abc", 3);
    }

    #[test]
    fn record_errors_name_the_tag_and_line() {
        let r = Record {
            tag: "inst".into(),
            fields: vec![i(1), s("x")],
            line: 7,
        };
        assert_eq!(r.arity(2), Ok(()));
        let e = r.arity(3).unwrap_err();
        assert_eq!((e.line, e.msg.as_str()), (7, "inst: 2 fields, expected 3"));
        assert_eq!(r.err("bad").to_string(), "line 7: inst: bad");
        assert_eq!(r.int(1).unwrap_err().msg, "inst: field 2 is not an integer");
        assert_eq!(r.str(5).unwrap_err().msg, "inst: field 6 is not a string");
    }

    #[test]
    fn round_trips_escapes_and_chunks() {
        let long = "x".repeat(CHUNK_MAX * 2 + 7);
        let recs = vec![
            Record::new("sfyx", vec![i(1), s("schemify")]),
            Record::new(
                "label",
                vec![
                    i(3),
                    s("a\"b\\c\nd\te\r\u{1}\u{7f}é"),
                    i(-160000),
                    Value::List(vec![]),
                ],
            ),
            Record::new("textview", vec![s("veriloga"), text(&long)]),
            Record::new("end", vec![]),
        ];
        let t = write(&recs);
        let back = read(&t).unwrap();
        require_complete(&back).unwrap();
        assert_eq!(back.len(), recs.len());
        for (a, b) in recs.iter().zip(&back) {
            assert_eq!(a.tag, b.tag);
            assert_eq!(a.fields, b.fields);
        }
        assert_eq!(back[2].text(1).unwrap(), long);
        assert_eq!(write(&back), t, "canonical");
        assert!(t.contains("\\001") && t.contains("\\177"), "{t}");
    }

    #[test]
    fn refuses_what_skill_would_misread() {
        for bad in [
            "(\"a\" 1u)",
            "(\"a\" 007)",
            "(\"a\" 1.5)",
            "(\"a\" word)",
            "(1 2)",
            "(\"a\" \"\\q\")",
            "(\"a\" 99999999999)",
            "(\"a\"",
            "(\"a\") x",
        ] {
            assert!(read(bad).is_err(), "{bad} should be refused");
        }
        assert!(
            require_complete(&read("(\"sfyx\" 1 \"p\")\n").unwrap()).is_err(),
            "truncated"
        );
    }
}
