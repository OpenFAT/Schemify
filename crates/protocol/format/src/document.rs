//! The record model: one parsed or built file as flat tables, and the read side over
//! them (spec §2 lines → records, attribute lines, values, verbatim lines, comments).
//! Rows are appended by `reader.rs` and `builder.rs`; nothing else writes them.
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
//!
//! Contents:
//! - `pub const VERSION: u16`, `pub const NONE: u32`
//! - `pub struct Document<'src>`, `pub struct Texts<'doc>`
//! - `pub(crate) struct Str`, `Str::src_offset`, `Str::src`, `Str::arena_tail`
//! - `pub(crate) struct UnknownWord`
//! - `pub fn is_word(s: &str) -> bool`, `pub(crate) fn is_word_byte(c: u8) -> bool`
//! - `pub(crate) fn resolve<'doc>(src: &'doc str, arena: &'doc str, s: Str) -> &'doc str`
//! - `pub(crate) fn record_eq(a: &Document, ra: u32, b: &Document, rb: u32) -> bool`
//! - `Document::{kind, version, record_count, record_kind, record_word, fields, field,
//!   record_key, attrs, attr_key, attr_values, attr_value, field_coords, value_coords,
//!   find_attr, verbatim, comments, eof_comments, warnings}`
//! - crate: `Document::{assert_columns, record_line, attr_line, field_range, value_range, field_spans,
//!   value_spans, text, ranks, wire_reversed}`

use crate::diag::Diagnostic;
use crate::scalar;
use crate::schema::{FileKind, KeyRule, RANK_MAX, RecordKind};
use std::fmt;
use std::ops::Range;

/// Current file format version; a file with a larger version is read in forward-compatible mode.
pub const VERSION: u16 = 1;

/// Sentinel record/attribute index meaning "absent".
pub const NONE: u32 = u32::MAX;

/// One file as flat tables of records, attribute lines, values, verbatim lines and comments.
///
/// Record 0 is always the header (`schemify <version> <kind>` plus header attributes);
/// its two fields are the version and kind words. Rows are only appended, so every
/// index handed out stays valid for the document's lifetime. A parsed document borrows
/// its source bytes for `'src`; a built one is `'static`.
#[derive(Clone)]
pub struct Document<'src> {
    pub(crate) src: &'src str,
    pub(crate) arena: String,
    pub(crate) version: u16,
    pub(crate) kind: FileKind,
    pub(crate) record_kinds: Vec<RecordKind>,
    pub(crate) record_lines: Vec<u32>,
    pub(crate) record_field_start: Vec<u32>,
    pub(crate) record_attr_start: Vec<u32>,
    pub(crate) record_verbatim_start: Vec<u32>,
    pub(crate) record_comment_start: Vec<u32>,
    /// The word of every `Unknown` record, in record order.
    pub(crate) unknown_words: Vec<UnknownWord>,
    pub(crate) fields: Vec<Str>,
    /// Milli-units of each field (D33: i32, the whole range): a `Coord` slot's value as the
    /// reader's check pass parsed it (each coordinate once) or as `push_field_coord` pushed
    /// it; 0 for every other value. The lexer pushes fields without it; the check pass
    /// then rebuilds it whole.
    pub(crate) field_milli: Vec<i32>,
    pub(crate) attr_key: Vec<Str>,
    pub(crate) attr_line: Vec<u32>,
    pub(crate) attr_value_start: Vec<u32>,
    pub(crate) attr_values: Vec<Str>,
    /// As `field_milli`, one per attribute value.
    pub(crate) value_milli: Vec<i32>,
    pub(crate) verbatim: Vec<Str>,
    pub(crate) comments: Vec<Str>,
    pub(crate) eof_comments: Vec<Str>,
    pub(crate) warnings: Vec<Diagnostic>,
}

/// Iterator over a run of document texts (fields, values, verbatim lines, comments).
/// Borrows the document, so the document cannot grow while one is alive.
#[derive(Clone)]
pub struct Texts<'doc> {
    src: &'doc str,
    arena: &'doc str,
    spans: std::slice::Iter<'doc, Str>,
}

/// A span of document text: source bytes, or arena bytes when bit 31 of `off` is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Str {
    pub(crate) off: u32,
    pub(crate) len: u32,
}

/// The word of one `Unknown` record (spec §5), which its kind cannot supply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnknownWord {
    pub(crate) record: u32,
    pub(crate) word: Str,
}

/// Bit 31 of [`Str::off`]: the text lives in the owned arena, not the source.
const ARENA: u32 = 1 << 31;

/// Returns whether `s` matches the spec `word` production: an ASCII letter, then ASCII
/// letters, digits and `_ - . :` (record words and attribute keys).
#[must_use]
pub fn is_word(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphabetic) && b[1..].iter().all(|&c| is_word_byte(c))
}

/// Returns whether `c` may continue a word.
pub(crate) fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':')
}

/// Returns the text of span `s`, split-borrowing the document's source and arena.
pub(crate) fn resolve<'doc>(src: &'doc str, arena: &'doc str, s: Str) -> &'doc str {
    let (base, off) = if s.off & ARENA != 0 {
        (arena, s.off & !ARENA)
    } else {
        (src, s.off)
    };
    let end = off as usize + s.len as usize;
    assert!(end <= base.len(), "span past the end of its text");
    &base[off as usize..end]
}

/// Returns whether record `ra` of `a` and record `rb` of `b` are identical: kind, word,
/// fields, attribute lines in order, verbatim lines and comments.
pub(crate) fn record_eq(a: &Document<'_>, ra: u32, b: &Document<'_>, rb: u32) -> bool {
    assert!(ra < a.record_count(), "record {ra} out of range");
    assert!(rb < b.record_count(), "record {rb} out of range");
    let attrs_eq = || {
        let (xa, xb) = (a.attrs(ra), b.attrs(rb));
        xa.len() == xb.len()
            && xa.zip(xb).all(|(i, j)| {
                a.attr_key(i) == b.attr_key(j) && a.attr_values(i).eq(b.attr_values(j))
            })
    };
    a.record_kind(ra) == b.record_kind(rb)
        && a.record_word(ra) == b.record_word(rb)
        && a.fields(ra).eq(b.fields(rb))
        && attrs_eq()
        && a.verbatim(ra).eq(b.verbatim(rb))
        && a.comments(ra).eq(b.comments(rb))
}

impl Str {
    /// Returns the source byte offset, or `None` for arena text.
    pub(crate) fn src_offset(self) -> Option<usize> {
        (self.off & ARENA == 0).then_some(self.off as usize)
    }

    /// Returns the span of source bytes `start..end`.
    pub(crate) fn src(start: usize, end: usize) -> Str {
        assert!(start <= end);
        assert!(end < ARENA as usize, "source spans address 2^31 bytes");
        Str {
            off: start as u32,
            len: (end - start) as u32,
        }
    }

    /// Returns the span of `arena[start..]`, the text appended since `start`.
    pub(crate) fn arena_tail(arena: &str, start: usize) -> Str {
        assert!(start <= arena.len());
        assert!(arena.len() < ARENA as usize, "the arena holds under 2 GiB");
        Str {
            off: start as u32 | ARENA,
            len: (arena.len() - start) as u32,
        }
    }
}

impl<'doc> Iterator for Texts<'doc> {
    type Item = &'doc str;

    fn next(&mut self) -> Option<&'doc str> {
        let s = self.spans.next()?;
        Some(resolve(self.src, self.arena, *s))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.spans.size_hint()
    }
}

impl ExactSizeIterator for Texts<'_> {}

/// Every `r` parameter below panics when `r >= record_count()`, and every `a` when it is
/// not an index from [`Document::attrs`].
impl<'src> Document<'src> {
    /// Returns the file kind from the header.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        // Pairs the typed kind with the header text the writer emits.
        if !self.record_kinds.is_empty() {
            debug_assert_eq!(self.field(0, 1), Some(self.kind.word()));
        }
        self.kind
    }

    /// Returns the header version; larger than [`VERSION`] means unknown records were kept.
    ///
    /// # Panics
    ///
    /// Never for a parsed or built document: the reader rejects version 0.
    #[must_use]
    pub fn version(&self) -> u16 {
        assert!(self.version >= 1, "version 0 is rejected by the reader");
        self.version
    }

    /// Returns the number of records, header included (so at least 1 for a parsed file).
    ///
    /// # Panics
    ///
    /// Never for a parsed or built document: asserts that the record columns are in step.
    #[must_use]
    pub fn record_count(&self) -> u32 {
        self.assert_columns();
        let count = self.record_kinds.len();
        assert!(count < NONE as usize, "record indices fit u32 below NONE");
        count as u32
    }

    /// Returns record `r`'s kind.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`; the same holds for every `r` parameter below.
    #[must_use]
    pub fn record_kind(&self, r: u32) -> RecordKind {
        let kind = self.record_kinds[r as usize];
        if r == 0 {
            assert_eq!(kind, RecordKind::Header, "record 0 is the header");
        }
        kind
    }

    /// Returns record `r`'s word (`"schemify"` for the header, the file's word for `Unknown`).
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn record_word(&self, r: u32) -> &str {
        let kind = self.record_kind(r);
        if kind != RecordKind::Unknown {
            return kind.word();
        }
        let i = self
            .unknown_words
            .binary_search_by_key(&r, |e| e.record)
            .expect("every Unknown record has its word row");
        let word = self.text(self.unknown_words[i].word);
        assert!(is_word(word), "unknown record word is a word");
        word
    }

    /// Returns record `r`'s positional values (after the word).
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn fields(&self, r: u32) -> Texts<'_> {
        self.texts(self.field_spans(r))
    }

    /// Returns field `i` of record `r`, or `None` past the last field.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn field(&self, r: u32, i: usize) -> Option<&str> {
        self.field_spans(r).get(i).map(|s| self.text(*s))
    }

    /// Returns record `r`'s identity per its kind's [`KeyRule`]: `(id or name, "")`,
    /// `(type, name)` for `locked`, `("", "")` for the header and positional records.
    /// `Unknown` records are keyed by their first field.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
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
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn attrs(&self, r: u32) -> Range<u32> {
        let range = csr(&self.record_attr_start, r, self.attr_key.len());
        assert!(range.end < NONE as usize, "attribute index fits u32");
        range.start as u32..range.end as u32
    }

    /// Returns attribute `a`'s word.
    ///
    /// # Panics
    ///
    /// Panics if `a` is not an attribute index.
    #[must_use]
    pub fn attr_key(&self, a: u32) -> &str {
        assert!(
            (a as usize) < self.attr_key.len(),
            "attribute {a} out of range ({} attributes)",
            self.attr_key.len()
        );
        self.text(self.attr_key[a as usize])
    }

    /// Returns attribute `a`'s values.
    ///
    /// # Panics
    ///
    /// Panics if `a` is not an attribute index.
    #[must_use]
    pub fn attr_values(&self, a: u32) -> Texts<'_> {
        self.texts(self.value_spans(a))
    }

    /// Returns value `i` of attribute `a`, or `None` past the last value.
    ///
    /// # Panics
    ///
    /// Panics if `a` is not an attribute index.
    #[must_use]
    pub fn attr_value(&self, a: u32, i: usize) -> Option<&str> {
        self.value_spans(a).get(i).map(|s| self.text(*s))
    }

    /// Returns record `r`'s fields in milli-units (D33: i32), one per field: each
    /// coordinate's value, parsed once by the reader's check pass or as pushed with
    /// [`Document::push_field_coord`]; 0 for every other value (a coordinate pushed as
    /// text included), so a consumer reads only its kind's `Coord` slots.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn field_coords(&self, r: u32) -> &[i32] {
        assert_eq!(self.field_milli.len(), self.fields.len(), "one per field");
        &self.field_milli[self.field_range(r)]
    }

    /// Returns attribute `a`'s values in milli-units, as [`Document::field_coords`]
    /// ([`Document::push_value_coord`] for a built document).
    ///
    /// # Panics
    ///
    /// Panics if `a` is not an attribute index.
    #[must_use]
    pub fn value_coords(&self, a: u32) -> &[i32] {
        assert_eq!(
            self.value_milli.len(),
            self.attr_values.len(),
            "one per value"
        );
        &self.value_milli[self.value_range(a)]
    }

    /// Returns the first attribute of record `r` with word `key`. Cost: linear in the
    /// record's attribute count.
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn find_attr(&self, r: u32, key: &str) -> Option<u32> {
        let found = self.attrs(r).find(|&a| self.attr_key(a) == key);
        assert!(found.is_none_or(|a| self.attrs(r).contains(&a)));
        found
    }

    /// Returns record `r`'s verbatim lines (text after `| `).
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn verbatim(&self, r: u32) -> Texts<'_> {
        self.texts(&self.verbatim[csr(&self.record_verbatim_start, r, self.verbatim.len())])
    }

    /// Returns the comments written before record `r` (text after `#`, trailing
    /// whitespace removed).
    ///
    /// # Panics
    ///
    /// Panics if `r >= record_count()`.
    #[must_use]
    pub fn comments(&self, r: u32) -> Texts<'_> {
        self.texts(&self.comments[csr(&self.record_comment_start, r, self.comments.len())])
    }

    /// Returns the comments after the last record.
    #[must_use]
    pub fn eof_comments(&self) -> Texts<'_> {
        self.texts(&self.eof_comments)
    }

    /// Returns parse warnings (`W001` duplicate ids, later occurrence named by
    /// [`Diagnostic::record`]); the `doc` layer re-ids those records.
    #[must_use]
    pub fn warnings(&self) -> &[Diagnostic] {
        debug_assert!(self.warnings.iter().all(|d| d.code.is_warning()));
        &self.warnings
    }

    /// Parallel columns of one table have equal lengths (the SoA invariant). The
    /// coordinate columns may lag their text columns while the lexer fills them; the
    /// reader's check pass rebuilds them whole.
    pub(crate) fn assert_columns(&self) {
        let records = self.record_kinds.len();
        assert_eq!(self.record_lines.len(), records);
        assert_eq!(self.record_field_start.len(), records);
        assert_eq!(self.record_attr_start.len(), records);
        assert_eq!(self.record_verbatim_start.len(), records);
        assert_eq!(self.record_comment_start.len(), records);
        assert!(self.unknown_words.len() <= records);
        assert_eq!(self.attr_line.len(), self.attr_key.len());
        assert_eq!(self.attr_value_start.len(), self.attr_key.len());
        assert!(self.field_milli.len() <= self.fields.len());
        assert!(self.value_milli.len() <= self.attr_values.len());
    }

    /// Returns the 1-based source line of record `r`; 0 for records built in memory.
    pub(crate) fn record_line(&self, r: u32) -> u32 {
        let line = self.record_lines[r as usize];
        if line != 0 {
            assert!(!self.src.is_empty(), "a source line needs a source");
        }
        line
    }

    /// Returns the 1-based source line of attribute `a`; 0 when built in memory.
    pub(crate) fn attr_line(&self, a: u32) -> u32 {
        let line = self.attr_line[a as usize];
        if line != 0 {
            assert!(!self.src.is_empty(), "a source line needs a source");
        }
        line
    }

    /// Returns where record `r`'s fields sit in `self.fields`.
    pub(crate) fn field_range(&self, r: u32) -> Range<usize> {
        csr(&self.record_field_start, r, self.fields.len())
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

    /// Returns the text of span `s`.
    pub(crate) fn text(&self, s: Str) -> &str {
        resolve(self.src, &self.arena, s)
    }

    /// Returns each record's rank; `Unknown` records take the rank of the closest known
    /// record before them (spec §5).
    pub(crate) fn ranks(&self) -> Vec<u8> {
        let mut current = 0;
        let ranks: Vec<u8> = self
            .record_kinds
            .iter()
            .map(|k| {
                if *k != RecordKind::Unknown {
                    current = k.rank();
                }
                current
            })
            .collect();
        assert_eq!(ranks.len(), self.record_kinds.len());
        assert!(
            ranks.iter().all(|&r| r <= RANK_MAX),
            "inherited ranks are real ranks"
        );
        ranks
    }

    /// Returns whether wire `r` is written with its endpoints swapped so that
    /// `(x1, y1) < (x2, y2)` (spec §4). `false` for anything not a well-formed wire.
    /// Parses the text, since a built wire may carry coordinates pushed as text.
    pub(crate) fn wire_reversed(&self, r: u32) -> bool {
        let f = self.field_spans(r);
        if self.record_kind(r) != RecordKind::Wire || f.len() != 5 {
            return false;
        }
        let c = |i: usize| scalar::parse_coord(self.text(f[i])).ok();
        match (c(1), c(2), c(3), c(4)) {
            (Some(x1), Some(y1), Some(x2), Some(y2)) => (x2, y2) < (x1, y1),
            _ => false,
        }
    }
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

// ---- private ----

/// Returns `starts[i]..starts[i + 1]` (or `..total` for the last row).
fn csr(starts: &[u32], i: u32, total: usize) -> Range<usize> {
    let i = i as usize;
    assert!(i < starts.len(), "row index out of range");
    let start = starts[i] as usize;
    let end = starts.get(i + 1).map_or(total, |&e| e as usize);
    assert!(start <= end, "CSR row starts out of order");
    assert!(end <= total, "CSR row past the end of its pool");
    start..end
}

impl Document<'_> {
    fn texts<'doc>(&'doc self, spans: &'doc [Str]) -> Texts<'doc> {
        Texts {
            src: self.src,
            arena: &self.arena,
            spans: spans.iter(),
        }
    }
}
