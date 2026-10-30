//! Ops ↔ op text, the line-record form the journal stores and plugins send (plugin ABI
//! §7 `edit` records).
//! Cites: edits spec §1 (op text), format spec §3 (lexical rules).
//!
//! Text is parsed as a format `journal` file (no known record words, so every record is
//! kept as written) and written through the same format `Document` builder, so quoting,
//! escapes, verbatim lines and comments are exactly the format's. One record per op
//! (`<word> <key>` addresses a record, see [`Target`]):
//!
//! ```text
//! insert <word> <fields…>            (+ the record's attribute and | lines; # comments before it are its comments)
//! insert_at <row> <word> <fields…>   (same, at a row of an ordered table)
//! delete <word> <key>
//! put <word> <key> <fields…>         (+ attribute, | and # lines of the replacement)
//! set_field <word> <key> <index> <value>
//! set_attr <word> <key> <attr> <values…>
//! remove_attr <word> <key> <attr> [<first value>]
//! reorder <word> <key> <row>
//! ```
//!
//! Contents:
//! - `pub(crate) const JOURNAL_HEADER`, `pub(crate) struct LexError`
//! - `pub fn ops_from_text(text: &str, file: FileKind) -> Result<Vec<Op>, Vec<OpError>>`
//! - `pub fn ops_to_text(ops: &[Op]) -> String`
//! - `pub(crate) fn encode_into`, `pub(crate) fn push_op`, `pub(crate) fn lex`,
//!   `pub(crate) fn number`

use crate::Code;
use crate::ops::{Op, OpError, Target};
use crate::record::{Attr, RecordBuf};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind};
use std::str::FromStr;

/// Header the journal file starts with; op text is lexed and built under it too.
pub(crate) const JOURNAL_HEADER: &str = "schemify 1 journal\n";

/// A lexical error in op text or a journal body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LexError {
    /// 1-based line within the lexed body (0 for the header the lexer adds).
    pub(crate) line: u32,
    pub(crate) code: Code,
}

/// Parses op text (module docs) for a document of kind `file`, e.g. the `|` lines of a
/// plugin `edit` record (plugin ABI §7).
///
/// # Errors
///
/// Every failing op as (op index, code): `E028` lexical, `E027` unknown op word or
/// record word for `file`, `E029` wrong arity or a non-number index. Lexical errors are
/// attributed to the op whose lines contain them. Records are validated later, when
/// the transaction applies.
///
/// # Panics
///
/// Only on a bug in this function: a lexed body without errors yields one op per record.
pub fn ops_from_text(text: &str, file: FileKind) -> Result<Vec<Op>, Vec<OpError>> {
    let lexed = lex(text, |d| {
        let mut ops = Vec::with_capacity(d.record_count() as usize);
        let mut errors = Vec::new();
        for r in 1..d.record_count() {
            match decode_op(d, r, file) {
                Ok(op) => ops.push(op),
                Err(code) => errors.push(OpError { op: r - 1, code }),
            }
        }
        if errors.is_empty() {
            assert_eq!(
                ops.len() + 1,
                d.record_count() as usize,
                "one op per record"
            );
            Ok(ops)
        } else {
            Err(errors)
        }
    });
    match lexed {
        Ok(result) => result,
        Err(errors) => Err(errors
            .into_iter()
            .map(|e| OpError {
                op: op_index_at(text, e.line),
                code: e.code,
            })
            .collect()),
    }
}

/// Writes ops as op text (module docs), the form the journal stores and plugins send
/// (plugin ABI §7 `edit` records).
///
/// # Panics
///
/// Panics if a payload record breaks a lexical rule `check_record` enforces (a comment
/// or verbatim line with a line break, an attribute key that is not a word). Ops that
/// a transaction accepted never do.
#[must_use]
pub fn ops_to_text(ops: &[Op]) -> String {
    let mut text = String::new();
    encode_into(&mut text, |d| {
        for op in ops {
            push_op(d, op);
        }
    });
    assert!(ops.is_empty() || !text.is_empty());
    text
}

/// Appends the record lines `build` adds to `out`: the format writer's text without the
/// journal header it starts with, written in place (no intermediate copy).
pub(crate) fn encode_into(out: &mut String, build: impl FnOnce(&mut FormatDoc<'static>)) {
    let mut d = FormatDoc::new(FileKind::Journal);
    build(&mut d);
    let start = out.len();
    schemify_format::write_into(&d, out);
    assert!(out[start..].starts_with(JOURNAL_HEADER));
    let blank = out[start + JOURNAL_HEADER.len()..].starts_with('\n');
    out.replace_range(start..start + JOURNAL_HEADER.len() + usize::from(blank), "");
    assert!(out.len() == start || out.ends_with('\n'));
}

pub(crate) fn push_op(d: &mut FormatDoc<'static>, op: &Op) {
    let records = d.record_count();
    match op {
        Op::Insert { rec, at: None } => {
            d.begin_unknown_record("insert");
            d.push_field(rec.kind.word());
            push_payload(d, rec);
        }
        Op::Insert { rec, at: Some(row) } => {
            d.begin_unknown_record("insert_at");
            d.push_field(&row.to_string());
            d.push_field(rec.kind.word());
            push_payload(d, rec);
        }
        Op::Delete(t) => {
            d.begin_unknown_record("delete");
            push_target(d, t);
        }
        Op::Put { target, rec } => {
            d.begin_unknown_record("put");
            push_target(d, target);
            push_payload(d, rec);
        }
        Op::SetField {
            target,
            index,
            value,
        } => {
            d.begin_unknown_record("set_field");
            push_target(d, target);
            d.push_field(&index.to_string());
            d.push_field(value);
        }
        Op::SetAttr {
            target,
            key,
            values,
        } => {
            d.begin_unknown_record("set_attr");
            push_target(d, target);
            d.push_field(key);
            for v in values {
                d.push_field(v);
            }
        }
        Op::RemoveAttr { target, key, first } => {
            d.begin_unknown_record("remove_attr");
            push_target(d, target);
            d.push_field(key);
            if let Some(f) = first {
                d.push_field(f);
            }
        }
        Op::Reorder { target, to } => {
            d.begin_unknown_record("reorder");
            push_target(d, target);
            d.push_field(&to.to_string());
        }
    }
    assert_eq!(d.record_count(), records + 1, "one record per op");
}

/// Lexes `body` (record lines without a header) with the format's lexer and hands the
/// model to `read`.
pub(crate) fn lex<T>(
    body: &str,
    read: impl FnOnce(&FormatDoc<'_>) -> T,
) -> Result<T, Vec<LexError>> {
    let mut text = String::with_capacity(JOURNAL_HEADER.len() + body.len());
    text.push_str(JOURNAL_HEADER);
    text.push_str(body);
    match schemify_format::parse(text.as_bytes()) {
        Ok(d) => {
            assert_eq!(d.kind(), FileKind::Journal);
            assert!((1..d.record_count()).all(|r| d.record_kind(r) == RecordKind::Unknown));
            Ok(read(&d))
        }
        Err(e) => Err(e
            .diagnostics
            .iter()
            .map(|g| LexError {
                line: g.line.saturating_sub(1),
                code: Code::from_format(g.code),
            })
            .collect()),
    }
}

/// Parses a decimal of digits only (no sign, no `+`), as op text and journal records
/// write row indices and sequence numbers.
pub(crate) fn number<T: FromStr>(text: &str) -> Option<T> {
    let digits = text.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| text.parse().ok()).flatten()
}

// ---- private ----

fn push_payload(d: &mut FormatDoc<'static>, rec: &RecordBuf) {
    let records = d.record_count();
    assert!(
        records > 1,
        "a payload extends an op record after the header"
    );
    for c in &rec.comments {
        d.push_comment(c);
    }
    for f in &rec.fields {
        d.push_field(f);
    }
    for a in &rec.attrs {
        d.push_attr(&a.key);
        for v in &a.values {
            d.push_value(v);
        }
    }
    for v in &rec.verbatim {
        d.push_verbatim(v);
    }
    assert_eq!(d.record_count(), records, "the payload starts no record");
}

fn push_target(d: &mut FormatDoc<'static>, t: &Target) {
    assert!(t.kind != RecordKind::Unknown, "ops address known kinds");
    d.push_field(t.kind.word());
    d.push_field(&t.key);
}

fn kind_of(word: &str, file: FileKind) -> Result<RecordKind, Code> {
    if word == RecordKind::Header.word() {
        return Ok(RecordKind::Header);
    }
    file.lookup(word).ok_or(Code::E027)
}

fn payload(d: &FormatDoc<'_>, r: u32, kind: RecordKind, fields: &[&str]) -> RecordBuf {
    let rec = RecordBuf {
        kind,
        fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        attrs: d
            .attrs(r)
            .map(|a| Attr {
                key: d.attr_key(a).to_owned(),
                values: d.attr_values(a).map(str::to_owned).collect(),
            })
            .collect(),
        verbatim: d.verbatim(r).map(str::to_owned).collect(),
        comments: d.comments(r).map(str::to_owned).collect(),
    };
    assert_eq!(rec.fields.len(), fields.len());
    assert_eq!(
        rec.attrs.len(),
        d.attrs(r).len(),
        "every attribute line copied"
    );
    rec
}

fn decode_op(d: &FormatDoc<'_>, r: u32, file: FileKind) -> Result<Op, Code> {
    let word = d.record_word(r);
    let f: Vec<&str> = d.fields(r).collect();
    let target = |f: &[&str]| -> Result<Target, Code> {
        match f {
            [w, key, ..] => Ok(Target::new(kind_of(w, file)?, key)),
            _ => Err(Code::E029),
        }
    };
    let op = match word {
        "insert" => {
            let (w, rest) = f.split_first().ok_or(Code::E029)?;
            Op::Insert {
                rec: payload(d, r, kind_of(w, file)?, rest),
                at: None,
            }
        }
        "insert_at" => {
            let [row, w, rest @ ..] = f.as_slice() else {
                return Err(Code::E029);
            };
            Op::Insert {
                rec: payload(d, r, kind_of(w, file)?, rest),
                at: Some(number(row).ok_or(Code::E029)?),
            }
        }
        "delete" if f.len() == 2 => Op::Delete(target(&f)?),
        "put" => {
            let t = target(&f)?;
            let rec = payload(d, r, t.kind, &f[2..]);
            Op::Put { target: t, rec }
        }
        "set_field" if f.len() == 4 => Op::SetField {
            target: target(&f)?,
            index: number(f[2]).ok_or(Code::E029)?,
            value: f[3].to_owned(),
        },
        "set_attr" if f.len() >= 3 => Op::SetAttr {
            target: target(&f)?,
            key: f[2].to_owned(),
            values: f[3..].iter().map(|v| (*v).to_owned()).collect(),
        },
        "remove_attr" if (3..=4).contains(&f.len()) => Op::RemoveAttr {
            target: target(&f)?,
            key: f[2].to_owned(),
            first: f.get(3).map(|v| (*v).to_owned()),
        },
        "reorder" if f.len() == 3 => Op::Reorder {
            target: target(&f)?,
            to: number(f[2]).ok_or(Code::E029)?,
        },
        "delete" | "set_field" | "set_attr" | "remove_attr" | "reorder" => return Err(Code::E029),
        _ => return Err(Code::E027),
    };
    let is_payload_op = matches!(op, Op::Insert { .. } | Op::Put { .. });
    if !is_payload_op && (!d.attrs(r).is_empty() || d.verbatim(r).len() > 0) {
        return Err(Code::E029);
    }
    assert!(
        op.subject().0 != RecordKind::Unknown,
        "ops address known kinds"
    );
    Ok(op)
}

/// Returns the index of the op whose lines include 1-based `line` of `text`: the
/// number of record lines (column-0 letters) before it, minus one, floored at 0.
fn op_index_at(text: &str, line: u32) -> u32 {
    let starts = text
        .lines()
        .take(line as usize)
        .filter(|l| l.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
        .count() as u32;
    assert!(starts <= line, "at most one record starts per line");
    starts.saturating_sub(1)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::record::RecordBuf;

    pub(crate) fn sample_ops() -> Vec<Op> {
        let mut inst = RecordBuf::new(RecordKind::Inst)
            .field("0gsrp0")
            .field("sky130:nfet_01v8")
            .attr("at", &["0", "-12.5", "R0"])
            .attr("name", &["M 1"])
            .attr("W", &["say \"hi\"\n\\"]);
        inst.comments.push(" placed by test".into());
        let prose = RecordBuf::new(RecordKind::Prose)
            .field("0abc")
            .verbatim("# Title")
            .verbatim("")
            .verbatim("| not a marker");
        vec![
            Op::Insert {
                rec: inst,
                at: None,
            },
            Op::Insert {
                rec: prose,
                at: Some(2),
            },
            Op::Delete(Target::new(RecordKind::Wire, "0d2k1m")),
            Op::Put {
                target: Target::header(),
                rec: RecordBuf::new(RecordKind::Header).attr("doc", &["x y"]),
            },
            Op::SetField {
                target: Target::new(RecordKind::Label, "2ma7xe"),
                index: 4,
                value: "".into(),
            },
            Op::SetAttr {
                target: Target::new(RecordKind::Inst, "0gsrp0"),
                key: "at".into(),
                values: vec!["1".into(), "2".into(), "MR90".into()],
            },
            Op::RemoveAttr {
                target: Target::new(RecordKind::Inst, "0gsrp0"),
                key: "inherit".into(),
                first: Some("VDD".into()),
            },
            Op::Reorder {
                target: Target::new(RecordKind::Line, "3"),
                to: 0,
            },
        ]
    }

    #[test]
    fn op_text_round_trips() {
        let ops = sample_ops();
        let text = ops_to_text(&ops);
        assert_eq!(ops_from_text(&text, FileKind::Shcomp), Ok(ops));
        assert_eq!(ops_from_text("", FileKind::Shcomp), Ok(vec![]));
    }

    #[test]
    fn op_text_negative_space() {
        let err = |t: &str| ops_from_text(t, FileKind::Shbench).unwrap_err();
        assert_eq!(
            err("frob inst 0abc\n")[0],
            OpError {
                op: 0,
                code: Code::E027
            }
        );
        assert_eq!(err("delete inst\n")[0].code, Code::E029);
        assert_eq!(
            err("delete pin A\n")[0].code,
            Code::E027,
            "pin is not a .shbench kind"
        );
        assert_eq!(
            err("delete inst 0abc\nreorder var v x\n")[0],
            OpError {
                op: 1,
                code: Code::E029
            }
        );
        assert_eq!(
            err("reorder line 1 0\n")[0].code,
            Code::E027,
            "graphics are not .shbench kinds"
        );
        assert_eq!(err("delete inst 0abc\n  at 1 2 R0\n")[0].code, Code::E029);
        let lexical = err("delete inst 0abc\ndelete inst \"open\n");
        assert_eq!(
            lexical[0],
            OpError {
                op: 1,
                code: Code::E028
            }
        );
    }
}
