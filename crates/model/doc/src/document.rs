//! One cell, bench or interface file as tables ([`Document`]) and its read API: table
//! slices, lookups by id, records as owned text, canonical bytes.
//! Cites: format spec §2 (canonical form), §4 (ranks and kinds), §5 (versions); D11
//! (existence-based tables).
//!
//! Load and save live in `load.rs`, the record-level edit primitives in `rows.rs`; both
//! are `impl Document` blocks over the fields declared here.
//!
//! Contents:
//! - `pub enum TableId`: `ALL`, `of`, `ordered`, `capacity`; `pub struct Loc`
//! - `pub struct Document`: `new`, `cursor`, `changes_since`, `kind`, `version`,
//!   `read_only`, `strings`, `pool`, `header_value`, `header_values`, `find_id`,
//!   `table_len`, `record`, `canonical`; crate: `header_record`
//! - one accessor per table: `pins`, `params`, `impls`, `graphics`, `instances`, `wires`,
//!   `labels`, `ports`, `noconns`, `texts`, `shapes`, `constraints`, `waivers`,
//!   `doc_blocks`, `bodies`, `corners`, `configs`, `sweeps`, `montecarlos`, `optimizes`,
//!   `outputs`, `specs`, `codes`
//! - `pub(crate) struct Loaded`
//! - crate macros `with_table!`, `with_table_mut!` (`#[macro_use]` in `lib.rs`, so every
//!   module declared after this one sees them)

use crate::Diag;
use crate::changes::{Change, ChangeLog, Cursor};
use crate::pool::{self, Pool, Span};
use crate::record::RecordBuf;
use crate::schematic::{Instances, Noconns, Placed, Shapes, Wires};
use crate::strings::{FastMap, ObjId, Strings, Sym};
use crate::tables::{Graphics, Pins, Records, Table};
use schemify_format::{FileKind, RecordKind, VERSION};

/// Identifies one table of a [`Document`]. Declaration order is rank order, so emitting
/// tables in this order gives the format writer a rank-ordered model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs, reason = "each variant is the table of the same name")]
pub enum TableId {
    Pins,
    Params,
    Impls,
    Graphics,
    Instances,
    Wires,
    Labels,
    Ports,
    Noconns,
    Texts,
    Shapes,
    Constraints,
    Waivers,
    DocBlocks,
    Bodies,
    Corners,
    Configs,
    Sweeps,
    Montecarlos,
    Optimizes,
    Outputs,
    Specs,
    Codes,
}

/// A row: table plus index. Valid until the next edit of that table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    /// The table.
    pub table: TableId,
    /// Row index.
    pub row: u32,
}

/// One cell, bench or interface file as tables. Read it through the table accessors;
/// only this crate mutates it (edit requests go through [`crate::apply_edits`]).
///
/// A `.shprim` simply has no rank-3 rows and a `.shbench` no symbol rows: which tables
/// have rows is the file kind (existence-based, D11). An interface loaded for library
/// browsing has rows only in the rank-0/1/2 tables.
#[derive(Clone, Debug)]
pub struct Document {
    pub(crate) kind: FileKind,
    pub(crate) version: u16,
    pub(crate) strings: Strings,
    pub(crate) pool: Pool,
    pub(crate) header: Span,
    pub(crate) pins: Pins,
    pub(crate) params: Records,
    pub(crate) impls: Records,
    pub(crate) graphics: Graphics,
    pub(crate) instances: Instances,
    pub(crate) wires: Wires,
    pub(crate) labels: Placed,
    pub(crate) ports: Placed,
    pub(crate) noconns: Noconns,
    pub(crate) texts: Placed,
    pub(crate) shapes: Shapes,
    pub(crate) constraints: Records,
    pub(crate) waivers: Records,
    pub(crate) doc_blocks: Records,
    pub(crate) bodies: Records,
    pub(crate) corners: Records,
    pub(crate) configs: Records,
    pub(crate) sweeps: Records,
    pub(crate) montecarlos: Records,
    pub(crate) optimizes: Records,
    pub(crate) outputs: Records,
    pub(crate) specs: Records,
    pub(crate) codes: Records,
    pub(crate) eof_comments: Vec<Sym>,
    /// Every id-bearing row, for `O(1)` lookup by id. Never iterated for output.
    pub(crate) by_id: FastMap<ObjId, Loc>,
    /// Every row write since the load, for consumers that follow edits (see [`Cursor`]).
    pub(crate) log: ChangeLog,
    /// Records of a newer format version this reader does not know (spec §5). Their
    /// presence makes the document read-only.
    pub(crate) unknown_records: u32,
}

/// Result of loading a format model into tables.
pub(crate) struct Loaded {
    pub(crate) doc: Document,
    pub(crate) diags: Vec<Diag>,
}

/// Dispatches `$body` over the concrete table selected by a [`TableId`]; `$tab` is a
/// shared reference to it. Static dispatch: one monomorphized arm per table.
macro_rules! with_table {
    ($doc:expr, $t:expr, |$tab:ident| $body:expr) => {
        with_table!(@ $doc, $t, $tab, $body, )
    };
    (@ $doc:expr, $t:expr, $tab:ident, $body:expr, $($m:tt)?) => {
        match $t {
            TableId::Pins => { let $tab = &$($m)? $doc.pins; $body }
            TableId::Params => { let $tab = &$($m)? $doc.params; $body }
            TableId::Impls => { let $tab = &$($m)? $doc.impls; $body }
            TableId::Graphics => { let $tab = &$($m)? $doc.graphics; $body }
            TableId::Instances => { let $tab = &$($m)? $doc.instances; $body }
            TableId::Wires => { let $tab = &$($m)? $doc.wires; $body }
            TableId::Labels => { let $tab = &$($m)? $doc.labels; $body }
            TableId::Ports => { let $tab = &$($m)? $doc.ports; $body }
            TableId::Noconns => { let $tab = &$($m)? $doc.noconns; $body }
            TableId::Texts => { let $tab = &$($m)? $doc.texts; $body }
            TableId::Shapes => { let $tab = &$($m)? $doc.shapes; $body }
            TableId::Constraints => { let $tab = &$($m)? $doc.constraints; $body }
            TableId::Waivers => { let $tab = &$($m)? $doc.waivers; $body }
            TableId::DocBlocks => { let $tab = &$($m)? $doc.doc_blocks; $body }
            TableId::Bodies => { let $tab = &$($m)? $doc.bodies; $body }
            TableId::Corners => { let $tab = &$($m)? $doc.corners; $body }
            TableId::Configs => { let $tab = &$($m)? $doc.configs; $body }
            TableId::Sweeps => { let $tab = &$($m)? $doc.sweeps; $body }
            TableId::Montecarlos => { let $tab = &$($m)? $doc.montecarlos; $body }
            TableId::Optimizes => { let $tab = &$($m)? $doc.optimizes; $body }
            TableId::Outputs => { let $tab = &$($m)? $doc.outputs; $body }
            TableId::Specs => { let $tab = &$($m)? $doc.specs; $body }
            TableId::Codes => { let $tab = &$($m)? $doc.codes; $body }
        }
    };
}

/// As [`with_table!`] with a mutable table plus the document's strings and pool.
macro_rules! with_table_mut {
    ($doc:expr, $t:expr, |$tab:ident, $strings:ident, $pool:ident| $body:expr) => {{
        let Document {
            strings: $strings,
            pool: $pool,
            ..
        } = $doc;
        let _ = (&$strings, &$pool);
        with_table!(@ $doc, $t, $tab, $body, mut)
    }};
}

// `TableId::ALL` lists every variant once, in declaration (rank) order.
const _: () = {
    assert!(TableId::ALL.len() == TableId::Codes as usize + 1);
    let mut i = 0;
    while i < TableId::ALL.len() {
        assert!(TableId::ALL[i] as usize == i, "ALL is in declaration order");
        i += 1;
    }
};

impl TableId {
    /// Every table, in rank order.
    pub const ALL: [TableId; 23] = [
        TableId::Pins,
        TableId::Params,
        TableId::Impls,
        TableId::Graphics,
        TableId::Instances,
        TableId::Wires,
        TableId::Labels,
        TableId::Ports,
        TableId::Noconns,
        TableId::Texts,
        TableId::Shapes,
        TableId::Constraints,
        TableId::Waivers,
        TableId::DocBlocks,
        TableId::Bodies,
        TableId::Corners,
        TableId::Configs,
        TableId::Sweeps,
        TableId::Montecarlos,
        TableId::Optimizes,
        TableId::Outputs,
        TableId::Specs,
        TableId::Codes,
    ];

    /// Returns the table holding records of `kind`; `None` for the header and unknown
    /// records, and for project/lock/config kinds.
    #[must_use]
    pub fn of(kind: RecordKind) -> Option<TableId> {
        use RecordKind as K;
        let t = match kind {
            K::Pin => TableId::Pins,
            K::Param => TableId::Params,
            K::Impl => TableId::Impls,
            K::Line | K::Rect | K::Arc | K::Circle | K::Poly | K::Stext => TableId::Graphics,
            K::Inst => TableId::Instances,
            K::Wire => TableId::Wires,
            K::Label => TableId::Labels,
            K::Port => TableId::Ports,
            K::Noconn => TableId::Noconns,
            K::Text => TableId::Texts,
            K::Shape => TableId::Shapes,
            K::Constraint => TableId::Constraints,
            K::Waive => TableId::Waivers,
            K::Prose | K::Var => TableId::DocBlocks,
            K::Body => TableId::Bodies,
            K::Corner => TableId::Corners,
            K::Config => TableId::Configs,
            K::Sweep => TableId::Sweeps,
            K::Montecarlo => TableId::Montecarlos,
            K::Optimize => TableId::Optimizes,
            K::Output => TableId::Outputs,
            K::Spec => TableId::Specs,
            K::Code => TableId::Codes,
            _ => return None,
        };
        Some(t)
    }

    /// Returns whether row order is file order (ranks 1, 2, 4, 5). Rows of the other
    /// tables are unordered; the writer sorts them by key.
    #[must_use]
    pub fn ordered(self) -> bool {
        matches!(
            self,
            TableId::Pins
                | TableId::Params
                | TableId::Impls
                | TableId::Graphics
                | TableId::DocBlocks
                | TableId::Bodies
        )
    }

    /// Returns the row capacity (architecture table capacities); an insert past it is
    /// refused with `E043`, a load past it panics.
    #[must_use]
    pub fn capacity(self) -> u32 {
        match self {
            TableId::Params => 1 << 8,
            TableId::Impls | TableId::Bodies => 1 << 4,
            TableId::Pins | TableId::DocBlocks => 1 << 12,
            TableId::Graphics | TableId::Shapes | TableId::Constraints | TableId::Waivers => {
                1 << 16
            }
            TableId::Instances
            | TableId::Wires
            | TableId::Labels
            | TableId::Ports
            | TableId::Noconns
            | TableId::Texts => 1 << 22,
            TableId::Corners
            | TableId::Configs
            | TableId::Sweeps
            | TableId::Montecarlos
            | TableId::Optimizes
            | TableId::Outputs
            | TableId::Specs
            | TableId::Codes => 1 << 12,
        }
    }
}

impl Document {
    /// Creates an empty document of `kind` at the current format version.
    ///
    /// # Panics
    ///
    /// Panics unless `kind` is a cell file kind (`.shprim`, `.shcomp`, `.shbench`).
    #[must_use]
    pub fn new(kind: FileKind) -> Document {
        assert!(
            matches!(
                kind,
                FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
            ),
            "documents are cell files"
        );
        let doc = Document {
            kind,
            version: VERSION,
            strings: Strings::default(),
            pool: Pool::default(),
            header: Span::default(),
            pins: Pins::default(),
            params: Records::default(),
            impls: Records::default(),
            graphics: Graphics::default(),
            instances: Instances::default(),
            wires: Wires::default(),
            labels: Placed::new(RecordKind::Label),
            ports: Placed::new(RecordKind::Port),
            noconns: Noconns::default(),
            texts: Placed::new(RecordKind::Text),
            shapes: Shapes::default(),
            constraints: Records::default(),
            waivers: Records::default(),
            doc_blocks: Records::default(),
            bodies: Records::default(),
            corners: Records::default(),
            configs: Records::default(),
            sweeps: Records::default(),
            montecarlos: Records::default(),
            optimizes: Records::default(),
            outputs: Records::default(),
            specs: Records::default(),
            codes: Records::default(),
            eof_comments: Vec::new(),
            by_id: FastMap::default(),
            log: ChangeLog::default(),
            unknown_records: 0,
        };
        assert!(!doc.read_only(), "a new document is at the current version");
        doc
    }

    /// Where the change log ends now: [`Document::changes_since`] of it lists the row
    /// writes made after this call.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.log.cursor()
    }

    /// The row writes made since `c` was taken, oldest first (a row can appear more than
    /// once), or `None` when the log no longer reaches `c`: it was taken on another
    /// document (a reload, a merge and a copy are other documents), more than
    /// [`LOG_MAX`](crate::LOG_MAX) changes ago, or before a compaction renumbered the
    /// tables. A consumer then rebuilds from the tables.
    #[must_use]
    pub fn changes_since(&self, c: Cursor) -> Option<&[Change]> {
        let rows = self.log.since(c);
        debug_assert!(rows.is_none_or(|r| r.iter().all(|x| x.table.capacity() > x.row)));
        rows
    }

    /// Returns the file kind.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// Returns the header version. Above [`VERSION`] the document is read-only.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.version
    }

    /// Returns whether edits and saves are refused because the file comes from a newer
    /// format version and holds records this reader cannot keep (`E044`).
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.version > VERSION
    }

    /// Returns the string arena every `Sym` column of this document indexes.
    #[must_use]
    pub fn strings(&self) -> &Strings {
        &self.strings
    }

    /// Returns the pool every `Span` column of this document indexes.
    #[must_use]
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Returns the value of header attribute `key` (first value), e.g. `default`.
    #[must_use]
    pub fn header_value(&self, key: &str) -> Option<&str> {
        let v = self.pool.find_attr(&self.strings, self.header, key)?;
        v.first().map(|s| self.strings.get(*s))
    }

    /// Returns every value of header attribute `key` (`vinputs` lists names), empty when
    /// the header has none. Allocates the list.
    #[must_use]
    pub fn header_values(&self, key: &str) -> Vec<&str> {
        self.pool
            .find_attr(&self.strings, self.header, key)
            .map_or_else(Vec::new, |v| {
                v.iter().map(|s| self.strings.get(*s)).collect()
            })
    }

    /// `pin` rows in port order.
    #[must_use]
    pub fn pins(&self) -> &Pins {
        &self.pins
    }
    /// `param` rows (the cell's parameter schema), in file order.
    #[must_use]
    pub fn params(&self) -> &Records {
        &self.params
    }
    /// `impl` rows, in file order.
    #[must_use]
    pub fn impls(&self) -> &Records {
        &self.impls
    }
    /// Symbol graphics in draw order.
    #[must_use]
    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }
    /// `inst` rows (unordered).
    #[must_use]
    pub fn instances(&self) -> &Instances {
        &self.instances
    }
    /// `wire` rows (unordered).
    #[must_use]
    pub fn wires(&self) -> &Wires {
        &self.wires
    }
    /// `label` rows (unordered).
    #[must_use]
    pub fn labels(&self) -> &Placed {
        &self.labels
    }
    /// `port` rows (unordered).
    #[must_use]
    pub fn ports(&self) -> &Placed {
        &self.ports
    }
    /// `noconn` rows (unordered).
    #[must_use]
    pub fn noconns(&self) -> &Noconns {
        &self.noconns
    }
    /// `text` rows (unordered).
    #[must_use]
    pub fn texts(&self) -> &Placed {
        &self.texts
    }
    /// `shape` rows: schematic graphics (unordered).
    #[must_use]
    pub fn shapes(&self) -> &Shapes {
        &self.shapes
    }
    /// `constraint` rows (unordered, D30).
    #[must_use]
    pub fn constraints(&self) -> &Records {
        &self.constraints
    }
    /// `waive` rows (unordered, D13): one row per waived (instance, param).
    #[must_use]
    pub fn waivers(&self) -> &Records {
        &self.waivers
    }
    /// `prose` and `var` rows in document order (D19).
    #[must_use]
    pub fn doc_blocks(&self) -> &Records {
        &self.doc_blocks
    }
    /// `body` rows, in file order.
    #[must_use]
    pub fn bodies(&self) -> &Records {
        &self.bodies
    }
    /// Bench `corner` rows.
    #[must_use]
    pub fn corners(&self) -> &Records {
        &self.corners
    }
    /// Bench `config` rows (D11a).
    #[must_use]
    pub fn configs(&self) -> &Records {
        &self.configs
    }
    /// Bench `sweep` rows.
    #[must_use]
    pub fn sweeps(&self) -> &Records {
        &self.sweeps
    }
    /// Bench `montecarlo` rows.
    #[must_use]
    pub fn montecarlos(&self) -> &Records {
        &self.montecarlos
    }
    /// Bench `optimize` rows.
    #[must_use]
    pub fn optimizes(&self) -> &Records {
        &self.optimizes
    }
    /// Bench `output` rows.
    #[must_use]
    pub fn outputs(&self) -> &Records {
        &self.outputs
    }
    /// Bench `spec` rows.
    #[must_use]
    pub fn specs(&self) -> &Records {
        &self.specs
    }
    /// Bench `code` rows.
    #[must_use]
    pub fn codes(&self) -> &Records {
        &self.codes
    }

    /// Returns the row holding the record with id `id`, in any table.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: the id index points past the end of a table.
    #[must_use]
    pub fn find_id(&self, id: ObjId) -> Option<Loc> {
        let loc = self.by_id.get(&id).copied();
        assert!(loc.is_none_or(|l| (l.row as usize) < self.table_len(l.table)));
        loc
    }

    /// Returns the number of rows in table `t`.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a table holds more rows than its capacity.
    #[must_use]
    pub fn table_len(&self, t: TableId) -> usize {
        let len = with_table!(self, t, |tab| tab.len());
        assert!(
            len <= t.capacity() as usize,
            "{t:?} holds more rows than its capacity"
        );
        len
    }

    /// Returns the record at `loc` as owned text (cold path: allocates).
    ///
    /// # Panics
    ///
    /// Panics on a stale `loc` (its table has no such row).
    #[must_use]
    pub fn record(&self, loc: Loc) -> RecordBuf {
        assert!((loc.row as usize) < self.table_len(loc.table), "stale row");
        let mut rec = RecordBuf::new(RecordKind::Header);
        with_table!(self, loc.table, |tab| tab.emit(
            loc.row as usize,
            &self.strings,
            &self.pool,
            &mut rec
        ));
        assert!(rec.kind != RecordKind::Header);
        rec
    }

    /// Returns the canonical file bytes (format spec §2) of this document. Cost: one
    /// write of every row.
    ///
    /// # Panics
    ///
    /// Panics on a read-only document (it lost the records it cannot model).
    #[must_use]
    pub fn canonical(&self) -> String {
        let text = schemify_format::write(&self.to_format());
        assert!(text.starts_with("schemify "));
        text
    }

    /// Returns the header as a record (fields empty, attributes in order).
    pub(crate) fn header_record(&self) -> RecordBuf {
        let mut rec = RecordBuf::new(RecordKind::Header);
        pool::emit_attrs(self.header, &self.strings, &self.pool, &mut rec);
        assert_eq!(rec.attrs.len(), self.header.len as usize);
        rec
    }
}
