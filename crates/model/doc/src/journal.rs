//! Committed transactions ↔ the journal sidecar's records, and a parsed journal → the
//! undo/redo stacks and the unsaved tail it describes for a file of a known hash.
//! Cites: edits spec §3 (journal, checkpoints, crash recovery, torn tails).
//!
//! Records: `base <sha256>`, `hist`/`tx <seq> <unix_ms> <label>` (op lines as `|`
//! verbatim lines) each followed by `inverse <seq>` (the inverse op lines), `undo <seq>`,
//! `redo <seq>`, `save <seq> <sha256>`.
//!
//! Contents:
//! - `pub(crate) struct Entry`, `pub(crate) enum JournalRecord`, `pub(crate) struct Analysis`
//! - `pub(crate) fn encode_records(out: &mut String, recs: &[JournalRecord<E>])`
//! - `pub(crate) fn parse_journal(bytes: &[u8], file: FileKind) -> Result<Vec<JournalRecord>, Code>`
//! - `pub(crate) fn analyze(recs: Vec<JournalRecord>, file_hash: &str, undo_max: usize) -> Analysis`

use crate::Code;
use crate::ops::Op;
use crate::optext::{self, JOURNAL_HEADER};
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
pub(crate) enum JournalRecord<E = Entry> {
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
    /// The document was saved with hash `hash` after transaction `seq`.
    Save {
        /// The last transaction the saved bytes hold.
        seq: u64,
        /// sha256 hex of the saved bytes.
        hash: String,
    },
}

/// What a journal says about a file whose current hash is known (edits spec §3).
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    /// Undo stack at the checkpoint, oldest first.
    pub(crate) undo: Vec<Entry>,
    /// Redo stack at the checkpoint, top last.
    pub(crate) redo: Vec<Entry>,
    /// Records after the checkpoint: unsaved edits to replay (`Tx`, `Undo`, `Redo`).
    pub(crate) tail: Vec<JournalRecord>,
    /// A checkpoint (`base`/`save`) matched the file hash. Otherwise the file changed
    /// outside Schemify; history is dropped and the tail is replayed best-effort.
    pub(crate) matched: bool,
    /// Next transaction sequence number.
    pub(crate) next_seq: u64,
}

/// Torn-tail retries when a journal does not lex (each drops the last record).
const TORN_RETRIES_MAX: u32 = 4;

/// Appends journal records (no header) to `out`.
pub(crate) fn encode_records<E: Borrow<Entry>>(out: &mut String, recs: &[JournalRecord<E>]) {
    let before = out.len();
    let mut buf = String::new();
    optext::encode_into(out, |d| {
        for r in recs {
            match r {
                JournalRecord::Base(h) => {
                    d.begin_unknown_record("base");
                    d.push_field(h);
                }
                JournalRecord::Hist(e) => push_entry(d, "hist", e.borrow(), &mut buf),
                JournalRecord::Tx(e) => push_entry(d, "tx", e.borrow(), &mut buf),
                JournalRecord::Undo(seq) => {
                    d.begin_unknown_record("undo");
                    d.push_field(&seq.to_string());
                }
                JournalRecord::Redo(seq) => {
                    d.begin_unknown_record("redo");
                    d.push_field(&seq.to_string());
                }
                JournalRecord::Save { seq, hash } => {
                    d.begin_unknown_record("save");
                    d.push_field(&seq.to_string());
                    d.push_field(hash);
                }
            }
        }
    });
    assert!(
        recs.is_empty() || out.len() > before,
        "every record writes a line"
    );
}

/// Parses a journal file. A torn tail (a crash mid-append) is dropped: the text is cut
/// before the record holding the first lexical error and re-lexed.
///
/// # Errors
///
/// `E028` when line 1 is not `schemify 1 journal`.
pub(crate) fn parse_journal(bytes: &[u8], file: FileKind) -> Result<Vec<JournalRecord>, Code> {
    let text = String::from_utf8_lossy(bytes);
    let body = text.strip_prefix(JOURNAL_HEADER).ok_or(Code::E028)?;
    let mut body: &str = body;
    for _ in 0..TORN_RETRIES_MAX {
        match optext::lex(body, |d| decode_records(d, file)) {
            Ok(recs) => return Ok(recs),
            Err(errors) => {
                let line = errors.iter().map(|e| e.line).min().unwrap_or(1);
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
pub(crate) fn analyze(recs: Vec<JournalRecord>, file_hash: &str, undo_max: usize) -> Analysis {
    assert!(undo_max > 0, "an undo stack holds at least one entry");
    let is_checkpoint =
        |r: &JournalRecord| matches!(r, JournalRecord::Base(_) | JournalRecord::Save { .. });
    let matches_file = |r: &JournalRecord| match r {
        JournalRecord::Base(hash) | JournalRecord::Save { hash, .. } => hash == file_hash,
        JournalRecord::Hist(_)
        | JournalRecord::Tx(_)
        | JournalRecord::Undo(_)
        | JournalRecord::Redo(_) => false,
    };
    let matching = recs.iter().rposition(matches_file);
    let checkpoint = matching.or_else(|| recs.iter().rposition(is_checkpoint));
    let next_seq = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Tx(e) | JournalRecord::Hist(e) => Some(e.seq),
            JournalRecord::Save { seq, .. } => Some(*seq),
            JournalRecord::Base(_) | JournalRecord::Undo(_) | JournalRecord::Redo(_) => None,
        })
        .max()
        .map_or(1, |seq| seq + 1);
    let mut a = Analysis {
        matched: matching.is_some(),
        next_seq,
        ..Analysis::default()
    };
    let mut split = checkpoint.map_or(0, |i| i + 1);
    // `hist` records written after a `base` are the history *at* that base.
    while recs
        .get(split)
        .is_some_and(|r| matches!(r, JournalRecord::Hist(_)))
    {
        split += 1;
    }
    let mut recs = recs;
    a.tail = recs.split_off(split);
    a.tail.retain(|r| {
        matches!(
            r,
            JournalRecord::Tx(_) | JournalRecord::Undo(_) | JournalRecord::Redo(_)
        )
    });
    if a.matched {
        for r in recs {
            match r {
                JournalRecord::Hist(e) | JournalRecord::Tx(e) => {
                    a.redo.clear(); // a commit clears redo; history entries have none
                    a.undo.push(e);
                    if a.undo.len() > undo_max {
                        a.undo.remove(0);
                    }
                }
                JournalRecord::Undo(seq) => {
                    if let Some(e) = a.undo.pop_if(|e| e.seq == seq) {
                        a.redo.push(e);
                    }
                }
                JournalRecord::Redo(seq) => {
                    if let Some(e) = a.redo.pop_if(|e| e.seq == seq) {
                        a.undo.push(e);
                    }
                }
                JournalRecord::Base(_) | JournalRecord::Save { .. } => {}
            }
        }
    }
    assert!(a.undo.len() <= undo_max);
    assert!(
        a.undo.iter().chain(&a.redo).all(|e| e.seq < a.next_seq),
        "the next sequence number is past every entry"
    );
    a
}

// ---- private ----

/// Pushes `ops` as op text, one verbatim line per text line; `buf` is scratch.
fn push_ops(d: &mut FormatDoc<'static>, ops: &[Op], buf: &mut String) {
    buf.clear();
    optext::encode_into(buf, |o| {
        for op in ops {
            optext::push_op(o, op);
        }
    });
    assert!(ops.is_empty() || !buf.is_empty(), "every op writes a line");
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
fn decode_records(d: &FormatDoc<'_>, file: FileKind) -> Vec<JournalRecord> {
    let mut out = Vec::new();
    let mut r = 1;
    while r < d.record_count() {
        let f: Vec<&str> = d.fields(r).collect();
        let rec = match (d.record_word(r), f.as_slice()) {
            ("base", [h]) => Some(JournalRecord::Base((*h).to_owned())),
            ("undo", [seq]) => optext::number(seq).map(JournalRecord::Undo),
            ("redo", [seq]) => optext::number(seq).map(JournalRecord::Redo),
            ("save", [seq, hash]) => optext::number(seq).map(|seq| JournalRecord::Save {
                seq,
                hash: (*hash).to_owned(),
            }),
            (w @ ("tx" | "hist"), [seq, unix_ms, label]) => {
                let entry = decode_entry(d, r, file, seq, unix_ms, label);
                r += 1; // the `inverse` record
                entry.map(|e| {
                    if w == "tx" {
                        JournalRecord::Tx(e)
                    } else {
                        JournalRecord::Hist(e)
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
    seq: &str,
    unix_ms: &str,
    label: &str,
) -> Option<Entry> {
    assert!(r < d.record_count());
    let inverse = r + 1;
    if inverse >= d.record_count()
        || d.record_word(inverse) != "inverse"
        || d.field(inverse, 0) != Some(seq)
    {
        return None;
    }
    let ops = |rec: u32| -> Option<Vec<Op>> {
        let text: String = d.verbatim(rec).flat_map(|l| [l, "\n"]).collect();
        optext::ops_from_text(&text, file).ok()
    };
    let entry = Entry {
        seq: optext::number(seq)?,
        unix_ms: optext::number(unix_ms)?,
        label: label.to_owned(),
        forward: ops(r)?,
        inverse: ops(inverse)?,
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
            JournalRecord::Base("ab".into()),
            JournalRecord::Hist(e.clone()),
            JournalRecord::Tx(e.clone()),
            JournalRecord::Undo(7),
            JournalRecord::Redo(7),
            JournalRecord::Save {
                seq: 7,
                hash: "cd".into(),
            },
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
            JournalRecord::Base("h0".into()),
            JournalRecord::Tx(e(1)),
            JournalRecord::Tx(e(2)),
            JournalRecord::Undo(2),
            JournalRecord::Save {
                seq: 2,
                hash: "h1".into(),
            },
            JournalRecord::Tx(e(3)),
        ];
        let a = analyze(recs.clone(), "h1", 16);
        assert!(a.matched);
        assert_eq!(a.undo.iter().map(|e| e.seq).collect::<Vec<_>>(), [1]);
        assert_eq!(a.redo.iter().map(|e| e.seq).collect::<Vec<_>>(), [2]);
        assert_eq!(a.tail, [JournalRecord::Tx(e(3))]);
        assert_eq!(a.next_seq, 4);
        let diverged = analyze(recs, "other", 16);
        assert!(!diverged.matched && diverged.undo.is_empty());
        assert_eq!(diverged.tail, [JournalRecord::Tx(e(3))]);
    }
}
