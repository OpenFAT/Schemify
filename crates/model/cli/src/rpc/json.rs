//! The JSON codec of the command stream: one JSON text (RFC 8259) → a [`Json`] tree, and a
//! tree → compact JSON with no line break (`docs/spec/rpc.md` §2, §8). No JSON dependency.
//!
//! Data: the tree is the only allocation that scales with the input; it is at most
//! [`DEPTH_MAX`] deep and its text at most `LINE_MAX` bytes (the framing's cap).
//!
//! Contents:
//! - `pub const DEPTH_MAX: u32`
//! - `pub enum Json`, `Json::get`, `Json::as_str`, `Json::obj`
//! - `pub struct JsonError { at, what }`
//! - `pub fn parse(&str) -> Result<Json, JsonError>`
//! - `pub fn write(&Json, &mut String)`

use std::fmt::Write as _;

/// Deepest nesting of arrays and objects a request may have.
pub const DEPTH_MAX: u32 = 64;

/// A JSON value. Objects keep their member order (and duplicate keys; [`Json::get`] finds
/// the first), so encoding a decoded value reproduces it.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number; always finite (the decoder refuses `1e999`).
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, members in text order.
    Obj(Vec<(String, Json)>),
}

/// Why a line is not JSON: the byte offset and what was expected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset in the line.
    pub at: usize,
    /// What went wrong.
    pub what: &'static str,
}

/// The decoder's cursor over one text.
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

/// Parses one JSON text (RFC 8259; surrounding whitespace allowed).
///
/// # Errors
///
/// The first syntax error, nesting deeper than [`DEPTH_MAX`], a number outside the finite
/// `f64` range, a lone surrogate escape, or trailing characters. Never panics.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing characters"));
    }
    assert!(p.i == text.len(), "the whole text was read");
    Ok(v)
}

/// Appends `v` as compact JSON (no whitespace, so never a line break: one value is one
/// frame). Integral numbers below 2^53 are written without a fraction; other numbers in
/// Rust's shortest round-trip form; a non-finite number (never decoded) as `null`.
pub fn write(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(x) if !x.is_finite() => out.push_str("null"),
        Json::Num(x) if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 => {
            write!(out, "{}", *x as i64).expect("writing to a String");
        }
        Json::Num(x) => write!(out, "{x:?}").expect("writing to a String"),
        Json::Str(s) => write_str(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (i, (k, item)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write(item, out);
            }
            out.push('}');
        }
    }
}

/// Appends `s` as a JSON string: quotes, the short escapes, `\u00XX` for other control
/// characters; never a raw line break.
pub(crate) fn write_str(s: &str, out: &mut String) {
    let before = out.len();
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    debug_assert!(!out[before..].contains(['\n', '\r']));
}

impl Json {
    /// Returns the first member named `key` of an object; `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|m| m.0 == key).map(|m| &m.1),
            _ => None,
        }
    }

    /// Returns the text of a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Builds an object from `(key, value)` pairs, in order.
    pub fn obj(members: Vec<(&str, Json)>) -> Json {
        Json::Obj(
            members
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }
}

// ---- private ----

impl Parser<'_> {
    fn err(&self, what: &'static str) -> JsonError {
        JsonError { at: self.i, what }
    }

    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        let hit = self.b.get(self.i) == Some(&c);
        self.i += usize::from(hit);
        hit
    }

    /// Consumes a run of ASCII digits; returns its length.
    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, JsonError> {
        if !self.b[self.i..].starts_with(word.as_bytes()) {
            return Err(self.err("unknown literal"));
        }
        self.i += word.len();
        Ok(v)
    }

    /// One value at nesting `depth`; recursion is bounded by [`DEPTH_MAX`].
    fn value(&mut self, depth: u32) -> Result<Json, JsonError> {
        if depth >= DEPTH_MAX {
            return Err(self.err("nested too deep"));
        }
        match self.b.get(self.i) {
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            _ => Err(self.err("expected a value")),
        }
    }

    fn array(&mut self, depth: u32) -> Result<Json, JsonError> {
        assert_eq!(self.b[self.i], b'[');
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b']') {
            return Ok(Json::Arr(items));
        }
        // Each pass consumes at least one byte or returns: bounded by the text.
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.eat(b']') {
                return Ok(Json::Arr(items));
            }
            if !self.eat(b',') {
                return Err(self.err("expected `,` or `]`"));
            }
        }
    }

    fn object(&mut self, depth: u32) -> Result<Json, JsonError> {
        assert_eq!(self.b[self.i], b'{');
        self.i += 1;
        let mut members = Vec::new();
        self.ws();
        if self.eat(b'}') {
            return Ok(Json::Obj(members));
        }
        // Each pass consumes at least one byte or returns: bounded by the text.
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a member name"));
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(b':') {
                return Err(self.err("expected `:`"));
            }
            self.ws();
            members.push((key, self.value(depth + 1)?));
            self.ws();
            if self.eat(b'}') {
                return Ok(Json::Obj(members));
            }
            if !self.eat(b',') {
                return Err(self.err("expected `,` or `}`"));
            }
        }
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.i;
        self.eat(b'-');
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.err("expected a digit")),
        }
        if self.eat(b'.') && self.digits() == 0 {
            return Err(self.err("expected a digit after `.`"));
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if self.digits() == 0 {
                return Err(self.err("expected an exponent"));
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII digits and signs");
        match text.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(Json::Num(x)),
            _ => Err(JsonError {
                at: start,
                what: "number out of range",
            }),
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        assert_eq!(self.b[self.i], b'"');
        self.i += 1;
        let mut out = String::new();
        // Each pass consumes at least one byte or returns: bounded by the text.
        loop {
            let run = self.i;
            while self
                .b
                .get(self.i)
                .is_some_and(|&c| c != b'"' && c != b'\\' && c >= 0x20)
            {
                self.i += 1;
            }
            // Stops only at ASCII bytes, so the run ends on a character boundary.
            out.push_str(std::str::from_utf8(&self.b[run..self.i]).expect("input is UTF-8"));
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = match self.b.get(self.i) {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.i += 1;
                            out.push(self.unicode()?);
                            continue;
                        }
                        _ => return Err(self.err("unknown escape")),
                    };
                    self.i += 1;
                    out.push(c);
                }
                Some(_) => return Err(self.err("control character in a string")),
                None => return Err(self.err("unterminated string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let b = self.b;
        let Some(h) = b
            .get(self.i..self.i + 4)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
        else {
            return Err(self.err("`\\u` needs four hex digits"));
        };
        self.i += 4;
        let text = std::str::from_utf8(h).expect("hex digits are ASCII");
        let code = u32::from_str_radix(text, 16).expect("four hex digits");
        assert!(code <= 0xFFFF);
        Ok(code)
    }

    /// The character of a `\u` escape (its `\u` consumed), joining a surrogate pair.
    fn unicode(&mut self) -> Result<char, JsonError> {
        let hi = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            if !(self.eat(b'\\') && self.eat(b'u')) {
                return Err(self.err("lone surrogate"));
            }
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.err("lone surrogate"));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(code).ok_or(self.err("lone surrogate"))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Generated values: every variant, keys and strings that need escapes, nesting to 4.
    pub(crate) fn json() -> impl Strategy<Value = Json> {
        let leaf = prop_oneof![
            Just(Json::Null),
            any::<bool>().prop_map(Json::Bool),
            any::<f64>()
                .prop_filter("finite", |x| x.is_finite())
                .prop_map(Json::Num),
            (-1_000_000i64..1_000_000).prop_map(|i| Json::Num(i as f64)),
            "\\PC{0,12}|[\\x00-\\x7f]{0,12}".prop_map(Json::Str),
        ];
        leaf.prop_recursive(4, 48, 6, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..6).prop_map(Json::Arr),
                proptest::collection::vec(("[a-z\"\\\\]{0,6}", inner), 0..6).prop_map(Json::Obj),
            ]
        })
    }

    pub(crate) fn text(v: &Json) -> String {
        let mut s = String::new();
        write(v, &mut s);
        s
    }

    proptest! {
        /// Oracle: decoding an encoded value gives the value back, and the encoding is one
        /// line.
        #[test]
        fn json_round_trips(v in json()) {
            let s = text(&v);
            prop_assert!(!s.contains('\n'));
            prop_assert_eq!(parse(&s), Ok(v));
        }
    }

    #[test]
    fn parse_accepts_rfc_8259_and_refuses_the_rest() {
        let ok = [
            ("  null ", Json::Null),
            ("-0.5e2", Json::Num(-50.0)),
            (r#""a\u00e9\ud83d\ude00\n""#, Json::Str("aé😀\n".into())),
            (
                r#"{"a":[1,true],"a":{}}"#,
                Json::Obj(vec![
                    (
                        "a".into(),
                        Json::Arr(vec![Json::Num(1.0), Json::Bool(true)]),
                    ),
                    ("a".into(), Json::Obj(Vec::new())),
                ]),
            ),
        ];
        for (src, want) in ok {
            assert_eq!(parse(src), Ok(want), "{src}");
        }
        let deep = "[".repeat(DEPTH_MAX as usize + 1);
        for bad in [
            "",
            "01",
            "1.",
            "-",
            "1e",
            "+1",
            "1e999",
            "nul",
            "[1,]",
            "{\"a\" 1}",
            "{a:1}",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\u12g4\"",
            "\"\\u+123\"",
            "\"a\nb\"",
            "1 2",
            deep.as_str(),
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    /// Oracle: the spec's number forms (§8): integers below 2^53 without a fraction, the
    /// rest in shortest round-trip form, non-finite as `null`.
    #[test]
    fn numbers_write_in_their_shortest_exact_form() {
        let cases = [
            (0.0, "0"),
            (-3.0, "-3"),
            (9_007_199_254_740_991.0, "9007199254740991"),
            (9_007_199_254_740_992.0, "9007199254740992.0"),
            (0.5, "0.5"),
            (f64::NAN, "null"),
            (f64::INFINITY, "null"),
        ];
        for (x, want) in cases {
            assert_eq!(text(&Json::Num(x)), want, "{x}");
        }
        assert_eq!(text(&Json::Str("\u{1}\"\t".into())), r#""\u0001\"\t""#);
    }
}
