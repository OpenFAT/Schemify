//! Committed transactions ↔ the journal sidecar's records, and a parsed journal → the
//! undo/redo stacks and the unsaved tail it describes for a file of a known hash.
//! Cites: edits spec §3 (journal, checkpoints, crash recovery, torn tails).
//!
//! Records: `base <sha256>`, `hist`/`tx <seq> <unix_ms> <label>` (op lines as `|`
//! verbatim lines) each followed by `inverse <seq>` (the inverse op lines), `undo <seq>`,
//! `redo <seq>`, `save <seq> <sha256>`.
//!
//! Contents:
//! - `pub(crate) struct Entry`, `pub(crate) enum JRec`, `pub(crate) struct Analysis`
//! - `pub(crate) fn encode_records(out: &mut String, recs: &[JRec<E>])`
//! - `pub(crate) fn parse_journal(bytes: &[u8], file: FileKind) -> Result<Vec<JRec>, Code>`
//! - `pub(crate) fn analyze(recs: Vec<JRec>, file_hash: &str, undo_max: usize) -> Analysis`

use crate::Code;
use crate::ops::Op;
use crate::optext::{JOURNAL_HEADER, encode_into, lex, number, ops_from_text, push_op};
use schemify_format::{Document as FormatDoc, FileKind};
use std::borrow::Borrow;

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

/// One journal record: decoded (`E = Entry`), or to encode, borrowing its entry
/// (`E = &Entry`) so writing history copies no ops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JRec<E = Entry> {
    /// The file hash the following records apply to.
    Base(String),
    /// A transaction that is undo history at `base` (not replayed).
    Hist(E),
    /// A committed transaction.
    Tx(E),
    /// The undo stack's top (seq) was undone.
    Undo(u64),
    /// The redo stack's top (seq) was redone.
    Redo(u64),
    /// The document was saved with this hash after transaction `seq`.
    Save(u64, String),
}

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

/// Torn-tail retries when a journal does not lex (each drops the last record).
const TORN_RETRIES_MAX: u32 = 4;

/// Appends journal records (no header) to `out`.
pub(crate) fn encode_records<E: Borrow<Entry>>(out: &mut String, recs: &[JRec<E>]) {
    let mut buf = String::new();
    encode_into(out, |d| {
        for r in recs {
            match r {
                JRec::Base(h) => {
                    d.begin_unknown_record("base");
                    d.push_field(h);
                }
                JRec::Hist(e) => push_entry(d, "hist", e.borrow(), &mut buf),
                JRec::Tx(e) => push_entry(d, "tx", e.borrow(), &mut buf),
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
    });
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

// ---- private ----

/// Pushes `ops` as op text, one verbatim line per text line; `buf` is scratch.
fn push_ops(d: &mut FormatDoc<'static>, ops: &[Op], buf: &mut String) {
    buf.clear();
    encode_into(buf, |o| ops.iter().for_each(|op| push_op(o, op)));
    for line in buf.lines() {
        d.push_verbatim(line);
    }
}

fn push_entry(d: &mut FormatDoc<'static>, word: &str, e: &Entry, buf: &mut String) {
    assert_eq!(e.forward.len(), e.inverse.len(), "one inverse per op");
    assert!(word == "tx" || word == "hist");
    d.begin_unknown_record(word);
    d.push_field(&e.seq.to_string());
    d.push_field(&e.unix_ms.to_string());
    d.push_field(&e.label);
    push_ops(d, &e.forward, buf);
    d.begin_unknown_record("inverse");
    d.push_field(&e.seq.to_string());
    push_ops(d, &e.inverse, buf);
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
            ("undo", [s]) => number(s).map(JRec::Undo),
            ("redo", [s]) => number(s).map(JRec::Redo),
            ("save", [s, h]) => number(s).map(|s| JRec::Save(s, (*h).to_owned())),
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
        seq: number(s)?,
        unix_ms: number(ms)?,
        label: label.to_owned(),
        forward: ops(r)?,
        inverse: ops(inv)?,
    };
    (entry.forward.len() == entry.inverse.len()).then_some(entry)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optext::tests::sample_ops;

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
        let mut text = JOURNAL_HEADER.to_owned();
        encode_records(&mut text, &recs);
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
        let mut tx_only = JOURNAL_HEADER.to_owned();
        encode_records(&mut tx_only, &recs[..3]);
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
