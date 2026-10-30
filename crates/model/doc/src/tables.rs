//! Records of one document → typed column-major (SoA) tables per record family, and
//! back (each table pushes a [`RecView`] and emits a row into a [`Sink`]).
//! Cites: format spec §4 (record kinds and ranks), D11 (existence-based tables), D33
//! (i32 milli-unit coordinates), D40 (instance overrides).
//!
//! Six questions, answered once for all tables here and per table in its doc:
//! 1. In: format records (load) or validated `RecordBuf`s (edits). Out: column slices
//!    for `elab`, `render`, `plugin`; format records again on save.
//! 2. Rank-3 tables hold up to 2^22 rows each (architecture: objects per cell); symbol
//!    and bench tables up to 2^12; capacities are [`crate::TableId::capacity`].
//! 3. Coordinates are i32 milli-units; text is a [`Sym`] (`u32`); ids are [`ObjId`]
//!    (`u32`); orientation and pin direction are `u8` enums; pool offsets `u32`.
//! 4. Consumers scan whole columns (connectivity reads `x1..y2` of every wire); edits
//!    touch one row. Attributes are read per row through its [`Span`].
//! 5. Lifetime: the document. Pool entries of replaced rows become garbage that
//!    [`crate::Document`] reclaims by rebuilding.
//! 6. Read-parallel: consumers get `&` slices; `doc` is the single writer.
//!
//! Row order: tables of ranks 1, 2, 4 and 5 are in file order (it is semantic: port
//! order, draw order, document order). Rank-3 and rank-6 tables are unordered; the
//! writer sorts them. Row indices are valid until the next edit of that table.
//!
//! Contents:
//! - `pub(crate) trait Table`, `pub(crate) struct Owned`, crate macro `row_ops!`
//!   (`#[macro_use]` in `lib.rs`, so `schematic` sees it)
//! - `pub enum PinDir`: `as_str`; `pub struct Pins`, `pub struct Graphics`
//! - `pub struct Records`
//! - crate: `fn move_elem`, `fn orient`
//!
//! The rank-3 tables live in `schematic.rs`.

use crate::pool::{self, Pool, RecView, Sink, Span};
use crate::strings::{ObjId, Strings, Sym};
use schemify_format::{KeyRule, Orient, RecordKind};

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
    /// The pool entries `row` owns: what replacing or removing it leaves as garbage.
    fn owned(&self, row: usize) -> Owned;
    /// The row's id, or `ObjId::NONE` for kinds without one.
    fn id_at(&self, _row: usize, _strings: &Strings) -> ObjId {
        ObjId::NONE
    }
    /// The row's name, or `Sym::NONE` for kinds without one.
    fn name_at(&self, _row: usize) -> Sym {
        Sym::NONE
    }
    /// Whether every column has `len()` rows (the SoA invariant).
    fn columns_ok(&self) -> bool;
}

/// The pool entries one row owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owned {
    /// The row's attribute lines (their values counted through the pool).
    pub(crate) attrs: Span,
    /// Its other entries: comments, verbatim lines, coordinates.
    pub(crate) others: u32,
}

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

macro_rules! row_ops {
    ($($col:ident),+ $(; $($span:ident),+)?) => {
        fn len(&self) -> usize {
            self.attrs.len()
        }
        fn swap_remove_row(&mut self, row: usize) {
            let len = self.len();
            assert!(row < len, "stale row");
            $( self.$col.swap_remove(row); )+
            assert!(self.columns_ok());
            assert_eq!(self.len(), len - 1);
        }
        fn remove_row(&mut self, row: usize) {
            let len = self.len();
            assert!(row < len, "stale row");
            $( self.$col.remove(row); )+
            assert!(self.columns_ok());
            assert_eq!(self.len(), len - 1);
        }
        fn move_row(&mut self, from: usize, to: usize) {
            let len = self.len();
            assert!(from < len, "stale source row");
            assert!(to < len, "stale destination row");
            $( $crate::tables::move_elem(&mut self.$col, from, to); )+
            assert!(self.columns_ok());
            assert_eq!(self.len(), len, "a move keeps every row");
        }
        fn owned(&self, row: usize) -> Owned {
            assert!(row < self.len(), "stale row");
            Owned {
                attrs: self.attrs[row],
                others: self.comments[row].len $($( + self.$span[row].len )+)?,
            }
        }
        fn columns_ok(&self) -> bool {
            let n = self.attrs.len();
            true $( && self.$col.len() == n )+
        }
    };
}

impl PinDir {
    /// Returns the file word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        ["in", "out", "inout"][self as usize]
    }
}

impl Table for Pins {
    row_ops!(name, dir, x, y, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Pin
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 4);
        self.name.push(strings.intern(rec.fields[0]));
        self.dir.push(PinDir::from_word(rec.fields[1]));
        self.x.push(rec.field_milli[2]);
        self.y.push(rec.field_milli[3]);
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(row < self.len(), "stale row");
        sink.begin(RecordKind::Pin);
        sink.field(strings.get(self.name[row]));
        sink.field(self.dir[row].as_str());
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn name_at(&self, row: usize) -> Sym {
        self.name[row]
    }
}

impl Table for Graphics {
    row_ops!(shape, coords, text, orient, angles, attrs, comments; coords);
    fn kind_at(&self, row: usize) -> RecordKind {
        self.shape[row]
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        let (f, m) = (rec.fields, rec.field_milli);
        let (coords, text, orient, angles) = match rec.kind {
            RecordKind::Arc => {
                assert_eq!(f.len(), 5);
                let a = [strings.intern(f[3]), strings.intern(f[4])];
                (pool.push_coords(&m[..3]), Sym::NONE, Orient::R0, a)
            }
            RecordKind::Stext => {
                assert_eq!(f.len(), 4);
                let t = strings.intern(f[3]);
                (pool.push_coords(&m[..2]), t, orient(f[2]), [Sym::NONE; 2])
            }
            RecordKind::Line | RecordKind::Rect | RecordKind::Circle | RecordKind::Poly => {
                (pool.push_coords(m), Sym::NONE, Orient::R0, [Sym::NONE; 2])
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
        assert_eq!(text == Sym::NONE, rec.kind != RecordKind::Stext);
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        let kind = self.shape[row];
        let coords = self.coords[row].len;
        match kind {
            RecordKind::Arc => assert_eq!(coords, 3, "an arc stores cx cy r"),
            RecordKind::Stext => assert_eq!(coords, 2, "an stext stores x y"),
            RecordKind::Poly => assert!(coords.is_multiple_of(2), "a poly stores x y pairs"),
            _ => {}
        }
        assert_eq!(
            self.angles[row][0] == Sym::NONE,
            kind != RecordKind::Arc,
            "only arcs have angles"
        );
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
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
}

impl Table for Records {
    row_ops!(kind, fields, verbatim, attrs, comments; verbatim);
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
        assert!(arity <= 3, "text-only kinds have at most 3 fields");
        debug_assert!(
            self.fields[row][arity..].iter().all(|&f| f == Sym::NONE),
            "slots past the arity are unused"
        );
        for &f in &self.fields[row][..arity] {
            sink.field(strings.get(f));
        }
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        for &l in &pool.lines[self.verbatim[row].range()] {
            sink.verbatim(strings.get(l));
        }
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, strings: &Strings) -> ObjId {
        if self.kind[row].key() != KeyRule::Id {
            return ObjId::NONE;
        }
        assert!(
            self.fields[row][0] != Sym::NONE,
            "an id-keyed row has its id field"
        );
        let id = ObjId::find(strings.get(self.fields[row][0]), strings);
        id.expect("an id field was interned when the row was pushed")
    }
    fn name_at(&self, row: usize) -> Sym {
        match self.kind[row].key() {
            KeyRule::Name => self.fields[row][0],
            KeyRule::Id | KeyRule::Position | KeyRule::Header | KeyRule::TypeName => Sym::NONE,
        }
    }
}

pub(crate) fn move_elem<T>(v: &mut [T], from: usize, to: usize) {
    assert!(from < v.len());
    assert!(to < v.len());
    if from < to {
        v[from..=to].rotate_left(1);
    } else {
        v[to..=from].rotate_right(1);
    }
}

pub(crate) fn orient(text: &str) -> Orient {
    Orient::parse(text).expect("orientations are validated before they reach a table")
}

// ---- private ----

impl PinDir {
    fn from_word(word: &str) -> PinDir {
        match word {
            "in" => PinDir::In,
            "out" => PinDir::Out,
            "inout" => PinDir::Inout,
            _ => panic!("pin dir {word:?} was validated before reaching the table"),
        }
    }
}
