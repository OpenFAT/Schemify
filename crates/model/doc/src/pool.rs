//! Per-row variable-length data of one document ([`Pool`]: attribute lines, verbatim
//! lines, comments, graphic coordinates), plus the two row shapes that cross it: a
//! borrowed record going in ([`RecView`], from a format document or a [`RecordBuf`]) and
//! a record coming out ([`Sink`], into a format document or a [`RecordBuf`]).
//! Cites: format spec §3–§4 (record model), D33 (milli-unit coordinates).
//!
//! Six questions: 1. record parts in, `Span`s per row out; 2. a few entries per row,
//! [`POOL_MAX`] per vector; 3. `Sym` values, `i32` coordinates, `u32` span offsets;
//! 4. appended on load and edit, read per row through its span; 5. lifetime: the
//! document, garbage counted in [`Pool::dead`] and reclaimed by compaction; 6. one
//! writer (`doc`), readers through `&Pool`.
//!
//! Contents:
//! - `pub struct Span`: `range`; `pub(crate) const POOL_MAX`
//! - `pub struct Pool`: `attrs`, `find_attr`; crate: `push_attrs`, `push_lines`,
//!   `push_coords`, `orphan`, `size`
//! - `pub(crate) struct RecView`: `attr`; `pub(crate) struct AttrLine`,
//!   `pub(crate) struct AttrValues`; `pub(crate) struct Scratch`: `fill_format`,
//!   `fill_buf`, `view`
//! - `pub(crate) trait Sink`, `pub(crate) struct FormatSink`: `new`
//! - `pub(crate) fn emit_attrs`, `pub(crate) fn emit_comments`

use crate::record::{Attr, RecordBuf};
use crate::strings::{ObjId, Strings, Sym};
use schemify_format::{Document as FormatDoc, RecordKind};
use std::ops::Range;

/// A run of pool entries owned by one row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// First entry.
    pub start: u32,
    /// Entry count.
    pub len: u32,
}

/// Pool capacity per vector (attribute lines, values, lines, coordinates).
pub(crate) const POOL_MAX: usize = 1 << 28;

/// Per-row variable-length data, shared by every table of a document.
///
/// Rows own disjoint [`Span`]s. Edits never write into an existing span: a replaced row
/// appends fresh runs and the old ones become garbage (copy on write), so spans other
/// rows hold stay valid across edits; a compaction renumbers them all.
#[derive(Clone, Debug, Default)]
pub struct Pool {
    /// Attribute line keys.
    pub attr_key: Vec<Sym>,
    /// Attribute line values, as spans into [`Pool::vals`]; parallel to `attr_key`.
    pub attr_vals: Vec<Span>,
    /// Attribute values.
    pub vals: Vec<Sym>,
    /// Verbatim lines and comments.
    pub lines: Vec<Sym>,
    /// Graphic coordinates (milli-units).
    pub coords: Vec<i32>,
    /// Entries orphaned by removed or replaced rows.
    pub dead: u32,
}

/// A borrowed record from either source (a format document or a [`RecordBuf`]), so one
/// push path serves load and edits without per-record allocation.
pub(crate) struct RecView<'src> {
    pub(crate) kind: RecordKind,
    pub(crate) fields: &'src [&'src str],
    /// Milli-units of each field: a `Coord` slot's value (D33: i32), unspecified at other
    /// slots, which no table reads. From format's coordinate column on load, so a loaded
    /// coordinate is parsed once.
    pub(crate) field_milli: &'src [i32],
    pub(crate) attrs: &'src [AttrLine<'src>],
    pub(crate) vals: &'src [&'src str],
    /// As `field_milli`, one per value.
    pub(crate) val_milli: &'src [i32],
    pub(crate) verbatim: &'src [&'src str],
    pub(crate) comments: &'src [&'src str],
}

/// One attribute line of a [`RecView`]: its key and its values' range in `vals`.
#[derive(Clone, Debug)]
pub(crate) struct AttrLine<'src> {
    pub(crate) key: &'src str,
    pub(crate) values: Range<u32>,
}

/// The values of one attribute line and their milli-units (in step).
pub(crate) struct AttrValues<'view> {
    pub(crate) texts: &'view [&'view str],
    pub(crate) milli: &'view [i32],
}

/// Reusable buffers that build a [`RecView`].
#[derive(Default)]
pub(crate) struct Scratch<'src> {
    fields: Vec<&'src str>,
    field_milli: Vec<i32>,
    attrs: Vec<AttrLine<'src>>,
    vals: Vec<&'src str>,
    val_milli: Vec<i32>,
    verbatim: Vec<&'src str>,
    comments: Vec<&'src str>,
}

/// Where a table row goes when written out: a format document (save) or a
/// [`RecordBuf`] (edits, undo images). Axis: the output form; two cases today; a
/// generic, so every emit is monomorphized with no dispatch.
pub(crate) trait Sink {
    fn begin(&mut self, kind: RecordKind);
    fn field(&mut self, text: &str);
    fn attr(&mut self, key: &str);
    fn value(&mut self, text: &str);
    fn verbatim(&mut self, line: &str);
    fn comment(&mut self, line: &str);
    fn coord(&mut self, milli: i32);
    fn coord_value(&mut self, milli: i32);
    fn id(&mut self, id: ObjId, strings: &Strings);
}

/// The save sink: the format document being built plus one reusable buffer for
/// formatted ids (no allocation per value).
pub(crate) struct FormatSink {
    pub(crate) doc: FormatDoc<'static>,
    buf: String,
}

impl Span {
    /// Returns the entries as an index range.
    #[must_use]
    pub fn range(self) -> Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

impl Pool {
    /// Iterates the attribute lines of `span` as `(key, values)`.
    ///
    /// # Panics
    ///
    /// Panics if `span` is not a span of this pool (from another document, or from
    /// before a compaction).
    pub fn attrs(&self, span: Span) -> impl Iterator<Item = (Sym, &[Sym])> + '_ {
        span.range()
            .map(|a| (self.attr_key[a], &self.vals[self.attr_vals[a].range()]))
    }

    /// Returns the values of the first attribute line of `span` whose key is `key`.
    /// `strings` is the arena of the same document.
    #[must_use]
    pub fn find_attr<'p>(&'p self, strings: &Strings, span: Span, key: &str) -> Option<&'p [Sym]> {
        let key = strings.find(key)?;
        self.attrs(span).find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Appends the attribute lines of `rec` whose keys are not in `skip`.
    pub(crate) fn push_attrs(
        &mut self,
        rec: &RecView<'_>,
        skip: &[&str],
        strings: &mut Strings,
    ) -> Span {
        assert_eq!(rec.vals.len(), rec.val_milli.len(), "value columns in step");
        let start = self.attr_key.len();
        for line in rec.attrs {
            if skip.contains(&line.key) {
                continue;
            }
            let values_start = self.vals.len();
            for v in &rec.vals[line.values.start as usize..line.values.end as usize] {
                self.vals.push(strings.intern(v));
            }
            self.attr_key.push(strings.intern(line.key));
            self.attr_vals.push(span(values_start, self.vals.len()));
        }
        assert!(self.attr_key.len() < POOL_MAX, "pool full");
        assert!(self.vals.len() < POOL_MAX, "pool full");
        assert_eq!(self.attr_key.len(), self.attr_vals.len());
        span(start, self.attr_key.len())
    }

    pub(crate) fn push_lines(&mut self, lines: &[&str], strings: &mut Strings) -> Span {
        let start = self.lines.len();
        for l in lines {
            self.lines.push(strings.intern(l));
        }
        assert!(self.lines.len() < POOL_MAX, "pool full");
        let pushed = span(start, self.lines.len());
        assert_eq!(pushed.len as usize, lines.len());
        pushed
    }

    pub(crate) fn push_coords(&mut self, milli: &[i32]) -> Span {
        let start = self.coords.len();
        self.coords.extend_from_slice(milli);
        assert!(self.coords.len() < POOL_MAX, "pool full");
        let pushed = span(start, self.coords.len());
        assert_eq!(pushed.len as usize, milli.len());
        pushed
    }

    /// Counts the entries a removed row leaves behind: its attribute lines with their
    /// values, plus `others` (comments, verbatim lines, coordinates).
    pub(crate) fn orphan(&mut self, attrs: Span, others: u32) {
        let vals: u32 = attrs.range().map(|a| self.attr_vals[a].len).sum();
        self.dead = self.dead.saturating_add(attrs.len + vals + others);
        assert!(
            self.dead as usize <= self.size(),
            "garbage is part of the pool"
        );
    }

    /// Returns the number of live-or-dead entries, the denominator for compaction.
    pub(crate) fn size(&self) -> usize {
        assert_eq!(
            self.attr_key.len(),
            self.attr_vals.len(),
            "attribute columns in step"
        );
        self.attr_key.len() + self.vals.len() + self.lines.len() + self.coords.len()
    }
}

impl RecView<'_> {
    /// The values of attribute `key` and their milli-units.
    pub(crate) fn attr(&self, key: &str) -> Option<AttrValues<'_>> {
        let line = self.attrs.iter().find(|line| line.key == key)?;
        let range = line.values.start as usize..line.values.end as usize;
        let values = AttrValues {
            texts: &self.vals[range.clone()],
            milli: &self.val_milli[range],
        };
        assert_eq!(values.texts.len(), values.milli.len());
        Some(values)
    }
}

impl<'src> Scratch<'src> {
    pub(crate) fn fill_format(&mut self, doc: &'src FormatDoc<'_>, r: u32) {
        self.clear();
        self.fields.extend(doc.fields(r));
        self.field_milli.extend_from_slice(doc.field_coords(r));
        for a in doc.attrs(r) {
            let start = self.vals.len() as u32;
            self.vals.extend(doc.attr_values(a));
            self.val_milli.extend_from_slice(doc.value_coords(a));
            self.attrs.push(AttrLine {
                key: doc.attr_key(a),
                values: start..self.vals.len() as u32,
            });
        }
        self.verbatim.extend(doc.verbatim(r));
        self.comments.extend(doc.comments(r));
        assert_eq!(self.attrs.len(), doc.attrs(r).len());
        assert_eq!(
            self.fields.len(),
            self.field_milli.len(),
            "one coordinate per field"
        );
        assert_eq!(
            self.vals.len(),
            self.val_milli.len(),
            "one coordinate per value"
        );
    }

    pub(crate) fn fill_buf(&mut self, rec: &'src RecordBuf) {
        // The edit path: values were checked (`check_record`), but not into a column.
        let milli = |t: &String| schemify_format::parse_coord(t).unwrap_or(0);
        self.clear();
        self.fields.extend(rec.fields.iter().map(String::as_str));
        self.field_milli.extend(rec.fields.iter().map(milli));
        for a in &rec.attrs {
            let start = self.vals.len() as u32;
            self.vals.extend(a.values.iter().map(String::as_str));
            self.val_milli.extend(a.values.iter().map(milli));
            self.attrs.push(AttrLine {
                key: a.key.as_str(),
                values: start..self.vals.len() as u32,
            });
        }
        self.verbatim
            .extend(rec.verbatim.iter().map(String::as_str));
        self.comments
            .extend(rec.comments.iter().map(String::as_str));
        assert_eq!(self.attrs.len(), rec.attrs.len());
        assert_eq!(
            self.fields.len(),
            self.field_milli.len(),
            "one coordinate per field"
        );
        assert_eq!(
            self.vals.len(),
            self.val_milli.len(),
            "one coordinate per value"
        );
    }

    pub(crate) fn view(&self, kind: RecordKind) -> RecView<'_> {
        RecView {
            kind,
            fields: &self.fields,
            field_milli: &self.field_milli,
            attrs: &self.attrs,
            vals: &self.vals,
            val_milli: &self.val_milli,
            verbatim: &self.verbatim,
            comments: &self.comments,
        }
    }
}

impl FormatSink {
    pub(crate) fn new(doc: FormatDoc<'static>) -> FormatSink {
        FormatSink {
            doc,
            buf: String::with_capacity(16),
        }
    }
}

impl Sink for FormatSink {
    fn begin(&mut self, kind: RecordKind) {
        self.doc.begin_record(kind);
    }
    fn field(&mut self, text: &str) {
        self.doc.push_field(text);
    }
    fn attr(&mut self, key: &str) {
        self.doc.push_attr(key);
    }
    fn value(&mut self, text: &str) {
        self.doc.push_value(text);
    }
    fn verbatim(&mut self, line: &str) {
        self.doc.push_verbatim(line);
    }
    fn comment(&mut self, line: &str) {
        self.doc.push_comment(line);
    }
    fn coord(&mut self, milli: i32) {
        self.doc.push_field_coord(milli);
    }
    fn coord_value(&mut self, milli: i32) {
        self.doc.push_value_coord(milli);
    }
    fn id(&mut self, id: ObjId, strings: &Strings) {
        self.buf.clear();
        id.write(strings, &mut self.buf);
        self.doc.push_field(&self.buf);
    }
}

impl Sink for RecordBuf {
    fn begin(&mut self, kind: RecordKind) {
        *self = RecordBuf::new(kind);
    }
    fn field(&mut self, text: &str) {
        self.fields.push(text.to_owned());
    }
    fn attr(&mut self, key: &str) {
        self.attrs.push(Attr {
            key: key.to_owned(),
            values: Vec::new(),
        });
    }
    fn value(&mut self, text: &str) {
        let a = self.attrs.last_mut().expect("value before attr");
        a.values.push(text.to_owned());
    }
    fn verbatim(&mut self, line: &str) {
        self.verbatim.push(line.to_owned());
    }
    fn comment(&mut self, line: &str) {
        self.comments.push(line.to_owned());
    }
    fn coord(&mut self, milli: i32) {
        let mut s = String::new();
        schemify_format::format_coord(milli, &mut s);
        self.fields.push(s);
    }
    fn coord_value(&mut self, milli: i32) {
        let mut s = String::new();
        schemify_format::format_coord(milli, &mut s);
        self.value(&s);
    }
    fn id(&mut self, id: ObjId, strings: &Strings) {
        self.fields.push(id.text(strings));
    }
}

/// Writes the attribute lines of `span` to `sink`.
pub(crate) fn emit_attrs<S: Sink>(span: Span, strings: &Strings, pool: &Pool, sink: &mut S) {
    for (key, vals) in pool.attrs(span) {
        sink.attr(strings.get(key));
        for v in vals {
            sink.value(strings.get(*v));
        }
    }
}

/// Writes the lines of `span` to `sink` as comments.
pub(crate) fn emit_comments<S: Sink>(span: Span, strings: &Strings, pool: &Pool, sink: &mut S) {
    for &l in &pool.lines[span.range()] {
        sink.comment(strings.get(l));
    }
}

// ---- private ----

fn span(start: usize, end: usize) -> Span {
    assert!(start <= end);
    assert!(end < POOL_MAX);
    Span {
        start: start as u32,
        len: (end - start) as u32,
    }
}

impl Scratch<'_> {
    fn clear(&mut self) {
        self.fields.clear();
        self.field_milli.clear();
        self.attrs.clear();
        self.vals.clear();
        self.val_milli.clear();
        self.verbatim.clear();
        self.comments.clear();
    }
}
