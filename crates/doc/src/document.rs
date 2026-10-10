//! One file's design data as tables ([`Document`]), its load from a format model
//! (with the semantic checks the format layer does not do), its save back to a format
//! model, and the record-level primitives every edit op is built from.
//!
//! Load spine: header attributes → one `push` per record into its table → `reid_dups`
//! (`W001`) → `load_diags` (`E024` duplicate overrides, `E042` dangling references).
//! Save spine: header → every table in rank order → end-of-file comments; the format
//! writer then sorts ranks 3 and 6.

use crate::record::{Attr, RecordBuf, check_record};
use crate::strings::{FastMap, ObjId, Strings, Sym};
use crate::tables::{
    FormatSink, Graphics, Instances, Noconns, Pins, Placed, Pool, Records, Scratch, Span, Table,
};
use crate::{Code, Diag};
use schemify_format::{
    Document as FormatDoc, FileKind, IdRng, KeyRule, RecordKind, VERSION, fresh_id,
};

/// Identifies one table of a [`Document`]. Declaration order is rank order, so emitting
/// tables in this order gives the format writer a rank-ordered model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs)] // each variant is the table of the same name
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

impl TableId {
    /// Every table, in rank order.
    pub const ALL: [TableId; 22] = [
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

    /// Returns the row capacity (ARCHITECTURE table capacities).
    pub fn capacity(self) -> u32 {
        match self {
            TableId::Params => 1 << 8,
            TableId::Impls | TableId::Bodies => 1 << 4,
            TableId::Pins | TableId::DocBlocks => 1 << 12,
            TableId::Graphics | TableId::Constraints | TableId::Waivers => 1 << 16,
            TableId::Instances
            | TableId::Wires
            | TableId::Labels
            | TableId::Ports
            | TableId::Noconns
            | TableId::Texts => 1 << 22,
            _ => 1 << 12,
        }
    }
}

/// A row: table plus index. Valid until the next edit of that table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    /// The table.
    pub table: TableId,
    /// Row index.
    pub row: u32,
}

/// Dispatches `$body` over the concrete table selected by a [`TableId`]; `$tab` is a
/// shared reference to it. Static dispatch: one monomorphized arm per table.
macro_rules! with_table {
    ($doc:expr, $t:expr, |$tab:ident| $body:expr) => {
        match $t {
            TableId::Pins => {
                let $tab = &$doc.pins;
                $body
            }
            TableId::Params => {
                let $tab = &$doc.params;
                $body
            }
            TableId::Impls => {
                let $tab = &$doc.impls;
                $body
            }
            TableId::Graphics => {
                let $tab = &$doc.graphics;
                $body
            }
            TableId::Instances => {
                let $tab = &$doc.instances;
                $body
            }
            TableId::Wires => {
                let $tab = &$doc.wires;
                $body
            }
            TableId::Labels => {
                let $tab = &$doc.labels;
                $body
            }
            TableId::Ports => {
                let $tab = &$doc.ports;
                $body
            }
            TableId::Noconns => {
                let $tab = &$doc.noconns;
                $body
            }
            TableId::Texts => {
                let $tab = &$doc.texts;
                $body
            }
            TableId::Constraints => {
                let $tab = &$doc.constraints;
                $body
            }
            TableId::Waivers => {
                let $tab = &$doc.waivers;
                $body
            }
            TableId::DocBlocks => {
                let $tab = &$doc.doc_blocks;
                $body
            }
            TableId::Bodies => {
                let $tab = &$doc.bodies;
                $body
            }
            TableId::Corners => {
                let $tab = &$doc.corners;
                $body
            }
            TableId::Configs => {
                let $tab = &$doc.configs;
                $body
            }
            TableId::Sweeps => {
                let $tab = &$doc.sweeps;
                $body
            }
            TableId::Montecarlos => {
                let $tab = &$doc.montecarlos;
                $body
            }
            TableId::Optimizes => {
                let $tab = &$doc.optimizes;
                $body
            }
            TableId::Outputs => {
                let $tab = &$doc.outputs;
                $body
            }
            TableId::Specs => {
                let $tab = &$doc.specs;
                $body
            }
            TableId::Codes => {
                let $tab = &$doc.codes;
                $body
            }
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
        match $t {
            TableId::Pins => {
                let $tab = &mut $doc.pins;
                $body
            }
            TableId::Params => {
                let $tab = &mut $doc.params;
                $body
            }
            TableId::Impls => {
                let $tab = &mut $doc.impls;
                $body
            }
            TableId::Graphics => {
                let $tab = &mut $doc.graphics;
                $body
            }
            TableId::Instances => {
                let $tab = &mut $doc.instances;
                $body
            }
            TableId::Wires => {
                let $tab = &mut $doc.wires;
                $body
            }
            TableId::Labels => {
                let $tab = &mut $doc.labels;
                $body
            }
            TableId::Ports => {
                let $tab = &mut $doc.ports;
                $body
            }
            TableId::Noconns => {
                let $tab = &mut $doc.noconns;
                $body
            }
            TableId::Texts => {
                let $tab = &mut $doc.texts;
                $body
            }
            TableId::Constraints => {
                let $tab = &mut $doc.constraints;
                $body
            }
            TableId::Waivers => {
                let $tab = &mut $doc.waivers;
                $body
            }
            TableId::DocBlocks => {
                let $tab = &mut $doc.doc_blocks;
                $body
            }
            TableId::Bodies => {
                let $tab = &mut $doc.bodies;
                $body
            }
            TableId::Corners => {
                let $tab = &mut $doc.corners;
                $body
            }
            TableId::Configs => {
                let $tab = &mut $doc.configs;
                $body
            }
            TableId::Sweeps => {
                let $tab = &mut $doc.sweeps;
                $body
            }
            TableId::Montecarlos => {
                let $tab = &mut $doc.montecarlos;
                $body
            }
            TableId::Optimizes => {
                let $tab = &mut $doc.optimizes;
                $body
            }
            TableId::Outputs => {
                let $tab = &mut $doc.outputs;
                $body
            }
            TableId::Specs => {
                let $tab = &mut $doc.specs;
                $body
            }
            TableId::Codes => {
                let $tab = &mut $doc.codes;
                $body
            }
        }
    }};
}

/// One cell, bench or interface file as tables. Read it through the table accessors;
/// only this crate mutates it (edit requests go through [`crate::apply_edits`]).
///
/// A `.shprim` simply has no rank-3 rows and a `.shbench` no symbol rows: which tables
/// have rows is the file kind (existence-based, D11). An interface loaded for library
/// browsing has rows only in the rank-0/1 tables.
#[derive(Clone, Debug)]
pub struct Document {
    kind: FileKind,
    version: u16,
    pub(crate) strings: Strings,
    pub(crate) pool: Pool,
    pub(crate) header: Span,
    pub(crate) pins: Pins,
    pub(crate) params: Records,
    pub(crate) impls: Records,
    pub(crate) graphics: Graphics,
    pub(crate) instances: Instances,
    pub(crate) wires: crate::tables::Wires,
    pub(crate) labels: Placed,
    pub(crate) ports: Placed,
    pub(crate) noconns: Noconns,
    pub(crate) texts: Placed,
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
    by_id: FastMap<ObjId, Loc>,
    /// Records of a newer format version this reader does not know (spec §5). Their
    /// presence makes the document read-only.
    unknown_records: u32,
}

/// Result of loading a format model into tables.
pub(crate) struct Loaded {
    pub(crate) doc: Document,
    pub(crate) diags: Vec<Diag>,
}

impl Document {
    /// Creates an empty document of `kind` at the current format version.
    pub fn new(kind: FileKind) -> Document {
        assert!(
            matches!(
                kind,
                FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
            ),
            "documents are cell files"
        );
        Document {
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
            wires: crate::tables::Wires::default(),
            labels: Placed::new(RecordKind::Label),
            ports: Placed::new(RecordKind::Port),
            noconns: Noconns::default(),
            texts: Placed::new(RecordKind::Text),
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
            unknown_records: 0,
        }
    }

    // -------------------------------------------------------------- read API

    /// Returns the file kind.
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// Returns the header version. Above [`VERSION`] the document is read-only.
    pub fn version(&self) -> u16 {
        self.version
    }

    /// Returns whether edits and saves are refused because the file comes from a newer
    /// format version and holds records this reader cannot keep (`E044`).
    pub fn read_only(&self) -> bool {
        self.version > VERSION
    }

    /// Returns the string arena every `Sym` column of this document indexes.
    pub fn strings(&self) -> &Strings {
        &self.strings
    }

    /// Returns the pool every `Span` column of this document indexes.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Returns the header's attribute lines (`doc`, `default`, `prefix`, ...).
    pub fn header_attrs(&self) -> Span {
        self.header
    }

    /// Returns the value of header attribute `key` (first value), e.g. `default`.
    pub fn header_value(&self, key: &str) -> Option<&str> {
        let v = self.pool.find_attr(&self.strings, self.header, key)?;
        v.first().map(|s| self.strings.get(*s))
    }

    /// `pin` rows in port order.
    pub fn pins(&self) -> &Pins {
        &self.pins
    }
    /// `param` rows (the cell's parameter schema), in file order.
    pub fn params(&self) -> &Records {
        &self.params
    }
    /// `impl` rows, in file order.
    pub fn impls(&self) -> &Records {
        &self.impls
    }
    /// Symbol graphics in draw order.
    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }
    /// `inst` rows (unordered).
    pub fn instances(&self) -> &Instances {
        &self.instances
    }
    /// `wire` rows (unordered).
    pub fn wires(&self) -> &crate::tables::Wires {
        &self.wires
    }
    /// `label` rows (unordered).
    pub fn labels(&self) -> &Placed {
        &self.labels
    }
    /// `port` rows (unordered).
    pub fn ports(&self) -> &Placed {
        &self.ports
    }
    /// `noconn` rows (unordered).
    pub fn noconns(&self) -> &Noconns {
        &self.noconns
    }
    /// `text` rows (unordered).
    pub fn texts(&self) -> &Placed {
        &self.texts
    }
    /// `constraint` rows (unordered, D30).
    pub fn constraints(&self) -> &Records {
        &self.constraints
    }
    /// `waive` rows (unordered, D13): one row per waived (instance, param).
    pub fn waivers(&self) -> &Records {
        &self.waivers
    }
    /// `prose` and `var` rows in document order (D19).
    pub fn doc_blocks(&self) -> &Records {
        &self.doc_blocks
    }
    /// `body` rows, in file order.
    pub fn bodies(&self) -> &Records {
        &self.bodies
    }
    /// Bench `corner` rows.
    pub fn corners(&self) -> &Records {
        &self.corners
    }
    /// Bench `config` rows (D11a).
    pub fn configs(&self) -> &Records {
        &self.configs
    }
    /// Bench `sweep` rows.
    pub fn sweeps(&self) -> &Records {
        &self.sweeps
    }
    /// Bench `montecarlo` rows.
    pub fn montecarlos(&self) -> &Records {
        &self.montecarlos
    }
    /// Bench `optimize` rows.
    pub fn optimizes(&self) -> &Records {
        &self.optimizes
    }
    /// Bench `output` rows.
    pub fn outputs(&self) -> &Records {
        &self.outputs
    }
    /// Bench `spec` rows.
    pub fn specs(&self) -> &Records {
        &self.specs
    }
    /// Bench `code` rows.
    pub fn codes(&self) -> &Records {
        &self.codes
    }

    /// Returns the row holding the record with id `id`, in any table.
    pub fn find_id(&self, id: ObjId) -> Option<Loc> {
        let loc = self.by_id.get(&id).copied();
        assert!(loc.is_none_or(|l| (l.row as usize) < self.table_len(l.table)));
        loc
    }

    /// Returns the number of rows in table `t`.
    pub fn table_len(&self, t: TableId) -> usize {
        with_table!(self, t, |tab| tab.len())
    }

    /// Returns the record at `loc` as owned text (cold path: allocates).
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

    /// Returns the header as a record (fields empty, attributes in order).
    pub fn header_record(&self) -> RecordBuf {
        let mut rec = RecordBuf::new(RecordKind::Header);
        for (k, vals) in self.pool.attrs(self.header) {
            rec.attrs.push(Attr {
                key: self.strings.get(k).to_owned(),
                values: vals
                    .iter()
                    .map(|v| self.strings.get(*v).to_owned())
                    .collect(),
            });
        }
        assert_eq!(rec.attrs.len(), self.header.len as usize);
        rec
    }

    /// Returns the canonical file bytes (format spec §2) of this document.
    pub fn canonical(&self) -> String {
        let text = schemify_format::write(&self.to_format());
        assert!(text.starts_with("schemify "));
        text
    }

    // -------------------------------------------------------------- load

    /// Loads tables from a parsed format model, re-iding `W001` duplicates and reporting
    /// what the format layer cannot see (see module docs). Never fails: format errors
    /// were caught by the parser; semantic findings are diagnostics.
    pub(crate) fn from_format(src: &FormatDoc<'_>) -> Loaded {
        let kind = src.kind();
        assert!(src.record_count() >= 1);
        let mut doc = Document::new(kind);
        doc.version = src.version();
        doc.by_id.reserve(src.record_count() as usize);
        let mut scratch = Scratch::default();
        scratch.fill_format(src, 0);
        let header = scratch.view(RecordKind::Header);
        doc.header = push_header(&mut doc.pool, &mut doc.strings, &header);
        let mut locs = Vec::with_capacity(src.record_count() as usize);
        locs.push(None);
        for r in 1..src.record_count() {
            let rk = src.record_kind(r);
            let Some(t) = TableId::of(rk) else {
                assert_eq!(rk, RecordKind::Unknown, "cell files hold only cell kinds");
                doc.unknown_records += 1;
                locs.push(None);
                continue;
            };
            scratch.fill_format(src, r);
            let loc = doc.push_row(t, &scratch.view(rk));
            locs.push(Some(loc));
        }
        for c in src.eof_comments() {
            let s = doc.strings.intern(c);
            doc.eof_comments.push(s);
        }
        let mut diags = doc.reid_dups(src, &locs);
        diags.extend(doc.load_diags());
        assert!(doc.unknown_records == 0 || doc.read_only());
        Loaded { doc, diags }
    }

    /// Appends a validated record to table `t` and indexes its id. Returns its row.
    fn push_row(&mut self, t: TableId, rec: &crate::tables::RecView<'_>) -> Loc {
        let row = with_table_mut!(self, t, |tab, strings, pool| {
            tab.push(rec, strings, pool);
            tab.len() - 1
        });
        assert!(row < t.capacity() as usize, "{t:?} exceeds its capacity");
        let loc = Loc {
            table: t,
            row: row as u32,
        };
        let id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
        if id != ObjId::NONE {
            // First occurrence wins; `reid_dups` gives later duplicates new ids.
            self.by_id.entry(id).or_insert(loc);
        }
        assert_eq!(self.table_len(t), row + 1, "pushed rows land at the end");
        loc
    }

    /// Gives every `W001` duplicate a fresh id, deterministically: the generator is
    /// seeded from the old id text and the duplicate's ordinal (`IdRng::for_import`), so
    /// reloading the same bytes always yields the same ids. References (`waive`
    /// targets, constraint `member`s) keep pointing at the first occurrence.
    fn reid_dups(&mut self, src: &FormatDoc<'_>, locs: &[Option<Loc>]) -> Vec<Diag> {
        let mut diags = Vec::new();
        for (ordinal, w) in src.warnings().iter().enumerate() {
            assert_eq!(w.code, schemify_format::Code::W001);
            let Some(loc) = locs[w.record as usize] else {
                continue;
            };
            let mut rec = self.record(loc);
            let old = rec.fields[0].clone();
            let mut rng = IdRng::for_import(&old, ordinal as u32);
            let by_id = &self.by_id;
            let strings = &self.strings;
            let new = fresh_id(&mut rng, |t| {
                ObjId::find(t, strings).is_some_and(|id| by_id.contains_key(&id))
            });
            rec.fields[0] = new.as_str().to_owned();
            self.replace_row(loc, &rec);
            diags.push(Diag {
                code: Code::W001,
                line: w.line,
                detail: "duplicate id re-assigned",
            });
        }
        assert_eq!(diags.len(), src.warnings().len());
        diags
    }

    /// Findings on a loaded document the format layer does not report: duplicate
    /// override keys on one instance (`E024`) and dangling in-file references (`E042`).
    fn load_diags(&self) -> Vec<Diag> {
        let mut diags = Vec::new();
        let inherit = self.strings.find("inherit");
        let mut keys = Vec::new();
        for &span in &self.instances.attrs {
            // `inherit <pin> <net>` is repeatable, one line per pin (format spec §4),
            // so it is keyed by (inherit, pin); every other key must be unique.
            keys.clear();
            keys.extend(self.pool.attrs(span).map(|(k, vals)| {
                let pin = (Some(k) == inherit)
                    .then(|| vals.first().copied())
                    .flatten();
                (k, pin)
            }));
            if (1..keys.len()).any(|i| keys[..i].contains(&keys[i])) {
                diags.push(Diag {
                    code: Code::E024,
                    line: 0,
                    detail: "instance overrides the same key twice",
                });
            }
        }
        let before = diags.len();
        for _ in self.dangling() {
            diags.push(Diag {
                code: Code::E042,
                line: 0,
                detail: "reference to a record that does not exist",
            });
        }
        assert!(diags.len() >= before && diags.iter().all(|d| !d.code.is_warning()));
        diags
    }

    // -------------------------------------------------------------- save

    /// Builds the format model of this document: header, then every table in rank
    /// order (instances write `at` then `name` first), then end-of-file comments.
    ///
    /// # Panics
    ///
    /// Panics on a read-only document (it lost the records it cannot model).
    pub(crate) fn to_format(&self) -> FormatDoc<'static> {
        assert!(!self.read_only(), "a read-only document is never written");
        let mut out = FormatDoc::new(self.kind);
        for (k, vals) in self.pool.attrs(self.header) {
            out.push_attr(self.strings.get(k));
            for v in vals {
                out.push_value(self.strings.get(*v));
            }
        }
        let mut sink = FormatSink::new(out);
        for t in TableId::ALL {
            with_table!(self, t, |tab| {
                for row in 0..tab.len() {
                    tab.emit(row, &self.strings, &self.pool, &mut sink);
                }
            });
        }
        let mut out = sink.doc;
        for &c in &self.eof_comments {
            out.push_eof_comment(self.strings.get(c));
        }
        assert_eq!(out.kind(), self.kind);
        out
    }

    // -------------------------------------------------------------- lookups

    /// Finds a record by kind and key: id text for id kinds, name for named kinds,
    /// draw-order index for graphics. The header is not a table row (see edit code).
    pub(crate) fn locate(&self, kind: RecordKind, key: &str) -> Option<Loc> {
        let t = TableId::of(kind)?;
        let loc = match kind.key() {
            KeyRule::Id => self
                .find_id(ObjId::find(key, &self.strings)?)
                .filter(|l| l.table == t),
            KeyRule::Name => {
                let name = self.strings.find(key)?;
                with_table!(self, t, |tab| (0..tab.len())
                    .find(|&r| tab.name_at(r) == name && tab.kind_at(r) == kind))
                .map(|row| Loc {
                    table: t,
                    row: row as u32,
                })
            }
            KeyRule::Position => {
                let row: u32 = key.parse().ok()?;
                (row < self.graphics.len() as u32).then_some(Loc { table: t, row })
            }
            KeyRule::Header | KeyRule::TypeName => None,
        };
        let loc = loc.filter(|l| self.kind_at(*l) == kind);
        assert!(loc.is_none_or(|l| l.table == t));
        loc
    }

    /// Returns the record kind at `loc`.
    pub(crate) fn kind_at(&self, loc: Loc) -> RecordKind {
        with_table!(self, loc.table, |tab| tab.kind_at(loc.row as usize))
    }

    /// Returns the key text addressing `loc` in op text (id, name, or graphic index).
    pub(crate) fn key_of(&self, loc: Loc) -> String {
        let row = loc.row as usize;
        let key = match self.kind_at(loc).key() {
            KeyRule::Id => with_table!(self, loc.table, |tab| tab.id_at(row, &self.strings))
                .text(&self.strings),
            KeyRule::Name => {
                let name = with_table!(self, loc.table, |tab| tab.name_at(row));
                self.strings.get(name).to_owned()
            }
            KeyRule::Position => row.to_string(),
            KeyRule::Header | KeyRule::TypeName => {
                unreachable!("rows are id, name or position keyed")
            }
        };
        assert!(!key.is_empty() || self.kind_at(loc).key() == KeyRule::Name);
        key
    }

    // -------------------------------------------------------------- primitives

    /// Inserts a validated record: at row `at` of an ordered table (default: after the
    /// last row of the same kind, else at the end), anywhere in an unordered one.
    ///
    /// # Errors
    ///
    /// `E041` id taken, `E024` name taken, `E043` position out of range, position given
    /// for an unordered table, or table full.
    pub(crate) fn insert(&mut self, rec: &RecordBuf, at: Option<u32>) -> Result<Loc, Code> {
        let t = TableId::of(rec.kind).expect("validated records have a table");
        self.check_key_free(rec, None)?;
        let len = self.table_len(t);
        if len >= t.capacity() as usize {
            return Err(Code::E043);
        }
        let pos = match at {
            Some(_) if !t.ordered() => return Err(Code::E043),
            Some(p) if p as usize > len => return Err(Code::E043),
            Some(p) => p as usize,
            None if t.ordered() => self.default_position(t, rec.kind),
            None => len,
        };
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        let mut loc = self.push_row(t, &scratch.view(rec.kind));
        if pos != len {
            self.move_rows(t, len, pos);
            loc.row = pos as u32;
        }
        assert_eq!(self.kind_at(loc), rec.kind);
        Ok(loc)
    }

    fn default_position(&self, t: TableId, kind: RecordKind) -> usize {
        assert!(t.ordered() && TableId::of(kind) == Some(t));
        let pos = with_table!(self, t, |tab| {
            match (0..tab.len()).rev().find(|&r| tab.kind_at(r) == kind) {
                Some(r) => r + 1,
                None => tab.len(),
            }
        });
        assert!(pos <= self.table_len(t));
        pos
    }

    /// Deletes the row at `loc`. Ordered tables shift; unordered ones swap the last row in.
    pub(crate) fn delete(&mut self, loc: Loc) {
        let (t, row) = (loc.table, loc.row as usize);
        let len = self.table_len(t);
        assert!(row < len, "stale row");
        let id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
        if id != ObjId::NONE && self.by_id.get(&id) == Some(&loc) {
            self.by_id.remove(&id);
        }
        let (attrs, lines) = with_table!(self, t, |tab| (
            tab.attrs_col()[row],
            tab.comments_col()[row].len + tab.verbatim_col().get(row).map_or(0, |s| s.len)
        ));
        self.pool.orphan(attrs, lines);
        if t.ordered() {
            with_table_mut!(self, t, |tab, _s, _p| tab.remove_row(row));
            self.reindex(t, row..len - 1);
        } else {
            with_table_mut!(self, t, |tab, _s, _p| tab.swap_remove_row(row));
            self.reindex(t, row..(row + 1).min(len - 1));
        }
        assert_eq!(self.table_len(t), len - 1);
    }

    /// Replaces the row at `loc` with a validated record of the same table, keeping its
    /// position. The new key must be free (or be the row's own).
    ///
    /// # Errors
    ///
    /// `E041`/`E024` when the new key belongs to another record; `E043` when the record
    /// belongs to another table.
    pub(crate) fn put(&mut self, loc: Loc, rec: &RecordBuf) -> Result<Loc, Code> {
        if TableId::of(rec.kind) != Some(loc.table) {
            return Err(Code::E043);
        }
        self.check_key_free(rec, Some(loc))?;
        self.replace_row(loc, rec);
        assert_eq!(self.kind_at(loc), rec.kind);
        Ok(loc)
    }

    /// Replaces a row: push the new one, then swap it into place (only the replaced row
    /// changes position-wise, so file order is kept).
    fn replace_row(&mut self, loc: Loc, rec: &RecordBuf) {
        let (t, row) = (loc.table, loc.row as usize);
        let old_id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
        if old_id != ObjId::NONE && self.by_id.get(&old_id) == Some(&loc) {
            self.by_id.remove(&old_id);
        }
        let (attrs, lines) = with_table!(self, t, |tab| (
            tab.attrs_col()[row],
            tab.comments_col()[row].len + tab.verbatim_col().get(row).map_or(0, |s| s.len)
        ));
        self.pool.orphan(attrs, lines);
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        let len = self.table_len(t);
        let tmp = with_table_mut!(self, t, |tab, strings, pool| {
            tab.push(&scratch.view(rec.kind), strings, pool);
            tab.swap_remove_row(row);
            tab.len()
        });
        assert_eq!(tmp, len);
        self.reindex(t, row..row + 1);
    }

    /// Moves the row at `loc` to row `to` of its (ordered) table.
    ///
    /// # Errors
    ///
    /// `E043` for an unordered table or `to` out of range.
    pub(crate) fn reorder(&mut self, loc: Loc, to: u32) -> Result<Loc, Code> {
        let len = self.table_len(loc.table);
        if !loc.table.ordered() || to as usize >= len {
            return Err(Code::E043);
        }
        self.move_rows(loc.table, loc.row as usize, to as usize);
        let new = Loc {
            table: loc.table,
            row: to,
        };
        assert!(new.row < len as u32);
        Ok(new)
    }

    fn move_rows(&mut self, t: TableId, from: usize, to: usize) {
        let len = self.table_len(t);
        assert!(
            t.ordered() && from < len && to < len,
            "rows move only in ordered tables"
        );
        if from == to {
            return;
        }
        with_table_mut!(self, t, |tab, _s, _p| tab.move_row(from, to));
        self.reindex(t, from.min(to)..from.max(to) + 1);
        assert_eq!(self.table_len(t), len);
    }

    /// Re-points `by_id` at the rows in `rows` after they moved.
    fn reindex(&mut self, t: TableId, rows: std::ops::Range<usize>) {
        let len = self.table_len(t);
        assert!(rows.start <= rows.end && rows.start <= len);
        for row in rows.start..rows.end.min(len) {
            let id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
            if id != ObjId::NONE {
                self.by_id.insert(
                    id,
                    Loc {
                        table: t,
                        row: row as u32,
                    },
                );
            }
        }
    }

    /// Fails when `rec`'s id or name is held by a record other than `own`.
    fn check_key_free(&self, rec: &RecordBuf, own: Option<Loc>) -> Result<(), Code> {
        assert!(own.is_none_or(|l| TableId::of(rec.kind) == Some(l.table)));
        assert!(
            rec.kind.key() == KeyRule::Position || !rec.fields.is_empty(),
            "validated"
        );
        let key = rec.fields.first().map_or("", String::as_str);
        match rec.kind.key() {
            KeyRule::Id => {
                let taken = ObjId::find(key, &self.strings)
                    .and_then(|id| self.find_id(id))
                    .is_some_and(|l| Some(l) != own);
                if taken {
                    return Err(Code::E041);
                }
            }
            KeyRule::Name => {
                let taken = self.locate(rec.kind, key).is_some_and(|l| Some(l) != own);
                if taken {
                    return Err(Code::E024);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Replaces the header's attribute lines with those of a validated header record.
    pub(crate) fn put_header(&mut self, rec: &RecordBuf) {
        assert_eq!(rec.kind, RecordKind::Header);
        self.pool.orphan(self.header, 0);
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        let view = scratch.view(RecordKind::Header);
        self.header = push_header(&mut self.pool, &mut self.strings, &view);
        assert_eq!(self.header.len as usize, rec.attrs.len());
    }

    /// Validates a record for this document's file kind (see `check_record`).
    pub(crate) fn check(&self, rec: &RecordBuf) -> Result<(), Code> {
        check_record(rec, self.kind).map_err(|(c, _)| c)
    }

    // -------------------------------------------------------------- whole-document checks

    /// Spec §4 kind rules (`E025`), the same rules the reader applies, so a document that
    /// passes is a file that parses.
    pub(crate) fn kind_rules_ok(&self) -> bool {
        let impls = self.impls.len();
        let schematic = (0..impls)
            .filter(|&r| self.strings.get(self.impls.fields[r][1]) == "schematic")
            .count();
        assert!(schematic <= impls);
        assert!(
            self.kind != FileKind::Shbench || impls == 0,
            "benches have no impls"
        );
        let ok = match self.kind {
            FileKind::Shprim => self.pins.len() >= 1 && impls >= 1 && schematic == 0,
            FileKind::Shcomp => schematic == 1,
            _ => true,
        };
        let cell = matches!(self.kind, FileKind::Shprim | FileKind::Shcomp);
        ok && !(cell && impls > 1 && self.header_value("default").is_none())
    }

    /// In-file references whose target does not exist (`E042`): `waive` → instance id,
    /// constraint `member` → any id, `body` → impl name, `spec` → output name, header
    /// `default` → impl name. Returned as (referencing record, referenced text), sorted.
    pub(crate) fn dangling(&self) -> Vec<(RecordKind, String, String)> {
        let mut out = Vec::new();
        let s = &self.strings;
        for r in 0..self.waivers.len() {
            let target = s.get(self.waivers.fields[r][1]);
            let ok = ObjId::find(target, s)
                .and_then(|id| self.find_id(id))
                .is_some_and(|l| l.table == TableId::Instances);
            if !ok {
                out.push((
                    RecordKind::Waive,
                    s.get(self.waivers.fields[r][0]).to_owned(),
                    target.to_owned(),
                ));
            }
        }
        for r in 0..self.constraints.len() {
            for (k, vals) in self.pool.attrs(self.constraints.attrs[r]) {
                if s.get(k) != "member" {
                    continue;
                }
                let target = vals.first().map_or("", |v| s.get(*v));
                if ObjId::find(target, s)
                    .and_then(|id| self.find_id(id))
                    .is_none()
                {
                    out.push((
                        RecordKind::Constraint,
                        s.get(self.constraints.fields[r][0]).to_owned(),
                        target.to_owned(),
                    ));
                }
            }
        }
        for r in 0..self.bodies.len() {
            let name = s.get(self.bodies.fields[r][0]);
            if self.locate(RecordKind::Impl, name).is_none() {
                out.push((RecordKind::Body, name.to_owned(), name.to_owned()));
            }
        }
        for r in 0..self.specs.len() {
            let name = s.get(self.specs.fields[r][0]);
            if self.locate(RecordKind::Output, name).is_none() {
                out.push((RecordKind::Spec, name.to_owned(), name.to_owned()));
            }
        }
        if let Some(d) = self.header_value("default")
            && self.locate(RecordKind::Impl, d).is_none()
        {
            out.push((RecordKind::Header, String::new(), d.to_owned()));
        }
        out.sort();
        let members = self.pool.attr_key.len(); // a loose bound: every attribute line
        assert!(
            out.len() <= self.waivers.len() + members + self.bodies.len() + self.specs.len() + 1
        );
        out
    }

    /// Returns whether the wire at `loc` has equal endpoints (`E026`).
    pub(crate) fn wire_zero(&self, loc: Loc) -> bool {
        assert_eq!(loc.table, TableId::Wires);
        let r = loc.row as usize;
        let w = &self.wires;
        w.x1[r] == w.x2[r] && w.y1[r] == w.y2[r]
    }

    /// Rebuilds the arena and pool from the live rows when garbage outweighs live data,
    /// so a long session of edits stays bounded. Row order and ids are kept; `Sym` and
    /// `Span` values change (they are never held across edits).
    pub(crate) fn compact_if_needed(&mut self) {
        let size = self.pool.size();
        if (self.pool.dead as usize) < (1 << 16) || (self.pool.dead as usize) * 2 < size {
            return;
        }
        let model = self.to_format();
        let rebuilt = Document::from_format(&model).doc;
        assert_eq!(
            rebuilt.table_len(TableId::Instances),
            self.table_len(TableId::Instances)
        );
        *self = rebuilt;
        assert_eq!(self.pool.dead, 0);
    }
}

fn push_header(pool: &mut Pool, strings: &mut Strings, rec: &crate::tables::RecView<'_>) -> Span {
    assert!(rec.verbatim.is_empty() && rec.comments.is_empty());
    let span = pool.push_attrs(rec, &[], strings);
    assert_eq!(span.len as usize, rec.attrs.len());
    span
}
