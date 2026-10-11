//! Op text and the edit journal (edits spec §1 op text, §3 journal).
//!
//! Both use the line-record syntax: text is parsed as a format `journal` file (no known
//! record words, so every record is kept as written) and written through the same
//! `Document` builder. Quoting, escapes, verbatim lines and comments are therefore
//! exactly the format's.
//!
//! Op text, one record per op (`<word> <key>` addresses a record, see [`Target`]):
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
//! Journal records: `base <sha256>`, `hist`/`tx <seq> <unix_ms> <label>` (op lines as
//! `|` verbatim lines) each followed by `inverse <seq>` (the inverse op lines),
//! `undo <seq>`, `redo <seq>`, `save <seq> <sha256>`.

use crate::Code;
use crate::edit::{Op, OpError, Target};
use crate::record::{Attr, RecordBuf};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, parse, write};

/// Header the journal file starts with; op text is lexed and built under it too.
pub(crate) const JOURNAL_HEADER: &str = "schemify 1 journal\n";
/// Torn-tail retries when a journal does not lex (each drops the last record).
const TORN_RETRIES_MAX: u32 = 4;

// ------------------------------------------------------------------ encoding

fn encode(build: impl FnOnce(&mut FormatDoc<'static>)) -> String {
    let mut d = FormatDoc::new(FileKind::Journal);
    build(&mut d);
    let text = write(&d);
    let body = text
        .strip_prefix(JOURNAL_HEADER)
        .expect("the builder writes its header first");
    let body = body.strip_prefix('\n').unwrap_or(body);
    assert!(body.is_empty() || body.ends_with('\n'));
    body.to_owned()
}

fn push_payload(d: &mut FormatDoc<'static>, rec: &RecordBuf) {
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
}

fn push_target(d: &mut FormatDoc<'static>, t: &Target) {
    d.push_field(t.kind.word());
    d.push_field(&t.key);
}

fn push_op(d: &mut FormatDoc<'static>, op: &Op) {
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
}

/// Writes ops as op text (module docs), the form the journal stores and plugins send
/// (plugin ABI §7 `edit` records).
///
/// # Panics
///
/// Panics if a payload record breaks a lexical rule `check_record` enforces (a comment
/// or verbatim line with a line break, an attribute key that is not a word). Ops that
/// a transaction accepted never do.
pub fn ops_to_text(ops: &[Op]) -> String {
    let text = encode(|d| {
        for op in ops {
            push_op(d, op);
        }
    });
    assert!(ops.is_empty() || !text.is_empty());
    text
}

// ------------------------------------------------------------------ decoding

/// Lexes `body` (record lines without a header) with the format's lexer and hands the
/// model to `read`. Errors are (1-based line within `body`, code).
fn lex<T>(body: &str, read: impl FnOnce(&FormatDoc<'_>) -> T) -> Result<T, Vec<(u32, Code)>> {
    let mut text = String::with_capacity(JOURNAL_HEADER.len() + body.len());
    text.push_str(JOURNAL_HEADER);
    text.push_str(body);
    match parse(text.as_bytes()) {
        Ok(d) => {
            assert_eq!(d.kind(), FileKind::Journal);
            assert!((1..d.record_count()).all(|r| d.record_kind(r) == RecordKind::Unknown));
            Ok(read(&d))
        }
        Err(e) => Err(e
            .diagnostics
            .iter()
            .map(|g| (g.line.saturating_sub(1), Code::from_format(g.code)))
            .collect()),
    }
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
    rec
}

fn number(text: &str) -> Result<u32, Code> {
    let ok = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| text.parse().ok()).flatten().ok_or(Code::E029)
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
                at: Some(number(row)?),
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
            index: number(f[2])?,
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
            to: number(f[2])?,
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

/// Parses op text (module docs) for a document of kind `file`, e.g. the `|` lines of a
/// plugin `edit` record (plugin ABI §7).
///
/// # Errors
///
/// Every failing op as (op index, code): `E028` lexical, `E027` unknown op word or
/// record word for `file`, `E029` wrong arity or a non-number index. Lexical errors are
/// attributed to the op whose lines contain them. Records are validated later, when
/// the transaction applies.
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
        Err(diags) => Err(diags
            .into_iter()
            .map(|(line, code)| OpError {
                op: op_index_at(text, line),
                code,
            })
            .collect()),
    }
}

/// Returns the index of the op whose lines include 1-based `line` of `text`: the
/// number of record lines (column-0 letters) before it, minus one, floored at 0.
fn op_index_at(text: &str, line: u32) -> u32 {
    let starts = text
        .lines()
        .take(line as usize)
        .filter(|l| l.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
        .count() as u32;
    starts.saturating_sub(1)
}

// ------------------------------------------------------------------ journal records

/// One committed transaction as the journal and the undo stack keep it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) seq: u64,
    pub(crate) unix_ms: u64,
    pub(crate) label: String,
    /// Ops as applied (fresh ids drawn, values snapped).
    pub(crate) forward: Vec<Op>,
    /// Ops that undo `forward`, in application order.
    pub(crate) inverse: Vec<Op>,
}

/// One journal record, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JRec {
    /// The file hash the following records apply to.
    Base(String),
    /// A transaction that is undo history at `base` (not replayed).
    Hist(Entry),
    /// A committed transaction.
    Tx(Entry),
    /// The undo stack's top (seq) was undone.
    Undo(u64),
    /// The redo stack's top (seq) was redone.
    Redo(u64),
    /// The document was saved with this hash after transaction `seq`.
    Save(u64, String),
}

fn push_lines(d: &mut FormatDoc<'static>, text: &str) {
    for line in text.lines() {
        d.push_verbatim(line);
    }
}

fn push_entry(d: &mut FormatDoc<'static>, word: &str, e: &Entry) {
    assert_eq!(e.forward.len(), e.inverse.len(), "one inverse per op");
    assert!(word == "tx" || word == "hist");
    d.begin_unknown_record(word);
    d.push_field(&e.seq.to_string());
    d.push_field(&e.unix_ms.to_string());
    d.push_field(&e.label);
    push_lines(d, &ops_to_text(&e.forward));
    d.begin_unknown_record("inverse");
    d.push_field(&e.seq.to_string());
    push_lines(d, &ops_to_text(&e.inverse));
}

/// Encodes journal records (no header).
pub(crate) fn encode_records(recs: &[JRec]) -> String {
    encode(|d| {
        for r in recs {
            match r {
                JRec::Base(h) => {
                    d.begin_unknown_record("base");
                    d.push_field(h);
                }
                JRec::Hist(e) => push_entry(d, "hist", e),
                JRec::Tx(e) => push_entry(d, "tx", e),
                JRec::Undo(s) | JRec::Redo(s) => {
                    d.begin_unknown_record(if matches!(r, JRec::Undo(_)) {
                        "undo"
                    } else {
                        "redo"
                    });
                    d.push_field(&s.to_string());
                }
                JRec::Save(s, h) => {
                    d.begin_unknown_record("save");
                    d.push_field(&s.to_string());
                    d.push_field(h);
                }
            }
        }
    })
}

fn seq(text: &str) -> Option<u64> {
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

/// Decodes journal records from `d`, stopping at the first record that is incomplete
/// or malformed (a torn tail: everything after it is dropped).
fn decode_records(d: &FormatDoc<'_>, file: FileKind) -> Vec<JRec> {
    let mut out = Vec::new();
    let mut r = 1;
    while r < d.record_count() {
        let f: Vec<&str> = d.fields(r).collect();
        let rec = match (d.record_word(r), f.as_slice()) {
            ("base", [h]) => Some(JRec::Base((*h).to_owned())),
            ("undo", [s]) => seq(s).map(JRec::Undo),
            ("redo", [s]) => seq(s).map(JRec::Redo),
            ("save", [s, h]) => seq(s).map(|s| JRec::Save(s, (*h).to_owned())),
            (w @ ("tx" | "hist"), [s, ms, label]) => {
                let entry = decode_entry(d, r, file, s, ms, label);
                r += 1; // the `inverse` record
                entry.map(|e| {
                    if w == "tx" {
                        JRec::Tx(e)
                    } else {
                        JRec::Hist(e)
                    }
                })
            }
            _ => None,
        };
        let Some(rec) = rec else { break };
        out.push(rec);
        r += 1;
    }
    assert!(out.len() < d.record_count() as usize);
    out
}

fn decode_entry(
    d: &FormatDoc<'_>,
    r: u32,
    file: FileKind,
    s: &str,
    ms: &str,
    label: &str,
) -> Option<Entry> {
    let inv = r + 1;
    if inv >= d.record_count() || d.record_word(inv) != "inverse" || d.field(inv, 0) != Some(s) {
        return None;
    }
    let ops = |rec: u32| -> Option<Vec<Op>> {
        let text: String = d.verbatim(rec).flat_map(|l| [l, "\n"]).collect();
        ops_from_text(&text, file).ok()
    };
    let entry = Entry {
        seq: seq(s)?,
        unix_ms: seq(ms)?,
        label: label.to_owned(),
        forward: ops(r)?,
        inverse: ops(inv)?,
    };
    (entry.forward.len() == entry.inverse.len()).then_some(entry)
}

/// Parses a journal file. A torn tail (a crash mid-append) is dropped: the text is cut
/// before the record holding the first lexical error and re-lexed.
///
/// # Errors
///
/// `E028` when line 1 is not `schemify 1 journal`.
pub(crate) fn parse_journal(bytes: &[u8], file: FileKind) -> Result<Vec<JRec>, Code> {
    let text = String::from_utf8_lossy(bytes);
    let body = text.strip_prefix(JOURNAL_HEADER).ok_or(Code::E028)?;
    let mut body: &str = body;
    for _ in 0..TORN_RETRIES_MAX {
        match lex(body, |d| decode_records(d, file)) {
            Ok(recs) => return Ok(recs),
            Err(errs) => {
                let line = errs.iter().map(|e| e.0).min().unwrap_or(1);
                let cut = record_start_before(body, line);
                assert!(cut < body.len(), "every retry drops at least one record");
                body = &body[..cut];
            }
        }
    }
    Ok(Vec::new())
}

/// Returns the byte offset where the record containing 1-based `line` starts.
fn record_start_before(body: &str, line: u32) -> usize {
    let mut start = 0;
    let mut offset = 0;
    for (i, l) in body.split_inclusive('\n').enumerate() {
        if i as u32 + 1 > line {
            break;
        }
        if l.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
            start = offset;
        }
        offset += l.len();
    }
    assert!(start <= body.len());
    start
}

// ------------------------------------------------------------------ recovery analysis

/// What a journal says about a file whose current hash is known (edits spec §3).
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    /// Undo stack at the checkpoint, oldest first.
    pub(crate) undo: Vec<Entry>,
    /// Redo stack at the checkpoint, top last.
    pub(crate) redo: Vec<Entry>,
    /// Records after the checkpoint: unsaved edits to replay (`Tx`, `Undo`, `Redo`).
    pub(crate) tail: Vec<JRec>,
    /// A checkpoint (`base`/`save`) matched the file hash. Otherwise the file changed
    /// outside Schemify; history is dropped and the tail is replayed best-effort.
    pub(crate) matched: bool,
    /// Next transaction sequence number.
    pub(crate) next_seq: u64,
}

/// Splits journal records at the last checkpoint whose hash is `file_hash` (or, when
/// none matches, the last checkpoint at all) and rebuilds the undo/redo stacks the
/// records before it describe, without applying anything.
pub(crate) fn analyze(recs: Vec<JRec>, file_hash: &str, undo_max: usize) -> Analysis {
    let is_cp = |r: &JRec| matches!(r, JRec::Base(_) | JRec::Save(..));
    let matches_file = |r: &JRec| match r {
        JRec::Base(h) | JRec::Save(_, h) => h == file_hash,
        _ => false,
    };
    let matching = recs.iter().rposition(matches_file);
    let cp = matching.or_else(|| recs.iter().rposition(is_cp));
    let next_seq = recs
        .iter()
        .filter_map(|r| match r {
            JRec::Tx(e) | JRec::Hist(e) => Some(e.seq),
            JRec::Save(s, _) => Some(*s),
            _ => None,
        })
        .max()
        .map_or(1, |s| s + 1);
    let mut a = Analysis {
        matched: matching.is_some(),
        next_seq,
        ..Analysis::default()
    };
    let mut split = cp.map_or(0, |i| i + 1);
    // `hist` records written after a `base` are the history *at* that base.
    while recs.get(split).is_some_and(|r| matches!(r, JRec::Hist(_))) {
        split += 1;
    }
    let mut recs = recs;
    a.tail = recs.split_off(split);
    a.tail
        .retain(|r| matches!(r, JRec::Tx(_) | JRec::Undo(_) | JRec::Redo(_)));
    if a.matched {
        for r in recs {
            match r {
                JRec::Hist(e) | JRec::Tx(e) => {
                    a.redo.clear(); // a commit clears redo; history entries have none
                    a.undo.push(e);
                    if a.undo.len() > undo_max {
                        a.undo.remove(0);
                    }
                }
                JRec::Undo(s) => {
                    if a.undo.last().is_some_and(|e| e.seq == s) {
                        let e = a.undo.pop().expect("checked");
                        a.redo.push(e);
                    }
                }
                JRec::Redo(s) => {
                    if a.redo.last().is_some_and(|e| e.seq == s) {
                        let e = a.redo.pop().expect("checked");
                        a.undo.push(e);
                    }
                }
                JRec::Base(_) | JRec::Save(..) => {}
            }
        }
    }
    assert!(a.undo.len() <= undo_max);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordBuf;

    fn sample_ops() -> Vec<Op> {
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

    #[test]
    fn journal_round_trips_and_drops_a_torn_tail() {
        let e = Entry {
            seq: 7,
            unix_ms: 1_700_000_000_000,
            label: "move M1".into(),
            forward: sample_ops(),
            inverse: sample_ops(),
        };
        let recs = vec![
            JRec::Base("ab".into()),
            JRec::Hist(e.clone()),
            JRec::Tx(e.clone()),
            JRec::Undo(7),
            JRec::Redo(7),
            JRec::Save(7, "cd".into()),
        ];
        let text = format!("{JOURNAL_HEADER}{}", encode_records(&recs));
        assert_eq!(
            parse_journal(text.as_bytes(), FileKind::Shcomp),
            Ok(recs.clone())
        );
        // Torn: cut in the middle of the last record's line.
        let torn = &text[..text.len() - 3];
        assert_eq!(
            parse_journal(torn.as_bytes(), FileKind::Shcomp).unwrap(),
            recs[..5]
        );
        // Torn between a tx and its inverse.
        let tx_only = format!("{JOURNAL_HEADER}{}", encode_records(&recs[..3]));
        let cut = tx_only.rfind("\ninverse").unwrap() + 1;
        assert_eq!(
            parse_journal(&tx_only.as_bytes()[..cut], FileKind::Shcomp).unwrap(),
            recs[..2]
        );
        assert_eq!(
            parse_journal(b"schemify 1 lock\n", FileKind::Shcomp),
            Err(Code::E028)
        );
    }

    #[test]
    fn analysis_rebuilds_stacks_at_the_matching_checkpoint() {
        let e = |seq| Entry {
            seq,
            unix_ms: 0,
            label: String::new(),
            forward: vec![],
            inverse: vec![],
        };
        let recs = vec![
            JRec::Base("h0".into()),
            JRec::Tx(e(1)),
            JRec::Tx(e(2)),
            JRec::Undo(2),
            JRec::Save(2, "h1".into()),
            JRec::Tx(e(3)),
        ];
        let a = analyze(recs.clone(), "h1", 16);
        assert!(a.matched);
        assert_eq!(a.undo.iter().map(|e| e.seq).collect::<Vec<_>>(), [1]);
        assert_eq!(a.redo.iter().map(|e| e.seq).collect::<Vec<_>>(), [2]);
        assert_eq!(a.tail, [JRec::Tx(e(3))]);
        assert_eq!(a.next_seq, 4);
        let diverged = analyze(recs, "other", 16);
        assert!(!diverged.matched && diverged.undo.is_empty());
        assert_eq!(diverged.tail, [JRec::Tx(e(3))]);
    }
}
