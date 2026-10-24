//! Design tables of one document: typed column-major (SoA) tables per record family,
//! plus the shared pool that holds every row's attribute lines, verbatim lines,
//! comments and graphic coordinates.
//!
//! Six questions, answered once for all tables here and per table in its doc:
//! 1. In: format records (load) or validated [`RecordBuf`]s (edits). Out: column slices
//!    for `elab`, `render`, `plugin`; format records again on save.
//! 2. Rank-3 tables hold up to 2^22 rows each (ARCHITECTURE: objects per cell); symbol
//!    and bench tables up to 2^12; capacities are [`crate::TableId::capacity`].
//! 3. Coordinates are i32 milli-units (D33); text is a [`Sym`] (`u32`); ids are
//!    [`ObjId`] (`u32`); orientation and pin direction are `u8` enums; pool offsets `u32`.
//! 4. Consumers scan whole columns (connectivity reads `x1..y2` of every wire); edits
//!    touch one row. Attributes are read per row through its [`Span`].
//! 5. Lifetime: the document. Pool entries of replaced rows become garbage that
//!    [`crate::Document`] reclaims by rebuilding (counted in [`Pool::dead`]).
//! 6. Read-parallel: consumers get `&` slices; `doc` is the single writer.
//!
//! Row order: tables of ranks 1, 2, 4 and 5 are in file order (it is semantic: port
//! order, draw order, document order). Rank-3 and rank-6 tables are unordered; the
//! writer sorts them. Row indices are valid until the next edit of that table.

use crate::record::{INST_RESERVED, RecordBuf};
use crate::strings::{ObjId, Strings, Sym, push_coord};
use schemify_format::{Document as FormatDoc, KeyRule, Orient, RecordKind, parse_coord};
use std::ops::Range;

/// A run of pool entries owned by one row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// First entry.
    pub start: u32,
    /// Entry count.
    pub len: u32,
}

impl Span {
    /// Returns the entries as an index range.
    pub fn range(self) -> Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

/// Pool capacity per vector (attribute lines, values, lines, coordinates).
pub const POOL_MAX: usize = 1 << 28;

/// Per-row variable-length data, shared by every table of a document.
///
/// Rows own disjoint [`Span`]s. Edits never write into an existing span: a replaced row
/// appends fresh runs and the old ones become garbage (copy on write), so spans other
/// rows hold stay valid.
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

impl Pool {
    /// Iterates the attribute lines of `span` as `(key, values)`.
    pub fn attrs(&self, span: Span) -> impl Iterator<Item = (Sym, &[Sym])> + '_ {
        span.range()
            .map(|a| (self.attr_key[a], &self.vals[self.attr_vals[a].range()]))
    }

    /// Returns the values of the first attribute line of `span` whose key is `key`.
    pub fn find_attr<'p>(&'p self, strings: &Strings, span: Span, key: &str) -> Option<&'p [Sym]> {
        let key = strings.find(key)?;
        self.attrs(span).find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    pub(crate) fn push_attrs(
        &mut self,
        rec: &RecView<'_>,
        skip: &[&str],
        strings: &mut Strings,
    ) -> Span {
        let start = self.attr_key.len();
        for (key, vals) in rec.attrs {
            if skip.contains(key) {
                continue;
            }
            let v_start = self.vals.len();
            for v in &rec.vals[vals.start as usize..vals.end as usize] {
                self.vals.push(strings.intern(v));
            }
            self.attr_key.push(strings.intern(key));
            self.attr_vals.push(span(v_start, self.vals.len()));
        }
        assert!(
            self.attr_key.len() < POOL_MAX && self.vals.len() < POOL_MAX,
            "pool full"
        );
        assert_eq!(self.attr_key.len(), self.attr_vals.len());
        span(start, self.attr_key.len())
    }

    fn push_lines(&mut self, lines: &[&str], strings: &mut Strings) -> Span {
        let start = self.lines.len();
        for l in lines {
            self.lines.push(strings.intern(l));
        }
        assert!(self.lines.len() < POOL_MAX, "pool full");
        span(start, self.lines.len())
    }

    fn push_coords(&mut self, texts: &[&str]) -> Span {
        let start = self.coords.len();
        for t in texts {
            self.coords.push(coord(t));
        }
        assert!(self.coords.len() < POOL_MAX, "pool full");
        span(start, self.coords.len())
    }

    /// Counts the entries a removed row leaves behind.
    pub(crate) fn orphan(&mut self, attrs: Span, lines: u32) {
        let vals: u32 = attrs.range().map(|a| self.attr_vals[a].len).sum();
        self.dead = self.dead.saturating_add(attrs.len + vals + lines);
    }

    /// Returns the number of live-or-dead entries, the denominator for compaction.
    pub(crate) fn size(&self) -> usize {
        self.attr_key.len() + self.vals.len() + self.lines.len() + self.coords.len()
    }
}

fn span(start: usize, end: usize) -> Span {
    assert!(start <= end && end < POOL_MAX);
    Span {
        start: start as u32,
        len: (end - start) as u32,
    }
}

/// Parses a coordinate the caller already validated (format reader or `check_record`):
/// `-?digits(.digits)?` with at most 3 significant fractional digits, in i32 range. The
/// load path parses ~3 per object, so this skips `parse_coord`'s re-validation; tested
/// equal to it over a sweep.
pub(crate) fn coord(text: &str) -> i32 {
    let b = text.as_bytes();
    let (neg, digits) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        _ => (false, b),
    };
    let mut milli: i64 = 0;
    let mut frac: i32 = -1; // fractional digits seen, -1 before the point
    for &c in digits {
        if c == b'.' {
            frac = 0;
            continue;
        }
        if frac < 3 {
            milli = milli * 10 + i64::from(c - b'0');
            frac += i32::from(frac >= 0);
        }
    }
    for _ in frac.max(0)..3 {
        milli *= 10;
    }
    let v = if neg { -milli } else { milli };
    let v = i32::try_from(v).expect("coordinates are validated before they reach a table");
    debug_assert_eq!(Ok(v), parse_coord(text));
    v
}

fn orient(text: &str) -> Orient {
    Orient::parse(text).expect("orientations are validated before they reach a table")
}

// ------------------------------------------------------------------ record views

/// A borrowed record from either source (a format document or a [`RecordBuf`]), so one
/// push path serves load and edits without per-record allocation.
pub(crate) struct RecView<'a> {
    pub(crate) kind: RecordKind,
    pub(crate) fields: &'a [&'a str],
    /// `(key, value range into vals)`.
    pub(crate) attrs: &'a [(&'a str, Range<u32>)],
    pub(crate) vals: &'a [&'a str],
    pub(crate) verbatim: &'a [&'a str],
    pub(crate) comments: &'a [&'a str],
}

impl RecView<'_> {
    fn attr(&self, key: &str) -> Option<&[&str]> {
        self.attrs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, r)| &self.vals[r.start as usize..r.end as usize])
    }
}

/// Reusable buffers that build a [`RecView`].
#[derive(Default)]
pub(crate) struct Scratch<'a> {
    fields: Vec<&'a str>,
    attrs: Vec<(&'a str, Range<u32>)>,
    vals: Vec<&'a str>,
    verbatim: Vec<&'a str>,
    comments: Vec<&'a str>,
}

impl<'a> Scratch<'a> {
    pub(crate) fn fill_format(&mut self, doc: &'a FormatDoc<'_>, r: u32) {
        self.clear();
        self.fields.extend(doc.fields(r));
        for a in doc.attrs(r) {
            let start = self.vals.len() as u32;
            self.vals.extend(doc.attr_values(a));
            self.attrs
                .push((doc.attr_key(a), start..self.vals.len() as u32));
        }
        self.verbatim.extend(doc.verbatim(r));
        self.comments.extend(doc.comments(r));
        assert_eq!(self.attrs.len(), doc.attrs(r).len());
    }

    pub(crate) fn fill_buf(&mut self, rec: &'a RecordBuf) {
        self.clear();
        self.fields.extend(rec.fields.iter().map(String::as_str));
        for a in &rec.attrs {
            let start = self.vals.len() as u32;
            self.vals.extend(a.values.iter().map(String::as_str));
            self.attrs
                .push((a.key.as_str(), start..self.vals.len() as u32));
        }
        self.verbatim
            .extend(rec.verbatim.iter().map(String::as_str));
        self.comments
            .extend(rec.comments.iter().map(String::as_str));
        assert_eq!(self.attrs.len(), rec.attrs.len());
    }

    fn clear(&mut self) {
        self.fields.clear();
        self.attrs.clear();
        self.vals.clear();
        self.verbatim.clear();
        self.comments.clear();
    }

    pub(crate) fn view(&self, kind: RecordKind) -> RecView<'_> {
        RecView {
            kind,
            fields: &self.fields,
            attrs: &self.attrs,
            vals: &self.vals,
            verbatim: &self.verbatim,
            comments: &self.comments,
        }
    }
}

/// Where a table row goes when written out: a format document (save) or a
/// [`RecordBuf`] (edits, undo images). Compile-time generic; no dispatch.
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
/// formatted coordinates and ids (no allocation per value).
pub(crate) struct FormatSink {
    pub(crate) doc: FormatDoc<'static>,
    buf: String,
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
        self.buf.clear();
        push_coord(milli, &mut self.buf);
        self.doc.push_field(&self.buf);
    }
    fn coord_value(&mut self, milli: i32) {
        self.buf.clear();
        push_coord(milli, &mut self.buf);
        self.doc.push_value(&self.buf);
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
        self.attrs.push(crate::record::Attr {
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
        push_coord(milli, &mut s);
        self.fields.push(s);
    }
    fn coord_value(&mut self, milli: i32) {
        let mut s = String::new();
        push_coord(milli, &mut s);
        self.value(&s);
    }
    fn id(&mut self, id: ObjId, strings: &Strings) {
        self.fields.push(id.text(strings));
    }
}

fn emit_attrs<S: Sink>(span: Span, strings: &Strings, pool: &Pool, sink: &mut S) {
    for (key, vals) in pool.attrs(span) {
        sink.attr(strings.get(key));
        for v in vals {
            sink.value(strings.get(*v));
        }
    }
}

fn emit_lines<S: Sink>(span: Span, strings: &Strings, pool: &Pool, sink: &mut S, verbatim: bool) {
    for &l in &pool.lines[span.range()] {
        if verbatim {
            sink.verbatim(strings.get(l));
        } else {
            sink.comment(strings.get(l));
        }
    }
}

// ------------------------------------------------------------------ the Table trait

/// Operations every table supports; the document dispatches by `TableId` with a
/// `match`, so every call is static.
pub(crate) trait Table {
    fn len(&self) -> usize;
    /// Record kind of `row`.
    fn kind_at(&self, row: usize) -> RecordKind;
    /// Appends a row. The record was validated (format reader or `check_record`).
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool);
    /// Writes `row` as one record.
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S);
    fn swap_remove_row(&mut self, row: usize);
    fn remove_row(&mut self, row: usize);
    fn move_row(&mut self, from: usize, to: usize);
    fn attrs_col(&self) -> &[Span];
    fn comments_col(&self) -> &[Span];
    fn verbatim_col(&self) -> &[Span] {
        &[]
    }
    /// The row's id, or `ObjId::NONE` for kinds without one.
    fn id_at(&self, row: usize, strings: &Strings) -> ObjId;
    /// The row's name, or `Sym::NONE` for kinds without one.
    fn name_at(&self, row: usize) -> Sym;
    /// Whether every column has `len()` rows (the SoA invariant).
    fn columns_ok(&self) -> bool;
}

fn move_elem<T>(v: &mut [T], from: usize, to: usize) {
    assert!(from < v.len() && to < v.len());
    if from < to {
        v[from..=to].rotate_left(1);
    } else {
        v[to..=from].rotate_right(1);
    }
}

macro_rules! row_ops {
    ($($col:ident),+) => {
        fn len(&self) -> usize {
            self.attrs.len()
        }
        fn swap_remove_row(&mut self, row: usize) {
            $( self.$col.swap_remove(row); )+
            assert!(self.columns_ok());
        }
        fn remove_row(&mut self, row: usize) {
            $( self.$col.remove(row); )+
            assert!(self.columns_ok());
        }
        fn move_row(&mut self, from: usize, to: usize) {
            $( move_elem(&mut self.$col, from, to); )+
            assert!(self.columns_ok());
        }
        fn attrs_col(&self) -> &[Span] {
            &self.attrs
        }
        fn comments_col(&self) -> &[Span] {
            &self.comments
        }
        fn columns_ok(&self) -> bool {
            let n = self.attrs.len();
            true $( && self.$col.len() == n )+
        }
    };
}

// ------------------------------------------------------------------ tables

/// Pin direction (`pin <name> <dir> ...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PinDir {
    /// `in`
    In,
    /// `out`
    Out,
    /// `inout`
    Inout,
}

impl PinDir {
    /// Returns the file word.
    pub fn as_str(self) -> &'static str {
        ["in", "out", "inout"][self as usize]
    }

    fn parse(word: &str) -> PinDir {
        match word {
            "in" => PinDir::In,
            "out" => PinDir::Out,
            "inout" => PinDir::Inout,
            _ => panic!("pin dir {word:?} was validated before reaching the table"),
        }
    }
}

/// `pin` records (rank 1), file order = port order. Capacity 2^12. Attributes `domain`,
/// `inherit`, `number` stay in the pool (read per pin by elab, not scanned).
#[derive(Clone, Debug, Default)]
pub struct Pins {
    /// Pin name (may be a bus `D[3:0]`).
    pub name: Vec<Sym>,
    /// Direction.
    pub dir: Vec<PinDir>,
    /// Symbol-local x (milli-units).
    pub x: Vec<i32>,
    /// Symbol-local y (milli-units).
    pub y: Vec<i32>,
    /// Attribute lines.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Table for Pins {
    row_ops!(name, dir, x, y, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Pin
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 4);
        self.name.push(strings.intern(rec.fields[0]));
        self.dir.push(PinDir::parse(rec.fields[1]));
        self.x.push(coord(rec.fields[2]));
        self.y.push(coord(rec.fields[3]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(RecordKind::Pin);
        sink.field(strings.get(self.name[row]));
        sink.field(self.dir[row].as_str());
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, _row: usize, _strings: &Strings) -> ObjId {
        ObjId::NONE
    }
    fn name_at(&self, row: usize) -> Sym {
        self.name[row]
    }
}

/// Symbol graphics (rank 2) in draw order: `line rect arc circle poly stext` in one
/// table, because z-order across kinds is semantic. Capacity 2^16.
///
/// `coords` spans into [`Pool::coords`]: line/rect `x1 y1 x2 y2`, arc/circle `cx cy r`,
/// poly `x y` pairs, stext `x y`. `text` and `orient` are set for stext only, `angles`
/// (start, sweep, as number text) for arcs only; other rows hold `Sym::NONE`/`R0`.
#[derive(Clone, Debug, Default)]
pub struct Graphics {
    /// Record kind (one of the six graphic kinds).
    pub shape: Vec<RecordKind>,
    /// Coordinates.
    pub coords: Vec<Span>,
    /// stext template.
    pub text: Vec<Sym>,
    /// stext orientation.
    pub orient: Vec<Orient>,
    /// arc start and sweep degrees (file number literals).
    pub angles: Vec<[Sym; 2]>,
    /// Attribute lines (`layer`, `fill`, ...).
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Table for Graphics {
    row_ops!(shape, coords, text, orient, angles, attrs, comments);
    fn kind_at(&self, row: usize) -> RecordKind {
        self.shape[row]
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        let f = rec.fields;
        let (coords, text, orient, angles) = match rec.kind {
            RecordKind::Arc => {
                assert_eq!(f.len(), 5);
                let a = [strings.intern(f[3]), strings.intern(f[4])];
                (pool.push_coords(&f[..3]), Sym::NONE, Orient::R0, a)
            }
            RecordKind::Stext => {
                assert_eq!(f.len(), 4);
                let t = strings.intern(f[3]);
                (
                    pool.push_coords(&f[..2]),
                    t,
                    self::orient(f[2]),
                    [Sym::NONE; 2],
                )
            }
            RecordKind::Line | RecordKind::Rect | RecordKind::Circle | RecordKind::Poly => {
                (pool.push_coords(f), Sym::NONE, Orient::R0, [Sym::NONE; 2])
            }
            k => panic!("{k:?} is not a graphic"),
        };
        self.shape.push(rec.kind);
        self.coords.push(coords);
        self.text.push(text);
        self.orient.push(orient);
        self.angles.push(angles);
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        let kind = self.shape[row];
        sink.begin(kind);
        for &c in &pool.coords[self.coords[row].range()] {
            sink.coord(c);
        }
        match kind {
            RecordKind::Arc => {
                sink.field(strings.get(self.angles[row][0]));
                sink.field(strings.get(self.angles[row][1]));
            }
            RecordKind::Stext => {
                sink.field(self.orient[row].as_str());
                sink.field(strings.get(self.text[row]));
            }
            _ => {}
        }
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, _row: usize, _strings: &Strings) -> ObjId {
        ObjId::NONE
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// `inst` records (rank 3, unordered). Capacity 2^22. `at` and `name` are columns;
/// every other attribute line (parameter overrides, `inherit`, `ignore`) stays in the
/// pool in file order: overrides are the existence-based `inst_params` table (D40, a
/// missing line means "use the default").
#[derive(Clone, Debug, Default)]
pub struct Instances {
    /// Object id.
    pub id: Vec<ObjId>,
    /// Cell reference text (`lib:cell` or `./cell`).
    pub cell: Vec<Sym>,
    /// Placement x (milli-units).
    pub x: Vec<i32>,
    /// Placement y (milli-units).
    pub y: Vec<i32>,
    /// Placement orientation.
    pub orient: Vec<Orient>,
    /// Instance name (may be iterated `X1[3:0]`).
    pub name: Vec<Sym>,
    /// Attribute lines other than `at`/`name`.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Instances {
    /// Returns whether an attribute key of an instance is a parameter override
    /// (anything but `at`, `name`, `inherit`, `ignore`).
    pub fn is_override(key: &str) -> bool {
        !INST_RESERVED.contains(&key)
    }
}

impl Table for Instances {
    row_ops!(id, cell, x, y, orient, name, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Inst
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 2);
        let at = rec.attr("at").expect("`at` is required");
        let name = rec.attr("name").expect("`name` is required");
        assert!(at.len() == 3 && name.len() == 1);
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.cell.push(strings.intern(rec.fields[1]));
        self.x.push(coord(at[0]));
        self.y.push(coord(at[1]));
        self.orient.push(orient(at[2]));
        self.name.push(strings.intern(name[0]));
        self.attrs
            .push(pool.push_attrs(rec, &["at", "name"], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(RecordKind::Inst);
        sink.id(self.id[row], strings);
        sink.field(strings.get(self.cell[row]));
        // Spec §4: `at` first, then `name`; doc writes them in that order.
        sink.attr("at");
        sink.coord_value(self.x[row]);
        sink.coord_value(self.y[row]);
        sink.value(self.orient[row].as_str());
        sink.attr("name");
        sink.value(strings.get(self.name[row]));
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// `wire` records (rank 3, unordered). Capacity 2^22. Endpoints are normalized so
/// `(x1, y1) < (x2, y2)` (spec §4), the same model the reader produces. `bus` stays in
/// the pool.
#[derive(Clone, Debug, Default)]
pub struct Wires {
    /// Object id.
    pub id: Vec<ObjId>,
    /// First endpoint x (the lexicographically smaller endpoint).
    pub x1: Vec<i32>,
    /// First endpoint y.
    pub y1: Vec<i32>,
    /// Second endpoint x.
    pub x2: Vec<i32>,
    /// Second endpoint y.
    pub y2: Vec<i32>,
    /// Attribute lines.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Table for Wires {
    row_ops!(id, x1, y1, x2, y2, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Wire
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 5);
        let (mut a, mut b) = (
            (coord(rec.fields[1]), coord(rec.fields[2])),
            (coord(rec.fields[3]), coord(rec.fields[4])),
        );
        if b < a {
            std::mem::swap(&mut a, &mut b);
        }
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.x1.push(a.0);
        self.y1.push(a.1);
        self.x2.push(b.0);
        self.y2.push(b.1);
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(RecordKind::Wire);
        sink.id(self.id[row], strings);
        for c in [self.x1[row], self.y1[row], self.x2[row], self.y2[row]] {
            sink.coord(c);
        }
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// `label`, `port` and `text` records (rank 3, unordered, one table each): an anchor,
/// an orientation and one text (net expression, interface pin, or annotation).
/// Capacity 2^22.
#[derive(Clone, Debug)]
pub struct Placed {
    /// Which of the three kinds this table holds.
    pub kind: RecordKind,
    /// Object id.
    pub id: Vec<ObjId>,
    /// Anchor x (milli-units).
    pub x: Vec<i32>,
    /// Anchor y (milli-units).
    pub y: Vec<i32>,
    /// Orientation.
    pub orient: Vec<Orient>,
    /// Net expression (label), pin name (port) or annotation (text).
    pub text: Vec<Sym>,
    /// Attribute lines.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Placed {
    pub(crate) fn new(kind: RecordKind) -> Placed {
        assert!(matches!(
            kind,
            RecordKind::Label | RecordKind::Port | RecordKind::Text
        ));
        Placed {
            kind,
            id: Vec::new(),
            x: Vec::new(),
            y: Vec::new(),
            orient: Vec::new(),
            text: Vec::new(),
            attrs: Vec::new(),
            comments: Vec::new(),
        }
    }
}

impl Table for Placed {
    row_ops!(id, x, y, orient, text, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        self.kind
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert!(rec.kind == self.kind && rec.fields.len() == 5);
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.x.push(coord(rec.fields[1]));
        self.y.push(coord(rec.fields[2]));
        self.orient.push(orient(rec.fields[3]));
        self.text.push(strings.intern(rec.fields[4]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(self.kind);
        sink.id(self.id[row], strings);
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        sink.field(self.orient[row].as_str());
        sink.field(strings.get(self.text[row]));
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// `noconn` records (rank 3, unordered). Capacity 2^22.
#[derive(Clone, Debug, Default)]
pub struct Noconns {
    /// Object id.
    pub id: Vec<ObjId>,
    /// Anchor x (milli-units).
    pub x: Vec<i32>,
    /// Anchor y (milli-units).
    pub y: Vec<i32>,
    /// Attribute lines.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Table for Noconns {
    row_ops!(id, x, y, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Noconn
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 3);
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.x.push(coord(rec.fields[1]));
        self.y.push(coord(rec.fields[2]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(RecordKind::Noconn);
        sink.id(self.id[row], strings);
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// `shape` records (rank 3, unordered): schematic graphics, annotation only. Capacity
/// 2^16. `form` is the rank-2 kind of the same shape (`Line`, `Rect`, `Arc`, `Circle`,
/// `Poly`); `coords` spans into [`Pool::coords`] in record order — for an arc the start and
/// sweep degrees are stored in milli-degrees there too (the file writes them as coordinates,
/// spec §4 rank 3).
#[derive(Clone, Debug, Default)]
pub struct Shapes {
    /// Object id.
    pub id: Vec<ObjId>,
    /// The shape's form, as the rank-2 record kind of the same name.
    pub form: Vec<RecordKind>,
    /// Values after the form.
    pub coords: Vec<Span>,
    /// Attribute lines (`layer`, `fill`, `dash`, `width`, `closed`).
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

/// The rank-2 kind a `shape` form word names.
///
/// # Panics
///
/// Panics on a word outside `format::SHAPE_FORMS` (validated before reaching a table).
pub(crate) fn shape_form(word: &str) -> RecordKind {
    match word {
        "line" => RecordKind::Line,
        "rect" => RecordKind::Rect,
        "arc" => RecordKind::Arc,
        "circle" => RecordKind::Circle,
        "poly" => RecordKind::Poly,
        _ => panic!("shape form {word:?} was validated before reaching the table"),
    }
}

impl Table for Shapes {
    row_ops!(id, form, coords, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Shape
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert!(
            rec.fields.len() >= 5,
            "a shape has an id, a form and values"
        );
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.form.push(shape_form(rec.fields[1]));
        self.coords.push(pool.push_coords(&rec.fields[2..]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        sink.begin(RecordKind::Shape);
        sink.id(self.id[row], strings);
        sink.field(self.form[row].word());
        for &c in &pool.coords[self.coords[row].range()] {
            sink.coord(c);
        }
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
}

/// Text-only records: every kind whose fields are names, ids, words or expressions
/// (`param`, `impl`, `constraint`, `waive`, `prose`, `var`, `body`, and the bench kinds).
/// One table per kind, except `doc_blocks`, which holds `prose` and `var` interleaved in
/// document order.
///
/// `fields[row]` holds the record's positional values (at most 3; unused slots are
/// `Sym::NONE`), so the key is `fields[row][0]`: an id (`constraint`, `waive`, `prose`)
/// or a name. `waive`'s instance id is `fields[row][1]` as text (resolve with
/// [`ObjId::find`]). Attributes stay in the pool (param schema, impl formats, bench
/// settings). Capacities in `TableId::capacity`.
#[derive(Clone, Debug, Default)]
pub struct Records {
    /// Record kind of each row.
    pub kind: Vec<RecordKind>,
    /// Positional values.
    pub fields: Vec<[Sym; 3]>,
    /// Verbatim lines (`prose`, `body`, `code`).
    pub verbatim: Vec<Span>,
    /// Attribute lines.
    pub attrs: Vec<Span>,
    /// Comments.
    pub comments: Vec<Span>,
}

impl Table for Records {
    row_ops!(kind, fields, verbatim, attrs, comments);
    fn kind_at(&self, row: usize) -> RecordKind {
        self.kind[row]
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert!(
            rec.fields.len() <= 3,
            "text-only kinds have at most 3 fields"
        );
        let mut f = [Sym::NONE; 3];
        for (slot, text) in f.iter_mut().zip(rec.fields) {
            *slot = strings.intern(text);
        }
        self.kind.push(rec.kind);
        self.fields.push(f);
        self.verbatim.push(pool.push_lines(rec.verbatim, strings));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        let kind = self.kind[row];
        sink.begin(kind);
        let arity = kind.schema().fields.fixed.len();
        for &f in &self.fields[row][..arity] {
            sink.field(strings.get(f));
        }
        emit_attrs(self.attrs[row], strings, pool, sink);
        emit_lines(self.verbatim[row], strings, pool, sink, true);
        emit_lines(self.comments[row], strings, pool, sink, false);
    }
    fn verbatim_col(&self) -> &[Span] {
        &self.verbatim
    }
    fn id_at(&self, row: usize, strings: &Strings) -> ObjId {
        if self.kind[row].key() != KeyRule::Id {
            return ObjId::NONE;
        }
        let id = ObjId::find(strings.get(self.fields[row][0]), strings);
        id.expect("an id field was interned when the row was pushed")
    }
    fn name_at(&self, row: usize) -> Sym {
        match self.kind[row].key() {
            KeyRule::Name => self.fields[row][0],
            _ => Sym::NONE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_coordinate_parse_equals_the_format_reader() {
        let mut texts: Vec<String> = [
            "0",
            "-0",
            "1.5000",
            "007.010",
            "-12.5",
            "0.156",
            "2147483.647",
            "-2147483.648",
            "1.0",
        ]
        .map(String::from)
        .to_vec();
        let mut m: i64 = i64::from(i32::MIN);
        while m <= i64::from(i32::MAX) {
            let mut s = String::new();
            schemify_format::format_coord(m as i32, &mut s);
            texts.push(s);
            m += 7_919_993;
        }
        for t in &texts {
            assert_eq!(Ok(coord(t)), parse_coord(t), "{t}");
        }
    }
}
