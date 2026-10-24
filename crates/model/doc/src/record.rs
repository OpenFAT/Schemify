//! One record as owned text ([`RecordBuf`]) and its validation against the format
//! schema. `RecordBuf` is the payload of `insert`/`put` ops, the before-image an undo
//! entry restores, and what [`crate::Document::record`] returns; it is cold data
//! (one allocation per string), never used on the load/save path.

use crate::Code;
use schemify_format::{CoordError, Orient, is_id, is_number, parse_coord};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, Rest, Shape, Ty};

/// One attribute line: key plus values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    /// The attribute word.
    pub key: String,
    /// Its values, unescaped.
    pub values: Vec<String>,
}

/// A record as owned text, in the format's model: kind, positional fields (after the
/// word), attribute lines in order, verbatim lines and attached comments.
///
/// Coordinates are text here (`"-12.5"`); tables store them as i32 milli-units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordBuf {
    /// The record kind; `Header` only as the payload of a header `put`.
    pub kind: RecordKind,
    /// Positional values after the word.
    pub fields: Vec<String>,
    /// Attribute lines in file order.
    pub attrs: Vec<Attr>,
    /// `| text` lines (only `prose`, `body`, `code`).
    pub verbatim: Vec<String>,
    /// Comments written before the record (text after `#`).
    pub comments: Vec<String>,
}

impl RecordBuf {
    /// Creates an empty record of `kind`; add parts with the builder methods.
    pub fn new(kind: RecordKind) -> RecordBuf {
        assert!(
            kind != RecordKind::Unknown,
            "unknown records are not editable"
        );
        RecordBuf {
            kind,
            fields: Vec::new(),
            attrs: Vec::new(),
            verbatim: Vec::new(),
            comments: Vec::new(),
        }
    }

    /// Appends a positional field.
    pub fn field(mut self, text: &str) -> RecordBuf {
        self.fields.push(text.to_owned());
        self
    }

    /// Appends an attribute line.
    pub fn attr(mut self, key: &str, values: &[&str]) -> RecordBuf {
        self.attrs.push(Attr {
            key: key.to_owned(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
        });
        self
    }

    /// Appends a verbatim line.
    pub fn verbatim(mut self, line: &str) -> RecordBuf {
        self.verbatim.push(line.to_owned());
        self
    }

    /// Returns the first attribute line with `key`.
    pub fn find_attr(&self, key: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.key == key)
    }

    /// Copies record `r` of a parsed or built format document.
    pub fn from_format(doc: &FormatDoc<'_>, r: u32) -> RecordBuf {
        let kind = doc.record_kind(r);
        assert!(
            kind != RecordKind::Unknown,
            "unknown records are not editable"
        );
        let rec = RecordBuf {
            kind,
            fields: doc.fields(r).map(str::to_owned).collect(),
            attrs: doc
                .attrs(r)
                .map(|a| Attr {
                    key: doc.attr_key(a).to_owned(),
                    values: doc.attr_values(a).map(str::to_owned).collect(),
                })
                .collect(),
            verbatim: doc.verbatim(r).map(str::to_owned).collect(),
            comments: doc.comments(r).map(str::to_owned).collect(),
        };
        assert_eq!(rec.fields.len(), doc.fields(r).len());
        rec
    }
}

/// Attribute keys of an instance that are not parameter overrides (`cached <param> <value>
/// <digest>` holds a value Virtuoso derived, D20 as amended 2026-10-15).
pub(crate) const INST_RESERVED: [&str; 5] = ["at", "name", "inherit", "ignore", "cached"];

/// Validates `rec` as a record of a `file` document: allowed kind (`E027`), positional
/// shape and values (`E029`, `E022`), attribute shapes, duplicates (`E024`) and required
/// attributes (`E029`), verbatim lines only where allowed (`E029`), and the lexical
/// rules the writer relies on (`E028`: attribute keys are words, no line breaks in
/// verbatim lines or comments, no trailing whitespace in comments).
///
/// Zero-length wires (`E026`) are checked at the end of a transaction, not here, so a
/// wire may pass through zero length between two `set_field`s.
pub(crate) fn check_record(rec: &RecordBuf, file: FileKind) -> Result<(), (Code, &'static str)> {
    let schema = rec.kind.schema();
    if rec.kind == RecordKind::Header {
        if !rec.verbatim.is_empty() || !rec.comments.is_empty() {
            return Err((
                Code::E029,
                "the header carries no verbatim lines or comments",
            ));
        }
    } else {
        if rec.kind == RecordKind::Unknown || !file.records().contains(&rec.kind) {
            return Err((Code::E027, "record kind not allowed in this file kind"));
        }
        check_shape(&schema.fields, &rec.fields)?;
    }
    let mut seen: u32 = 0;
    for (i, a) in rec.attrs.iter().enumerate() {
        if !is_word(&a.key) {
            return Err((Code::E028, "attribute key is not a word"));
        }
        match schema.attr(&a.key) {
            Some((k, attr)) => {
                if seen & (1 << k) != 0 && !attr.repeat {
                    return Err((Code::E024, "attribute given twice"));
                }
                seen |= 1 << k;
                check_shape(&attr.shape, &a.values)?;
            }
            // Open kinds (instance overrides, constraint key/values): one line per key.
            None if schema.open && rec.attrs[..i].iter().any(|b| b.key == a.key) => {
                return Err((Code::E024, "attribute given twice"));
            }
            None => {}
        }
    }
    if schema
        .attrs
        .iter()
        .enumerate()
        .any(|(k, a)| a.required && seen & (1 << k) == 0)
    {
        return Err((Code::E029, "missing required attribute"));
    }
    if !rec.verbatim.is_empty() && !schema.verbatim {
        return Err((Code::E029, "this record kind takes no verbatim lines"));
    }
    if rec.kind == RecordKind::Shape
        && schemify_format::shape_arity(&rec.fields[1], rec.fields.len() - 2) != Some(true)
    {
        return Err((Code::E029, "wrong number of values for this shape form"));
    }
    if rec.verbatim.iter().any(|l| l.contains(['\n', '\r'])) {
        return Err((Code::E028, "verbatim line contains a line break"));
    }
    let bad_comment = |c: &String| c.contains(['\n', '\r']) || c.ends_with([' ', '\t']);
    if rec.comments.iter().any(bad_comment) {
        return Err((
            Code::E028,
            "comment contains a line break or trailing whitespace",
        ));
    }
    assert!(rec.kind == RecordKind::Header || file.records().contains(&rec.kind));
    Ok(())
}

/// Checks `values` against a schema shape (arity and per-slot value form).
pub(crate) fn check_shape(shape: &Shape, values: &[String]) -> Result<(), (Code, &'static str)> {
    let (n, fixed) = (values.len(), shape.fixed.len());
    assert!(fixed <= 8, "schema shapes are short");
    let arity_ok = n >= fixed
        && match shape.rest {
            Rest::None => n == fixed,
            Rest::Optional(_) => n <= fixed + 1,
            Rest::Many(_) => true,
            Rest::Pairs(_) => (n - fixed) % 2 == 0,
        };
    if !arity_ok {
        return Err((Code::E029, "wrong number of values"));
    }
    for (i, v) in values.iter().enumerate() {
        let ty = shape.ty(i).expect("arity checked above");
        let ok = match ty {
            Ty::Any => true,
            Ty::Id | Ty::IdRef => is_id(v),
            Ty::Coord => match parse_coord(v) {
                Ok(_) => true,
                Err(CoordError::NotRepresentable) => {
                    return Err((
                        Code::E022,
                        "coordinate needs more than 3 fractional digits or overflows",
                    ));
                }
                Err(CoordError::NotNumber) => false,
            },
            Ty::Number => is_number(v),
            Ty::Orient => Orient::parse(v).is_some(),
            Ty::Bool => v == "true" || v == "false",
            Ty::Choice(words) => words.contains(&v.as_str()),
        };
        if !ok {
            return Err((Code::E029, "value has the wrong form for its slot"));
        }
    }
    Ok(())
}

/// Returns whether `s` matches the format's `word` production.
pub(crate) fn is_word(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphabetic)
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst() -> RecordBuf {
        RecordBuf::new(RecordKind::Inst)
            .field("0gsrp0")
            .field("sky130:nfet_01v8")
            .attr("at", &["0", "-12.5", "R0"])
            .attr("name", &["M1"])
            .attr("W", &["2"])
    }

    #[test]
    fn valid_records_pass() {
        assert_eq!(check_record(&inst(), FileKind::Shcomp), Ok(()));
        let wire = RecordBuf::new(RecordKind::Wire)
            .field("0d2k1m")
            .field("0")
            .field("0")
            .field("10")
            .field("0");
        assert_eq!(check_record(&wire, FileKind::Shbench), Ok(()));
        let header = RecordBuf::new(RecordKind::Header).attr("doc", &["x"]);
        assert_eq!(check_record(&header, FileKind::Shcomp), Ok(()));
    }

    #[test]
    fn negative_space_reports_the_right_code() {
        let code = |r: &RecordBuf, f| check_record(r, f).unwrap_err().0;
        assert_eq!(
            code(
                &RecordBuf::new(RecordKind::Pin)
                    .field("A")
                    .field("in")
                    .field("0")
                    .field("0"),
                FileKind::Shbench
            ),
            Code::E027
        );
        let mut r = inst();
        r.attrs.retain(|a| a.key != "name");
        assert_eq!(code(&r, FileKind::Shcomp), Code::E029);
        let r = inst().attr("W", &["3"]);
        assert_eq!(code(&r, FileKind::Shcomp), Code::E024);
        let r = inst().attr("at", &["1", "1", "R0"]);
        assert_eq!(code(&r, FileKind::Shcomp), Code::E024);
        let mut r = inst();
        r.attrs[0].values[1] = "0.0005".into();
        assert_eq!(code(&r, FileKind::Shcomp), Code::E022);
        let mut r = inst();
        r.attrs[0].values[2] = "R45".into();
        assert_eq!(code(&r, FileKind::Shcomp), Code::E029);
        let mut r = inst();
        r.fields[0] = "BAD".into();
        assert_eq!(code(&r, FileKind::Shcomp), Code::E029);
        let r = inst().attr("2bad", &[]);
        assert_eq!(code(&r, FileKind::Shcomp), Code::E028);
        let r = inst().verbatim("x");
        assert_eq!(code(&r, FileKind::Shcomp), Code::E029);
        let mut r = inst();
        r.comments.push("trailing ".into());
        assert_eq!(code(&r, FileKind::Shcomp), Code::E028);
        let r = RecordBuf::new(RecordKind::Prose)
            .field("0abc")
            .verbatim("a\nb");
        assert_eq!(code(&r, FileKind::Shcomp), Code::E028);
    }
}
