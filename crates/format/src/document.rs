//! The record model: one parsed or built file as flat tables.
//!
//! Layout (data-oriented-design six questions):
//! 1. In: file bytes, or rows pushed by the `doc` crate. Out: canonical bytes, merges, diffs.
//! 2. One table row per record (≤ 2^22 objects per cell, ARCHITECTURE), attribute line,
//!    value, verbatim line and comment.
//! 3. Every row index and text offset is `u32`: a file is capped at 2^30 bytes (`E021`)
//!    and every row consumes at least one byte of it. Text offsets use bit 31 to tell the
//!    owned arena from the borrowed source. Record kinds are `u8`.
//! 4. Access is sequential by record, then by its children; children are CSR ranges
//!    (`*_start[i]..*_start[i + 1]`) into shared pools, so one record's data is contiguous.
//! 5. Lifetime: the document. Values without escapes borrow the source text (zero copy);
//!    unescaped and pushed text lives in one owned arena.
//! 6. Lines are self-describing, so parsing could split at `LF` + column-0; not done
//!    (`ponytail:` 100k objects parse well inside the D39 budget on one thread).

use crate::diag::Diagnostic;
use crate::scalar::{format_coord, parse_coord};
use crate::schema::{FileKind, KeyRule, RANK_MAX, RecordKind};
use std::fmt;
use std::ops::Range;

/// Current file format version; a file with a larger version is read in forward-compatible mode.
pub const VERSION: u16 = 1;

/// Sentinel record/attribute index meaning "absent".
pub const NONE: u32 = u32::MAX;

/// Bit 31 of [`Str::off`]: the text lives in the owned arena, not the source.
const ARENA: u32 = 1 << 31;

/// A span of document text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Str {
    pub(crate) off: u32,
    pub(crate) len: u32,
}

impl Str {
    /// Returns the source byte offset, or `None` for arena text.
    pub(crate) fn src_offset(self) -> Option<usize> {
        (self.off & ARENA == 0).then_some(self.off as usize)
    }
}

/// One file as flat tables of records, attribute lines, values, verbatim lines and comments.
///
/// Record 0 is always the header (`schemify <version> <kind>` plus header attributes);
/// its two fields are the version and kind words. Rows are only appended, so every
/// index handed out stays valid for the document's lifetime.
#[derive(Clone)]
pub struct Document<'s> {
    pub(crate) src: &'s str,
    pub(crate) arena: String,
    pub(crate) version: u16,
    pub(crate) kind: FileKind,
    pub(crate) rec_kind: Vec<RecordKind>,
    pub(crate) rec_line: Vec<u32>,
    pub(crate) rec_field_start: Vec<u32>,
    pub(crate) rec_attr_start: Vec<u32>,
    pub(crate) rec_verbatim_start: Vec<u32>,
    pub(crate) rec_comment_start: Vec<u32>,
    /// (record, word) for every `Unknown` record, in record order.
    pub(crate) unknown_words: Vec<(u32, Str)>,
    pub(crate) fields: Vec<Str>,
    pub(crate) attr_key: Vec<Str>,
    pub(crate) attr_line: Vec<u32>,
    pub(crate) attr_value_start: Vec<u32>,
    pub(crate) attr_values: Vec<Str>,
    pub(crate) verbatim: Vec<Str>,
    pub(crate) comments: Vec<Str>,
    pub(crate) eof_comments: Vec<Str>,
    pub(crate) warnings: Vec<Diagnostic>,
}

/// Iterator over a run of document texts (fields, values, verbatim lines, comments).
#[derive(Clone)]
pub struct Texts<'d> {
    src: &'d str,
    arena: &'d str,
    spans: std::slice::Iter<'d, Str>,
}

impl<'d> Iterator for Texts<'d> {
    type Item = &'d str;

    fn next(&mut self) -> Option<&'d str> {
        let s = self.spans.next()?;
        Some(resolve(self.src, self.arena, *s))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.spans.size_hint()
    }
}

impl ExactSizeIterator for Texts<'_> {}

fn resolve<'d>(src: &'d str, arena: &'d str, s: Str) -> &'d str {
    let (base, off) = if s.off & ARENA != 0 {
        (arena, s.off & !ARENA)
    } else {
        (src, s.off)
    };
    let end = off as usize + s.len as usize;
    assert!(end <= base.len(), "span past the end of its text");
    &base[off as usize..end]
}

/// Returns `starts[i]..starts[i + 1]` (or `..total` for the last row).
fn csr(starts: &[u32], i: u32, total: usize) -> Range<usize> {
    let i = i as usize;
    assert!(i < starts.len(), "row index out of range");
    let start = starts[i] as usize;
    let end = starts.get(i + 1).map_or(total, |&e| e as usize);
    assert!(start <= end && end <= total, "CSR range out of order");
    start..end
}

/// Returns whether `s` matches the spec `word` production.
pub(crate) fn is_word(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphabetic) && b[1..].iter().all(|&c| is_word_byte(c))
}

/// Returns whether `c` may continue a word.
pub(crate) fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':')
}

impl<'s> Document<'s> {
    /// Creates a document with no records (not even the header).
    pub(crate) fn bare(src: &'s str, version: u16, kind: FileKind) -> Document<'s> {
        assert!(src.len() < ARENA as usize);
        Document {
            src,
            arena: String::new(),
            version,
            kind,
            rec_kind: Vec::new(),
            rec_line: Vec::new(),
            rec_field_start: Vec::new(),
            rec_attr_start: Vec::new(),
            rec_verbatim_start: Vec::new(),
            rec_comment_start: Vec::new(),
            unknown_words: Vec::new(),
            fields: Vec::new(),
            attr_key: Vec::new(),
            attr_line: Vec::new(),
            attr_value_start: Vec::new(),
            attr_values: Vec::new(),
            verbatim: Vec::new(),
            comments: Vec::new(),
            eof_comments: Vec::new(),
            warnings: Vec::new(),
        }
    }

    // ------------------------------------------------------------ reading

    /// Returns the file kind from the header.
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// Returns the header version; larger than [`VERSION`] means unknown records were kept.
    pub fn version(&self) -> u16 {
        assert!(self.version >= 1, "version 0 is rejected by the reader");
        self.version
    }

    /// Returns the number of records, header included (so at least 1 for a parsed file).
    pub fn record_count(&self) -> u32 {
        self.assert_columns();
        self.rec_kind.len() as u32
    }

    /// Parallel columns of one table have equal lengths (the SoA invariant).
    fn assert_columns(&self) {
        let records = self.rec_kind.len();
        assert_eq!(self.rec_line.len(), records);
        assert_eq!(self.rec_field_start.len(), records);
        assert_eq!(self.rec_attr_start.len(), records);
        assert_eq!(self.rec_verbatim_start.len(), records);
        assert_eq!(self.rec_comment_start.len(), records);
        assert!(self.unknown_words.len() <= records);
        assert_eq!(self.attr_line.len(), self.attr_key.len());
        assert_eq!(self.attr_value_start.len(), self.attr_key.len());
    }

    /// Returns record `r`'s kind.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`; the same holds for every `r` parameter below.
    pub fn record_kind(&self, r: u32) -> RecordKind {
        self.rec_kind[r as usize]
    }

    /// Returns record `r`'s word (`"schemify"` for the header, the file's word for `Unknown`).
    pub fn record_word(&self, r: u32) -> &str {
        let kind = self.record_kind(r);
        if kind != RecordKind::Unknown {
            return kind.word();
        }
        let i = self
            .unknown_words
            .binary_search_by_key(&r, |e| e.0)
            .expect("unknown record has a word");
        let word = self.text(self.unknown_words[i].1);
        assert!(is_word(word), "unknown record word is a word");
        word
    }

    /// Returns the 1-based source line of record `r`; 0 for records built in memory.
    pub fn record_line(&self, r: u32) -> u32 {
        self.rec_line[r as usize]
    }

    /// Returns record `r`'s positional values (after the word).
    pub fn fields(&self, r: u32) -> Texts<'_> {
        self.texts(&self.fields[csr(&self.rec_field_start, r, self.fields.len())])
    }

    /// Returns field `i` of record `r`, or `None` past the last field.
    pub fn field(&self, r: u32, i: usize) -> Option<&str> {
        let range = csr(&self.rec_field_start, r, self.fields.len());
        self.fields[range].get(i).map(|s| self.text(*s))
    }

    /// Returns record `r`'s identity per its kind's [`KeyRule`]: `(id or name, "")`,
    /// `(type, name)` for `locked`, `("", "")` for the header and positional records.
    /// `Unknown` records are keyed by their first field.
    pub fn record_key(&self, r: u32) -> (&str, &str) {
        let first = || self.field(r, 0).unwrap_or("");
        let key = match self.record_kind(r).key() {
            KeyRule::Id | KeyRule::Name => (first(), ""),
            KeyRule::TypeName => (first(), self.field(r, 1).unwrap_or("")),
            KeyRule::Header | KeyRule::Position => ("", ""),
        };
        assert!(
            key.0.is_empty() <= key.1.is_empty(),
            "a second key part needs a first"
        );
        key
    }

    /// Returns the attribute indices of record `r`, in file order.
    pub fn attrs(&self, r: u32) -> Range<u32> {
        let range = csr(&self.rec_attr_start, r, self.attr_key.len());
        assert!(range.end < NONE as usize, "attribute index fits u32");
        range.start as u32..range.end as u32
    }

    /// Returns attribute `a`'s word.
    pub fn attr_key(&self, a: u32) -> &str {
        self.text(self.attr_key[a as usize])
    }

    /// Returns attribute `a`'s values.
    pub fn attr_values(&self, a: u32) -> Texts<'_> {
        self.texts(&self.attr_values[csr(&self.attr_value_start, a, self.attr_values.len())])
    }

    /// Returns value `i` of attribute `a`, or `None` past the last value.
    pub fn attr_value(&self, a: u32, i: usize) -> Option<&str> {
        let range = csr(&self.attr_value_start, a, self.attr_values.len());
        self.attr_values[range].get(i).map(|s| self.text(*s))
    }

    /// Returns the 1-based source line of attribute `a`; 0 when built in memory.
    pub fn attr_line(&self, a: u32) -> u32 {
        self.attr_line[a as usize]
    }

    /// Returns the first attribute of record `r` with word `key`. Cost: linear in the
    /// record's attribute count.
    pub fn find_attr(&self, r: u32, key: &str) -> Option<u32> {
        let found = self.attrs(r).find(|&a| self.attr_key(a) == key);
        assert!(found.is_none_or(|a| self.attrs(r).contains(&a)));
        found
    }

    /// Returns record `r`'s verbatim lines (text after `| `).
    pub fn verbatim(&self, r: u32) -> Texts<'_> {
        self.texts(&self.verbatim[csr(&self.rec_verbatim_start, r, self.verbatim.len())])
    }

    /// Returns the comments written before record `r` (text after `#`, trailing
    /// whitespace removed).
    pub fn comments(&self, r: u32) -> Texts<'_> {
        self.texts(&self.comments[csr(&self.rec_comment_start, r, self.comments.len())])
    }

    /// Returns the comments after the last record.
    pub fn eof_comments(&self) -> Texts<'_> {
        self.texts(&self.eof_comments)
    }

    /// Returns parse warnings (`W001` duplicate ids, later occurrence named by
    /// [`Diagnostic::record`]); the `doc` layer re-ids those records.
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    /// Returns where record `r`'s fields sit in `self.fields`.
    pub(crate) fn field_range(&self, r: u32) -> Range<usize> {
        csr(&self.rec_field_start, r, self.fields.len())
    }

    /// Returns where attribute `a`'s values sit in `self.attr_values`.
    pub(crate) fn value_range(&self, a: u32) -> Range<usize> {
        csr(&self.attr_value_start, a, self.attr_values.len())
    }

    /// Returns record `r`'s field spans (for in-crate checks that need positions).
    pub(crate) fn field_spans(&self, r: u32) -> &[Str] {
        &self.fields[self.field_range(r)]
    }

    /// Returns attribute `a`'s value spans.
    pub(crate) fn value_spans(&self, a: u32) -> &[Str] {
        &self.attr_values[self.value_range(a)]
    }

    pub(crate) fn text(&self, s: Str) -> &str {
        resolve(self.src, &self.arena, s)
    }

    fn texts<'d>(&'d self, spans: &'d [Str]) -> Texts<'d> {
        Texts {
            src: self.src,
            arena: &self.arena,
            spans: spans.iter(),
        }
    }

    /// Returns each record's rank; `Unknown` records take the rank of the closest known
    /// record before them (spec §5 amendment).
    pub(crate) fn ranks(&self) -> Vec<u8> {
        let mut current = 0;
        let ranks: Vec<u8> = self
            .rec_kind
            .iter()
            .map(|k| {
                if *k != RecordKind::Unknown {
                    current = k.rank();
                }
                current
            })
            .collect();
        assert_eq!(ranks.len(), self.rec_kind.len());
        assert!(
            ranks.iter().all(|&r| r <= RANK_MAX),
            "inherited ranks are real ranks"
        );
        ranks
    }

    /// Returns whether wire `r` is written with its endpoints swapped so that
    /// `(x1, y1) < (x2, y2)` (spec §4). `false` for anything not a well-formed wire.
    pub(crate) fn wire_reversed(&self, r: u32) -> bool {
        if self.record_kind(r) != RecordKind::Wire {
            return false;
        }
        let f = self.field_spans(r);
        if f.len() != 5 {
            return false;
        }
        let c = |i: usize| parse_coord(self.text(f[i])).ok();
        match (c(1), c(2), c(3), c(4)) {
            (Some(x1), Some(y1), Some(x2), Some(y2)) => (x2, y2) < (x1, y1),
            _ => false,
        }
    }

    // ------------------------------------------------------------ building

    /// Creates an empty document of `kind` at the current [`VERSION`], holding only the
    /// header record. Push records with [`Document::begin_record`]; push header
    /// attributes with [`Document::push_attr`] before the first record.
    pub fn new(kind: FileKind) -> Document<'static> {
        let mut doc = Document::bare("", VERSION, kind);
        doc.begin(RecordKind::Header, 0);
        doc.push_field(&VERSION.to_string());
        doc.push_field(kind.word());
        assert_eq!(doc.record_count(), 1);
        doc
    }

    /// Appends a record of a known kind and makes it current; returns its index.
    /// Fields, attributes, verbatim lines and comments pushed next belong to it.
    ///
    /// # Panics
    ///
    /// Panics if `kind` is `Header` or `Unknown`, or not allowed in this file kind.
    pub fn begin_record(&mut self, kind: RecordKind) -> u32 {
        assert!(
            kind != RecordKind::Header && kind != RecordKind::Unknown,
            "use Document::new / begin_unknown_record"
        );
        assert!(
            self.kind.records().contains(&kind),
            "{kind:?} is not allowed in a {:?} file",
            self.kind
        );
        self.begin(kind, 0)
    }

    /// Appends a record with a word this reader does not know (forward compatibility,
    /// spec §5) and makes it current.
    ///
    /// # Panics
    ///
    /// Panics if `word` is not a spec `word` or is a known word of this file kind.
    pub fn begin_unknown_record(&mut self, word: &str) -> u32 {
        assert!(is_word(word), "record word {word:?} is not a word");
        assert!(
            self.kind.lookup(word).is_none(),
            "{word:?} is a known record word"
        );
        let w = self.intern(word);
        let r = self.begin(RecordKind::Unknown, 0);
        self.unknown_words.push((r, w));
        assert_eq!(self.record_word(r), word);
        r
    }

    /// Appends a positional value to the current record.
    ///
    /// # Panics
    ///
    /// Panics if there is no record yet (never true after [`Document::new`]).
    ///
    /// Coordinates belong in [`Document::push_field_coord`]: text is written as given,
    /// and the reader rewrites coordinates to their shortest form, so `1.50` pushed as
    /// text would not round-trip.
    pub fn push_field(&mut self, text: &str) {
        let s = self.intern(text);
        self.push_field_str(s);
    }

    /// Appends a coordinate in milli-units, formatted as the shortest decimal.
    pub fn push_field_coord(&mut self, milli: i32) {
        let s = self.intern_coord(milli);
        self.push_field_str(s);
    }

    /// Appends an attribute line to the current record; returns its index. Push its
    /// values with [`Document::push_value`].
    ///
    /// # Panics
    ///
    /// Panics if `key` is not a spec `word`.
    pub fn push_attr(&mut self, key: &str) -> u32 {
        assert!(is_word(key), "attribute key {key:?} is not a word");
        let k = self.intern(key);
        self.push_attr_str(k, 0)
    }

    /// Appends a value to the last attribute of the current record.
    ///
    /// # Panics
    ///
    /// Panics if the current record has no attribute yet.
    pub fn push_value(&mut self, text: &str) {
        let s = self.intern(text);
        self.push_value_str(s);
    }

    /// Appends a coordinate value (milli-units) to the last attribute.
    pub fn push_value_coord(&mut self, milli: i32) {
        let s = self.intern_coord(milli);
        self.push_value_str(s);
    }

    /// Appends a verbatim line (written as `  | text`) to the current record.
    ///
    /// # Panics
    ///
    /// Panics if `line` contains CR or LF.
    pub fn push_verbatim(&mut self, line: &str) {
        assert!(
            !line.contains(['\n', '\r']),
            "verbatim line contains a line break"
        );
        let s = self.intern(line);
        self.push_verbatim_str(s);
    }

    /// Attaches a comment to the current record; it is written just before the record.
    /// `text` is what follows `#`.
    ///
    /// # Panics
    ///
    /// Panics if `text` contains CR or LF or ends in whitespace (the writer emits no
    /// trailing spaces, so such a comment could not round-trip), or if the current
    /// record is the header (line 1 has nothing before it).
    pub fn push_comment(&mut self, text: &str) {
        assert_comment(text);
        let s = self.intern(text);
        self.push_comment_str(s);
    }

    /// Appends a comment after the last record.
    ///
    /// # Panics
    ///
    /// As [`Document::push_comment`].
    pub fn push_eof_comment(&mut self, text: &str) {
        assert_comment(text);
        let s = self.intern(text);
        self.eof_comments.push(s);
    }

    /// Appends a copy of record `r` of `from` (word, fields, attributes, verbatim lines,
    /// comments) and returns its index. Copies text, so `from` may die afterwards.
    ///
    /// # Panics
    ///
    /// Panics if the record's kind is not allowed in this document's file kind, or if
    /// it is a header (a document has one header).
    pub fn copy_record(&mut self, from: &Document<'_>, r: u32) -> u32 {
        let kind = from.record_kind(r);
        assert!(
            kind != RecordKind::Header,
            "copy_record: a document has exactly one header"
        );
        assert!(kind == RecordKind::Unknown || self.kind.records().contains(&kind));
        self.copy_record_any(from, r)
    }

    pub(crate) fn copy_record_any(&mut self, from: &Document<'_>, r: u32) -> u32 {
        let new = self.begin_like(from, r);
        self.copy_comments(from, r);
        self.copy_fields(from, r);
        for a in from.attrs(r) {
            self.copy_attr(from, a);
        }
        self.copy_verbatim(from, r);
        assert_eq!(self.attrs(new).len(), from.attrs(r).len());
        assert_eq!(self.fields(new).len(), from.fields(r).len());
        new
    }

    pub(crate) fn begin(&mut self, kind: RecordKind, line: u32) -> u32 {
        let r = self.rec_kind.len();
        assert!(r < NONE as usize - 1, "record table full");
        self.rec_kind.push(kind);
        self.rec_line.push(line);
        self.rec_field_start.push(self.fields.len() as u32);
        self.rec_attr_start.push(self.attr_key.len() as u32);
        self.rec_verbatim_start.push(self.verbatim.len() as u32);
        self.rec_comment_start.push(self.comments.len() as u32);
        self.assert_columns();
        r as u32
    }

    /// Starts a record with the same kind and word as record `r` of `from`.
    pub(crate) fn begin_like(&mut self, from: &Document<'_>, r: u32) -> u32 {
        let kind = from.record_kind(r);
        let new = self.begin(kind, 0);
        if kind == RecordKind::Unknown {
            let w = self.intern(from.record_word(r));
            self.unknown_words.push((new, w));
        }
        assert_eq!(self.record_word(new), from.record_word(r));
        new
    }

    pub(crate) fn copy_fields(&mut self, from: &Document<'_>, r: u32) {
        let before = self.fields.len();
        for f in from.fields(r) {
            self.push_field(f);
        }
        assert_eq!(self.fields.len() - before, from.fields(r).len());
    }

    pub(crate) fn copy_attr(&mut self, from: &Document<'_>, a: u32) {
        let k = self.intern(from.attr_key(a));
        let new = self.push_attr_str(k, 0);
        for v in from.attr_values(a) {
            self.push_value(v);
        }
        assert_eq!(self.attr_values(new).len(), from.attr_values(a).len());
    }

    pub(crate) fn copy_verbatim(&mut self, from: &Document<'_>, r: u32) {
        let before = self.verbatim.len();
        for v in from.verbatim(r) {
            self.push_verbatim(v);
        }
        assert_eq!(self.verbatim.len() - before, from.verbatim(r).len());
    }

    pub(crate) fn copy_comments(&mut self, from: &Document<'_>, r: u32) {
        let before = self.comments.len();
        for c in from.comments(r) {
            let s = self.intern(c);
            self.push_comment_str(s);
        }
        assert_eq!(self.comments.len() - before, from.comments(r).len());
    }

    pub(crate) fn intern(&mut self, text: &str) -> Str {
        let off = self.arena.len();
        assert!(
            off + text.len() < ARENA as usize,
            "document text arena exceeds 2 GiB"
        );
        self.arena.push_str(text);
        assert_eq!(self.arena.len(), off + text.len());
        Str {
            off: off as u32 | ARENA,
            len: text.len() as u32,
        }
    }

    fn intern_coord(&mut self, milli: i32) -> Str {
        let off = self.arena.len();
        assert!(
            off + 16 < ARENA as usize,
            "document text arena exceeds 2 GiB"
        );
        format_coord(milli, &mut self.arena);
        assert!(
            self.arena.len() > off,
            "a coordinate has at least one digit"
        );
        Str {
            off: off as u32 | ARENA,
            len: (self.arena.len() - off) as u32,
        }
    }

    /// Starts an unescaped value in the arena; finish with [`Document::arena_span`].
    pub(crate) fn arena_len(&self) -> usize {
        self.arena.len()
    }

    pub(crate) fn arena_span(&self, start: usize) -> Str {
        assert!(start <= self.arena.len() && self.arena.len() < ARENA as usize);
        Str {
            off: start as u32 | ARENA,
            len: (self.arena.len() - start) as u32,
        }
    }

    pub(crate) fn src_span(&self, start: usize, end: usize) -> Str {
        assert!(start <= end && end <= self.src.len());
        Str {
            off: start as u32,
            len: (end - start) as u32,
        }
    }

    pub(crate) fn push_field_str(&mut self, s: Str) {
        assert!(!self.rec_kind.is_empty(), "push_field before any record");
        assert!(self.fields.len() < NONE as usize - 1, "field table full");
        self.fields.push(s);
    }

    pub(crate) fn push_attr_str(&mut self, key: Str, line: u32) -> u32 {
        assert!(!self.rec_kind.is_empty(), "push_attr before any record");
        let a = self.attr_key.len();
        assert!(a < NONE as usize - 1, "attribute table full");
        self.attr_key.push(key);
        self.attr_line.push(line);
        self.attr_value_start.push(self.attr_values.len() as u32);
        self.assert_columns();
        a as u32
    }

    pub(crate) fn push_value_str(&mut self, s: Str) {
        let first_attr = *self
            .rec_attr_start
            .last()
            .expect("push_value before any record") as usize;
        assert!(
            self.attr_key.len() > first_attr,
            "push_value: the current record has no attribute"
        );
        self.attr_values.push(s);
    }

    pub(crate) fn push_verbatim_str(&mut self, s: Str) {
        assert!(!self.rec_kind.is_empty(), "push_verbatim before any record");
        assert!(
            self.verbatim.len() < NONE as usize - 1,
            "verbatim table full"
        );
        self.verbatim.push(s);
    }

    pub(crate) fn push_comment_str(&mut self, s: Str) {
        let current = *self
            .rec_kind
            .last()
            .expect("push_comment before any record");
        assert!(
            current != RecordKind::Header,
            "the header is line 1 and cannot carry comments"
        );
        assert!(
            self.comments.len() < NONE as usize - 1,
            "comment table full"
        );
        self.comments.push(s);
    }
}

fn assert_comment(text: &str) {
    assert!(
        !text.contains(['\n', '\r']),
        "comment contains a line break"
    );
    assert!(!text.ends_with([' ', '\t']), "comment ends in whitespace");
}

/// Returns whether record `ra` of `a` and record `rb` of `b` are identical: kind, word,
/// fields, attribute lines in order, verbatim lines and comments.
pub(crate) fn record_eq(a: &Document<'_>, ra: u32, b: &Document<'_>, rb: u32) -> bool {
    a.record_kind(ra) == b.record_kind(rb)
        && a.record_word(ra) == b.record_word(rb)
        && a.fields(ra).eq(b.fields(rb))
        && attrs_eq(a, ra, b, rb)
        && a.verbatim(ra).eq(b.verbatim(rb))
        && a.comments(ra).eq(b.comments(rb))
}

fn attrs_eq(a: &Document<'_>, ra: u32, b: &Document<'_>, rb: u32) -> bool {
    let (xa, xb) = (a.attrs(ra), b.attrs(rb));
    xa.len() == xb.len()
        && xa
            .zip(xb)
            .all(|(i, j)| a.attr_key(i) == b.attr_key(j) && a.attr_values(i).eq(b.attr_values(j)))
}

/// Semantic equality: same version, kind, records in the same order with the same
/// texts, and the same end-of-file comments. Source lines and storage (borrowed vs
/// owned text) are ignored.
impl PartialEq for Document<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.version == other.version
            && self.record_count() == other.record_count()
            && (0..self.record_count()).all(|r| record_eq(self, r, other, r))
            && self.eof_comments().eq(other.eof_comments())
    }
}

/// Dumps records in document order (not canonical order), one per line, for test failures.
impl fmt::Debug for Document<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Document {:?} v{} {{", self.kind, self.version)?;
        for r in 0..self.record_count() {
            for c in self.comments(r) {
                writeln!(f, "  #{c}")?;
            }
            write!(f, "  {}", self.record_word(r))?;
            for v in self.fields(r) {
                write!(f, " {v:?}")?;
            }
            writeln!(f)?;
            for a in self.attrs(r) {
                write!(f, "    {}", self.attr_key(a))?;
                for v in self.attr_values(a) {
                    write!(f, " {v:?}")?;
                }
                writeln!(f)?;
            }
            for v in self.verbatim(r) {
                writeln!(f, "    | {v:?}")?;
            }
        }
        for c in self.eof_comments() {
            writeln!(f, "  #{c}")?;
        }
        write!(f, "}}")
    }
}
