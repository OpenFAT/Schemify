//! Canonical writer: [`Document`] → bytes (spec §2 "Writer canonical form", §4 ordering,
//! §8 conflict output).
//!
//! Spine: `emission_order` (stable sort by rank, then by (kind, key) inside sorted
//! ranks) → `write_record` per record, with blank lines between kinds and optional
//! merge-conflict markers around record spans.
//!
//! Contents:
//! - `pub fn write(doc: &Document<'_>) -> String`
//! - `pub fn write_into(doc: &Document<'_>, out: &mut String)`
//! - `pub(crate) struct ConflictSpan`
//! - `pub(crate) fn write_spans(doc, spans: &[ConflictSpan], eof_split: Option<u32>, out)`

use crate::document::{Document, NONE};
use crate::lex;
use crate::schema::{RANK_MAX, RecordKind};

/// Records `first .. first + ours` are our side of a conflict and the next `theirs`
/// records are theirs; the writer wraps them in conflict markers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConflictSpan {
    pub(crate) first: u32,
    pub(crate) ours: u32,
    pub(crate) theirs: u32,
}

/// Opening merge-conflict marker (git's 7-character form).
const MARK_OURS: &str = "<<<<<<< ours\n";
/// Separator between the two sides of a conflict.
const MARK_SPLIT: &str = "=======\n";
/// Closing merge-conflict marker.
const MARK_THEIRS: &str = ">>>>>>> theirs\n";

/// Writes `doc` in canonical form: rank order; rank-3 and rank-6 kinds in listed order,
/// each sorted bytewise by key; other ranks in document order; one blank line between
/// record kinds; attributes indented two spaces, known before unknown; values quoted
/// only when not a legal bare value; LF line ends and a final newline.
///
/// Deterministic: equal documents (see [`Document`]'s `PartialEq`) give equal bytes.
/// Cost: O(n) for a document already in canonical order, O(n log n) otherwise.
///
/// # Panics
///
/// Panics if `doc` lacks its header record (never true for a parsed or built one).
///
/// # Examples
///
/// ```
/// use schemify_format as format;
/// use schemify_format::{Document, FileKind, RecordKind};
/// let mut doc = Document::new(FileKind::Config);
/// doc.begin_record(RecordKind::Set);
/// doc.push_field("title");
/// doc.push_field("my bench");
/// assert_eq!(format::write(&doc), "schemify 1 config\n\nset title \"my bench\"\n");
/// ```
#[must_use]
pub fn write(doc: &Document<'_>) -> String {
    let mut out = String::with_capacity(doc.arena.len() + doc.src.len() + 64);
    write_into(doc, &mut out);
    assert!(out.starts_with("schemify "), "the header is written first");
    assert!(out.ends_with('\n'));
    out
}

/// Appends the canonical form of `doc` to `out` (see [`write()`]), so a caller writing many
/// records (the doc journal) reuses one buffer.
///
/// # Panics
///
/// As [`write()`].
pub fn write_into(doc: &Document<'_>, out: &mut String) {
    let before = out.len();
    write_spans(doc, &[], None, out);
    assert!(out.len() > before);
}

/// Writes `doc` with conflict markers around `spans` and, when `eof_split` is set,
/// around the end-of-file comments (`..split` ours, `split..` theirs).
pub(crate) fn write_spans(
    doc: &Document<'_>,
    spans: &[ConflictSpan],
    eof_split: Option<u32>,
    out: &mut String,
) {
    assert!(doc.record_count() >= 1, "document lacks a header");
    assert_eq!(
        doc.record_kind(0),
        RecordKind::Header,
        "document lacks a header"
    );
    let start_len = out.len();
    let order = emission_order(doc);
    let mut span_of = vec![NONE; doc.record_count() as usize];
    for (i, s) in spans.iter().enumerate() {
        assert!(s.ours + s.theirs > 0, "empty conflict span");
        for r in s.first..s.first + s.ours + s.theirs {
            assert_eq!(span_of[r as usize], NONE, "overlapping conflict spans");
            span_of[r as usize] = i as u32;
        }
    }
    let mut prev: Option<&str> = None;
    for (pos, &r) in order.iter().enumerate() {
        let word = doc.record_word(r);
        let si = span_of[r as usize];
        let span = (si != NONE).then(|| spans[si as usize]);
        let kind_changed = prev.is_some_and(|p| p != word);
        match span {
            Some(s) if r == s.first => {
                let len = (s.ours + s.theirs) as usize;
                assert!(
                    order[pos..]
                        .iter()
                        .take(len)
                        .copied()
                        .eq(s.first..s.first + len as u32),
                    "conflict span is not contiguous in canonical order"
                );
                if kind_changed {
                    out.push('\n');
                }
                out.push_str(MARK_OURS);
                if s.ours == 0 {
                    out.push_str(MARK_SPLIT);
                }
            }
            _ if kind_changed => out.push('\n'),
            _ => {}
        }
        write_record(doc, r, out);
        prev = Some(word);
        if let Some(s) = span {
            if s.ours > 0 && r == s.first + s.ours - 1 {
                out.push_str(MARK_SPLIT);
                prev = None;
            }
            if r == s.first + s.ours + s.theirs - 1 {
                out.push_str(MARK_THEIRS);
                prev = Some(word);
            }
        }
    }
    write_eof(doc, eof_split, out);
    assert!(out.len() > start_len);
    assert!(out.ends_with('\n'), "the output ends with a newline");
}

// ---- private ----

/// Returns record indices in canonical order. Stable, so ties keep document order.
fn emission_order(doc: &Document<'_>) -> Vec<u32> {
    let ranks = doc.ranks();
    let sorted: [bool; RANK_MAX as usize + 1] =
        std::array::from_fn(|r| doc.kind.rank_sorted(r as u8));
    let key = |r: u32| {
        let rank = ranks[r as usize];
        let kind = doc.record_kind(r);
        if !sorted[rank as usize] {
            return (rank, 0u8, 0u8, "", "");
        }
        if kind == RecordKind::Unknown {
            return (rank, 1, 0, "", ""); // after the known kinds, in file order
        }
        let (k0, k1) = doc.record_key(r);
        (rank, 0, kind as u8, k0, k1)
    };
    let mut order: Vec<u32> = (0..doc.record_count()).collect();
    // Stable, and each key is computed once instead of twice per comparison.
    order.sort_by_cached_key(|&r| key(r));
    assert_eq!(order.len(), doc.record_count() as usize);
    assert_eq!(order.first(), Some(&0), "the header is written first");
    order
}

/// Appends record `r`: its comments, the record line, attributes (known before
/// unknown), verbatim lines.
fn write_record(doc: &Document<'_>, r: u32, out: &mut String) {
    assert!(r < doc.record_count());
    let before = out.len();
    for c in doc.comments(r) {
        push_comment(c, out);
    }
    out.push_str(doc.record_word(r));
    let fields = doc.field_spans(r);
    let reversed = doc.wire_reversed(r);
    for i in 0..fields.len() {
        // A reversed wire `id x1 y1 x2 y2` is written `id x2 y2 x1 y1`.
        let j = if reversed && i > 0 {
            (i + 1) % 4 + 1
        } else {
            i
        };
        out.push(' ');
        push_value(doc.text(fields[j]), out);
    }
    out.push('\n');
    let kind = doc.record_kind(r);
    let schema = kind.schema();
    for known in [true, false] {
        for a in doc.attrs(r) {
            let key = doc.attr_key(a);
            if schema.knows_attr(key) != known {
                continue;
            }
            out.push_str("  ");
            out.push_str(key);
            for v in doc.attr_values(a) {
                out.push(' ');
                push_value(v, out);
            }
            out.push('\n');
        }
    }
    for v in doc.verbatim(r) {
        out.push_str("  |");
        if !v.is_empty() {
            out.push(' ');
            out.push_str(v);
        }
        out.push('\n');
    }
    assert!(out.len() > before && out.ends_with('\n'));
}

/// Appends the end-of-file comments after a blank line; with `eof_split`, as a
/// conflict of `..split` (ours) and `split..` (theirs).
fn write_eof(doc: &Document<'_>, eof_split: Option<u32>, out: &mut String) {
    let eof: Vec<&str> = doc.eof_comments().collect();
    if eof.is_empty() {
        return;
    }
    out.push('\n');
    let Some(split) = eof_split.map(|s| s as usize) else {
        for c in &eof {
            push_comment(c, out);
        }
        return;
    };
    assert!(split <= eof.len(), "the split lies inside the comments");
    out.push_str(MARK_OURS);
    for c in &eof[..split] {
        push_comment(c, out);
    }
    out.push_str(MARK_SPLIT);
    for c in &eof[split..] {
        push_comment(c, out);
    }
    out.push_str(MARK_THEIRS);
    assert!(out.ends_with(MARK_THEIRS));
}

/// Appends `#text` as one line.
fn push_comment(text: &str, out: &mut String) {
    assert!(!text.contains(['\n', '\r']), "a comment is one line");
    assert!(
        !text.ends_with([' ', '\t']),
        "the writer emits no trailing whitespace"
    );
    out.push('#');
    out.push_str(text);
    out.push('\n');
}

/// Appends `v` bare when legal, else quoted with the canonical escapes: `\"`, `\\`,
/// `\n`, `\t`, and `\u{hex}` (lowercase, no leading zeros) for CR, other C0 controls
/// and DEL.
fn push_value(v: &str, out: &mut String) {
    use std::fmt::Write as _;

    let before = out.len();
    if !needs_quotes(v) {
        out.push_str(v);
        assert!(out.len() > before, "a bare value is never empty");
        return;
    }
    out.push('"');
    let mut run = 0;
    for (i, c) in v.char_indices() {
        let escape = match c {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\t' => "\\t",
            c if (c as u32) < 0x20 || c as u32 == 0x7f => "",
            _ => continue,
        };
        out.push_str(&v[run..i]);
        if escape.is_empty() {
            write!(out, "\\u{{{:x}}}", c as u32).expect("writing to a String cannot fail");
        } else {
            out.push_str(escape);
        }
        run = i + c.len_utf8();
    }
    out.push_str(&v[run..]);
    out.push('"');
    assert!(
        out.len() >= before + 2 + v.len(),
        "escaping never shortens a value"
    );
}

/// Returns whether `v` must be quoted: empty, starting with `#` or `|`, or holding a
/// byte that is not a bare-value byte.
fn needs_quotes(v: &str) -> bool {
    let b = v.as_bytes();
    b.is_empty() || b[0] == b'#' || b[0] == b'|' || !b.iter().all(|&c| lex::is_bare_byte(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(v: &str) -> String {
        let mut s = String::new();
        push_value(v, &mut s);
        s
    }

    #[test]
    fn values_quote_only_when_needed() {
        assert_eq!(quoted("nfet_01v8"), "nfet_01v8");
        assert_eq!(quoted("{name}"), "{name}");
        assert_eq!(quoted("+"), "+");
        assert_eq!(quoted("µ"), "µ");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("#1"), "\"#1\"");
        assert_eq!(quoted("|x"), "\"|x\"");
        assert_eq!(quoted("a#b"), "a#b");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("say \"hi\"\\"), "\"say \\\"hi\\\"\\\\\"");
        assert_eq!(
            quoted("l1\nl2\tx\r\u{0}\u{7f}"),
            "\"l1\\nl2\\tx\\u{d}\\u{0}\\u{7f}\""
        );
    }
}
