//! The write side of the record model: rows pushed by callers (the `doc` crate, merge,
//! the reader's lexer) → appended [`Document`] tables (spec §2 records, §5 unknowns).
//!
//! Rows are only appended: no index handed out is ever invalidated.
//!
//! Contents:
//! - `Document::{new, begin_record, begin_unknown_record, push_field, push_field_coord,
//!   push_attr, push_value, push_value_coord, push_verbatim, push_comment,
//!   push_eof_comment, copy_record}`
//! - crate: `Document::{bare, begin, begin_like, copy_record_any, copy_fields, copy_attr,
//!   copy_comments, reserve_for, intern, push_field_str, push_attr_str, push_value_str,
//!   push_verbatim_str, push_comment_str}`
//! - `pub(crate) fn push_coord(arena: &mut String, milli: i32) -> Str`

use crate::document::{Document, NONE, Str, VERSION, is_word};
use crate::scalar::format_coord;
use crate::schema::{FileKind, RecordKind};

/// Appends the shortest decimal for `milli` to `arena`; returns its span.
///
/// # Panics
///
/// Panics when the arena would pass 2 GiB (bit 31 of a span offset marks the arena).
pub(crate) fn push_coord(arena: &mut String, milli: i32) -> Str {
    let off = arena.len();
    format_coord(milli, arena);
    assert!(arena.len() > off, "a coordinate has at least one digit");
    Str::arena_tail(arena, off)
}

impl Document<'static> {
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
}

impl<'s> Document<'s> {
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
    /// Coordinates belong in [`Document::push_field_coord`]: text is written as given,
    /// and the reader rewrites coordinates to their shortest form, so `1.50` pushed as
    /// text would not round-trip (and [`Document::field_coords`] reads 0 for it).
    ///
    /// # Panics
    ///
    /// Panics if there is no record yet (never true after [`Document::new`]).
    pub fn push_field(&mut self, text: &str) {
        let s = self.intern(text);
        self.push_field_str(s);
    }

    /// Appends a coordinate in milli-units, formatted as the shortest decimal.
    pub fn push_field_coord(&mut self, milli: i32) {
        let s = push_coord(&mut self.arena, milli);
        self.push_field_str(s);
        *self.field_milli.last_mut().expect("pushed above") = milli;
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
        let s = push_coord(&mut self.arena, milli);
        self.push_value_str(s);
        *self.value_milli.last_mut().expect("pushed above") = milli;
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

    /// Creates a document with no records (not even the header) over `src`.
    pub(crate) fn bare(src: &'s str, version: u16, kind: FileKind) -> Document<'s> {
        assert!(src.len() < 1 << 31, "spans address 2^31 source bytes");
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
            field_milli: Vec::new(),
            attr_key: Vec::new(),
            attr_line: Vec::new(),
            attr_value_start: Vec::new(),
            attr_values: Vec::new(),
            value_milli: Vec::new(),
            verbatim: Vec::new(),
            comments: Vec::new(),
            eof_comments: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Appends a record row of `kind` read from source line `line` (0: built).
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

    /// As [`Document::copy_record`], for any kind (merge copies headers' peers too).
    pub(crate) fn copy_record_any(&mut self, from: &Document<'_>, r: u32) -> u32 {
        let new = self.begin_like(from, r);
        self.copy_comments(from, r);
        self.copy_fields(from, r);
        for a in from.attrs(r) {
            self.copy_attr(from, a);
        }
        for v in from.verbatim(r) {
            self.push_verbatim(v);
        }
        assert_eq!(self.attrs(new).len(), from.attrs(r).len());
        assert_eq!(self.verbatim(new).len(), from.verbatim(r).len());
        new
    }

    /// Appends record `r` of `from`'s fields to the current record.
    pub(crate) fn copy_fields(&mut self, from: &Document<'_>, r: u32) {
        let before = self.fields.len();
        for f in from.fields(r) {
            self.push_field(f);
        }
        assert_eq!(self.fields.len() - before, from.fields(r).len());
    }

    /// Appends attribute `a` of `from` (key and values) to the current record.
    pub(crate) fn copy_attr(&mut self, from: &Document<'_>, a: u32) {
        let k = self.intern(from.attr_key(a));
        let new = self.push_attr_str(k, 0);
        for v in from.attr_values(a) {
            self.push_value(v);
        }
        assert_eq!(self.attr_values(new).len(), from.attr_values(a).len());
    }

    /// Appends record `r` of `from`'s comments to the current record.
    pub(crate) fn copy_comments(&mut self, from: &Document<'_>, r: u32) {
        let before = self.comments.len();
        for c in from.comments(r) {
            let s = self.intern(c);
            self.push_comment_str(s);
        }
        assert_eq!(self.comments.len() - before, from.comments(r).len());
    }

    /// Reserves the columns for a file of `bytes` bytes, from the measured shape of cell
    /// files (a record per ~40 bytes, a value per ~10), so lexing a large file grows nothing.
    pub(crate) fn reserve_for(&mut self, bytes: usize) {
        let (records, fields, attrs, values) = (bytes / 40, bytes / 10, bytes / 24, bytes / 16);
        for c in [
            &mut self.rec_line,
            &mut self.rec_field_start,
            &mut self.rec_attr_start,
            &mut self.rec_verbatim_start,
            &mut self.rec_comment_start,
        ] {
            c.reserve(records);
        }
        for c in [&mut self.attr_line, &mut self.attr_value_start] {
            c.reserve(attrs);
        }
        self.rec_kind.reserve(records);
        self.fields.reserve(fields);
        self.attr_key.reserve(attrs);
        self.attr_values.reserve(values);
    }

    /// Copies `text` into the arena; returns its span.
    pub(crate) fn intern(&mut self, text: &str) -> Str {
        let off = self.arena.len();
        self.arena.push_str(text);
        let s = Str::arena_tail(&self.arena, off); // asserts the arena is under 2 GiB
        assert_eq!(s.len as usize, text.len());
        s
    }

    /// Appends a field span (coordinate column 0) to the current record.
    pub(crate) fn push_field_str(&mut self, s: Str) {
        assert!(!self.rec_kind.is_empty(), "push_field before any record");
        assert!(self.fields.len() < NONE as usize - 1, "field table full");
        self.fields.push(s);
        self.field_milli.push(0);
    }

    /// Appends an attribute row with key `key` from source line `line` (0: built).
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

    /// Appends a value span (coordinate column 0) to the last attribute.
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
        self.value_milli.push(0);
    }

    /// Appends a verbatim line span to the current record.
    pub(crate) fn push_verbatim_str(&mut self, s: Str) {
        assert!(!self.rec_kind.is_empty(), "push_verbatim before any record");
        assert!(
            self.verbatim.len() < NONE as usize - 1,
            "verbatim table full"
        );
        self.verbatim.push(s);
    }

    /// Appends a comment span to the current record, which is not the header.
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

// ---- private ----

fn assert_comment(text: &str) {
    assert!(
        !text.contains(['\n', '\r']),
        "comment contains a line break"
    );
    assert!(!text.ends_with([' ', '\t']), "comment ends in whitespace");
}
