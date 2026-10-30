//! Schematic records (rank 3: `inst`, `wire`, `label`, `port`, `text`, `noconn`,
//! `shape`) → their unordered column-major tables, and back. The six questions and the
//! row-order rules are answered once in `tables.rs`.
//! Cites: format spec §4 rank 3, D33 (i32 milli-unit coordinates), D40 (instance
//! overrides).
//!
//! Contents:
//! - `pub struct Instances`: `is_override`; `pub struct Wires`
//! - `pub struct Placed`: crate `new`; `pub struct Noconns`, `pub struct Shapes`

use crate::pool::{self, Pool, RecView, Sink, Span};
use crate::record::INST_RESERVED;
use crate::strings::{ObjId, Strings, Sym};
use crate::tables::{self, Owned, Table};
use schemify_format::{Orient, RecordKind};

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

impl Instances {
    /// Returns whether an attribute key of an instance is a parameter override
    /// (anything but `at`, `name`, `inherit`, `ignore`).
    #[must_use]
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
        let name = rec.attr("name").expect("`name` is required").texts;
        assert_eq!(at.texts.len(), 3, "`at` is x, y, orientation");
        assert_eq!(name.len(), 1, "`name` is one value");
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.cell.push(strings.intern(rec.fields[1]));
        self.x.push(at.milli[0]);
        self.y.push(at.milli[1]);
        self.orient.push(tables::orient(at.texts[2]));
        self.name.push(strings.intern(name[0]));
        self.attrs
            .push(pool.push_attrs(rec, &["at", "name"], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(row < self.len(), "stale row");
        assert!(self.id[row] != ObjId::NONE, "every instance has an id");
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
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
}

impl Table for Wires {
    row_ops!(id, x1, y1, x2, y2, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Wire
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 5);
        let (mut a, mut b) = (
            (rec.field_milli[1], rec.field_milli[2]),
            (rec.field_milli[3], rec.field_milli[4]),
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
        assert!(a <= b, "endpoints are normalized (spec §4)");
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(
            (self.x1[row], self.y1[row]) <= (self.x2[row], self.y2[row]),
            "endpoints are normalized (spec §4)"
        );
        assert!(self.id[row] != ObjId::NONE, "every wire has an id");
        sink.begin(RecordKind::Wire);
        sink.id(self.id[row], strings);
        for c in [self.x1[row], self.y1[row], self.x2[row], self.y2[row]] {
            sink.coord(c);
        }
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
}

impl Placed {
    pub(crate) fn new(kind: RecordKind) -> Placed {
        assert!(matches!(
            kind,
            RecordKind::Label | RecordKind::Port | RecordKind::Text
        ));
        let placed = Placed {
            kind,
            id: Vec::new(),
            x: Vec::new(),
            y: Vec::new(),
            orient: Vec::new(),
            text: Vec::new(),
            attrs: Vec::new(),
            comments: Vec::new(),
        };
        assert!(placed.columns_ok());
        placed
    }
}

impl Table for Placed {
    row_ops!(id, x, y, orient, text, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        self.kind
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.kind, self.kind, "each placed table holds one kind");
        assert_eq!(rec.fields.len(), 5);
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.x.push(rec.field_milli[1]);
        self.y.push(rec.field_milli[2]);
        self.orient.push(tables::orient(rec.fields[3]));
        self.text.push(strings.intern(rec.fields[4]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(row < self.len(), "stale row");
        assert!(self.id[row] != ObjId::NONE, "every placed record has an id");
        sink.begin(self.kind);
        sink.id(self.id[row], strings);
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        sink.field(self.orient[row].as_str());
        sink.field(strings.get(self.text[row]));
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
}

impl Table for Noconns {
    row_ops!(id, x, y, attrs, comments);
    fn kind_at(&self, _row: usize) -> RecordKind {
        RecordKind::Noconn
    }
    fn push(&mut self, rec: &RecView<'_>, strings: &mut Strings, pool: &mut Pool) {
        assert_eq!(rec.fields.len(), 3);
        self.id.push(ObjId::intern(rec.fields[0], strings));
        self.x.push(rec.field_milli[1]);
        self.y.push(rec.field_milli[2]);
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(row < self.len(), "stale row");
        assert!(self.id[row] != ObjId::NONE, "every noconn has an id");
        sink.begin(RecordKind::Noconn);
        sink.id(self.id[row], strings);
        sink.coord(self.x[row]);
        sink.coord(self.y[row]);
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
}

impl Table for Shapes {
    row_ops!(id, form, coords, attrs, comments; coords);
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
        self.coords.push(pool.push_coords(&rec.field_milli[2..]));
        self.attrs.push(pool.push_attrs(rec, &[], strings));
        self.comments.push(pool.push_lines(rec.comments, strings));
        assert!(self.columns_ok());
    }
    fn emit<S: Sink>(&self, row: usize, strings: &Strings, pool: &Pool, sink: &mut S) {
        assert!(
            self.coords[row].len >= 3,
            "a shape has values after its form"
        );
        assert!(self.id[row] != ObjId::NONE, "every shape has an id");
        sink.begin(RecordKind::Shape);
        sink.id(self.id[row], strings);
        sink.field(self.form[row].word());
        for &c in &pool.coords[self.coords[row].range()] {
            sink.coord(c);
        }
        pool::emit_attrs(self.attrs[row], strings, pool, sink);
        pool::emit_comments(self.comments[row], strings, pool, sink);
    }
    fn id_at(&self, row: usize, _strings: &Strings) -> ObjId {
        self.id[row]
    }
}

// ---- private ----

/// The rank-2 kind a `shape` form word names.
///
/// # Panics
///
/// Panics on a word outside `format::SHAPE_FORMS` (validated before reaching a table).
fn shape_form(word: &str) -> RecordKind {
    match word {
        "line" => RecordKind::Line,
        "rect" => RecordKind::Rect,
        "arc" => RecordKind::Arc,
        "circle" => RecordKind::Circle,
        "poly" => RecordKind::Poly,
        _ => panic!("shape form {word:?} was validated before reaching the table"),
    }
}
