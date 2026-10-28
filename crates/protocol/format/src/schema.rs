//! Record schemas: the types that describe spec §4 as data, and the lookups the reader,
//! writer and merge make into the tables of `records.rs`.
//!
//! Every rule the reader enforces and every ordering decision the writer and the merge
//! make comes from those tables. Semantics beyond them (expressions, templates, cell
//! references) belong to later layers (spec §6).
//!
//! Contents:
//! - `pub enum FileKind`, `FileKind::ALL`, `FileKind::word`, `FileKind::from_word`,
//!   `FileKind::records`, `FileKind::lookup`, `FileKind::rank_sorted` (crate)
//! - `pub enum RecordKind`, `RecordKind::schema`, `RecordKind::word`, `RecordKind::rank`,
//!   `RecordKind::key`
//! - `pub enum KeyRule`, `pub enum Ty`, `Ty::check`, `pub enum Rest`
//! - `pub struct Shape`, `Shape::check_arity`, `Shape::ty`
//! - `pub struct AttrSchema`, `pub struct RecordSchema`, `RecordSchema::attr`,
//!   `RecordSchema::knows_attr` (crate)
//! - `pub const RANK_INHERIT: u8`, `pub const RANK_POSITIONAL: u8`,
//!   `pub const SHAPE_FORMS: &[&str]`, `pub(crate) const RANK_MAX: u8`
//! - `pub fn shape_arity(form: &str, n: usize) -> Option<bool>`

use crate::diag::Code;
use crate::id::is_id;
use crate::records::{FILE_RECORDS, RECORDS};
use crate::scalar::{CoordError, Orient, is_number, parse_coord};

/// The file kinds of spec §1, named by the third word of the header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    /// `.shprim`: symbol plus leaf impls.
    Shprim,
    /// `.shcomp`: symbol plus one schematic impl, leaf impls and docs.
    Shcomp,
    /// `.shbench`: schematic, docs and bench tables; no symbol.
    Shbench,
    /// `schemify.project`: libraries, plugins and settings.
    Project,
    /// `schemify.lock`: resolved sources, tool-written.
    Lock,
    /// User `config`: settings, keymap, simulators, plugin order.
    Config,
    /// The `doc` crate's edit journal and op text (edits spec §1, §3). It has no known
    /// record words: every record is kept like a §5 unknown record, at any version.
    Journal,
    /// `tech.map`: a PDK's technology-class map (pdk spec §6): one `class` record per
    /// (class, flavor) the PDK provides.
    Techmap,
}

/// Every record word the reader knows, across all file kinds.
///
/// Declaration order is the writer's kind order inside sorted ranks (rank 3 `inst`
/// before `wire`, rank 6 `corner` before `config`), so it follows the spec tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs)] // each variant is the spec record of the same name
pub enum RecordKind {
    /// Line 1, `schemify <version> <kind>`, with header attributes.
    Header,
    Pin,
    Param,
    Impl,
    Line,
    Rect,
    Arc,
    Circle,
    Poly,
    Stext,
    Inst,
    Wire,
    Label,
    Port,
    Noconn,
    Text,
    /// Rank-3 schematic graphic `shape <id> <form> <values…>` (annotation only, no
    /// connectivity): the forms of the rank-2 graphics with an id.
    Shape,
    Constraint,
    Waive,
    Prose,
    Var,
    Body,
    Corner,
    Config,
    Sweep,
    Montecarlo,
    Optimize,
    Output,
    Spec,
    Code,
    /// Project `lib <name> path|git|url ...`.
    Lib,
    /// Project `plugin <name> path|git|url ...`.
    Plugin,
    /// `set <key> <value>` (project and user config).
    Set,
    /// Lockfile `locked <lib|plugin> <name> <kind> <resolved> <sha256>`.
    Locked,
    /// User config `key <chord> <command>`.
    Key,
    /// User config `sim <name>`.
    Sim,
    /// User config `plugin <name>` (load order).
    PluginOrder,
    /// Techmap `class <class> <flavor>`: the PDK cell a technology class becomes and its
    /// param expressions (pdk spec §6).
    Class,
    /// A record word unknown to this reader, kept verbatim because the file's
    /// version is newer (spec §5). Its word lives in the document.
    Unknown,
}

/// How a record is identified (spec §4 ids and names, research §3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRule {
    /// The single header record.
    Header,
    /// Field 0 is an id, unique per file across all id-bearing kinds (`W001`).
    Id,
    /// Field 0 is a name, unique within the kind (`E024`).
    Name,
    /// Fields 0 and 1 together are the name (`locked <type> <name>`).
    TypeName,
    /// No identity: the record's position is its identity (rank 2 graphics).
    Position,
}

/// What a value in a positional slot must look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// Any value; meaning is assigned by later layers (spec §6).
    Any,
    /// An object id defining this record (Crockford base32, 1–13 chars).
    Id,
    /// An object id referring to another record of the same file; merge rewrites it
    /// when it re-ids a colliding record.
    IdRef,
    /// A coordinate: number with at most 3 fractional digits that fits i32 milli-units.
    Coord,
    /// A file-level number literal `-?digits(.digits)?`.
    Number,
    /// One of `R0 R90 R180 R270 MR0 MR90 MR180 MR270`.
    Orient,
    /// `true` or `false`.
    Bool,
    /// One of the listed words.
    Choice(&'static [&'static str]),
}

/// How many values may follow the fixed slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rest {
    /// None.
    None,
    /// Zero or one.
    Optional(Ty),
    /// Zero or more.
    Many(Ty),
    /// An even number (coordinate pairs).
    Pairs(Ty),
}

/// The values a record line or attribute line carries after its word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Required slots, in order.
    pub fixed: &'static [Ty],
    /// What may follow them.
    pub rest: Rest,
}

/// A known attribute of a record kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrSchema {
    /// The attribute word.
    pub key: &'static str,
    /// Its values.
    pub shape: Shape,
    /// Missing → `E029`.
    pub required: bool,
    /// May appear more than once; merge then identifies each line by key + first value.
    /// A second line of a non-repeatable attribute is `E024`.
    pub repeat: bool,
}

/// Everything the format layer knows about one record kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordSchema {
    /// The record word.
    pub word: &'static str,
    /// Section rank; records must appear in non-decreasing rank (`E023`).
    pub rank: u8,
    /// Identity rule.
    pub key: KeyRule,
    /// The writer sorts this kind by key (ranks 3 and 6, the lockfile); otherwise file
    /// order is kept because it is semantic.
    pub sorted: bool,
    /// Positional values on the record line.
    pub fields: Shape,
    /// Known attributes. Unknown ones are kept and written after known ones (spec §5).
    pub attrs: &'static [AttrSchema],
    /// Any attribute word is a known attribute (instance param overrides, constraint
    /// key/values, simulator settings).
    pub open: bool,
    /// The record may carry `| text` verbatim lines.
    pub verbatim: bool,
}

/// Rank sentinel of [`RecordKind::Unknown`]: the record takes the rank of the known
/// record before it.
pub const RANK_INHERIT: u8 = u8::MAX;
/// The one rank whose records have no identity (symbol graphics). Merge treats it as a
/// single block.
pub const RANK_POSITIONAL: u8 = 2;
/// The forms of a rank-3 `shape` (spec §4 rank 3), in [`shape_arity`] order.
pub const SHAPE_FORMS: &[&str] = &["line", "rect", "arc", "circle", "poly"];
/// Highest rank any record kind has.
pub(crate) const RANK_MAX: u8 = 6;
/// Header words of [`FileKind`], in declaration order.
const FILE_WORDS: [&str; 8] = [
    "shprim", "shcomp", "shbench", "project", "lock", "config", "journal", "techmap",
];

/// Whether `n` values may follow `shape <id> <form>`: 4 for `line`/`rect` (two corners), 5
/// for `arc` (center, radius, start and sweep degrees), 3 for `circle`, an even count ≥ 4
/// for `poly`. `None` for a form outside [`SHAPE_FORMS`].
pub fn shape_arity(form: &str, n: usize) -> Option<bool> {
    let ok = match form {
        "line" | "rect" => n == 4,
        "arc" => n == 5,
        "circle" => n == 3,
        "poly" => n >= 4 && n.is_multiple_of(2),
        _ => return None,
    };
    Some(ok)
}

impl FileKind {
    /// Every file kind, in declaration order.
    pub const ALL: [FileKind; 8] = [
        FileKind::Shprim,
        FileKind::Shcomp,
        FileKind::Shbench,
        FileKind::Project,
        FileKind::Lock,
        FileKind::Config,
        FileKind::Journal,
        FileKind::Techmap,
    ];

    /// Returns the header word (`shcomp`, `project`, ...).
    pub fn word(self) -> &'static str {
        FILE_WORDS[self as usize]
    }

    /// Parses a header kind word; `None` for anything not in spec §1.
    pub fn from_word(word: &str) -> Option<FileKind> {
        let i = FILE_WORDS.iter().position(|w| *w == word)?;
        let kind = FileKind::ALL[i];
        assert_eq!(kind.word(), word);
        Some(kind)
    }

    /// Returns the record kinds this file kind allows, in canonical writer order.
    /// The header is always allowed and is not listed.
    pub fn records(self) -> &'static [RecordKind] {
        FILE_RECORDS[self as usize]
    }

    /// Resolves a record word for this file kind. Words outside the kind's list are
    /// `None`, even if another file kind knows them (`pin` in a `.shbench` is unknown).
    pub fn lookup(self, word: &str) -> Option<RecordKind> {
        let kind = self.records().iter().copied().find(|k| k.word() == word);
        assert!(kind.is_none_or(|k| k != RecordKind::Header && k != RecordKind::Unknown));
        kind
    }

    /// Returns whether the writer sorts rank `rank` of this file kind by (kind, key).
    pub(crate) fn rank_sorted(self, rank: u8) -> bool {
        assert!(rank <= RANK_MAX);
        self.records()
            .iter()
            .any(|k| k.rank() == rank && k.schema().sorted)
    }
}

impl RecordKind {
    /// Returns this kind's schema row.
    pub fn schema(self) -> &'static RecordSchema {
        &RECORDS[self as usize]
    }

    /// Returns the record word (`"schemify"` for the header, `""` for `Unknown`).
    pub fn word(self) -> &'static str {
        self.schema().word
    }

    /// Returns the section rank; `Unknown` returns [`RANK_INHERIT`] because its rank is
    /// that of the record before it.
    pub fn rank(self) -> u8 {
        self.schema().rank
    }

    /// Returns how records of this kind are identified for duplicate checks, merge and diff.
    pub fn key(self) -> KeyRule {
        self.schema().key
    }
}

impl Ty {
    /// Checks value `v` against this slot type (spec §3); returns a coordinate's
    /// milli-units (`None` for the other types). The reader and the `doc` crate's record
    /// checks share it, so both report the same code and detail.
    ///
    /// # Errors
    ///
    /// `E029` with the expected form; `E022` for a coordinate with more than 3
    /// fractional digits or outside i32 milli-units.
    pub fn check(self, v: &str) -> Result<Option<i32>, (Code, &'static str)> {
        let (ok, detail) = match self {
            Ty::Any => (true, ""),
            Ty::Coord => {
                return parse_coord(v).map(Some).map_err(|e| match e {
                    CoordError::NotNumber => (Code::E029, "expected a coordinate number"),
                    CoordError::NotRepresentable => (
                        Code::E022,
                        "needs more than 3 fractional digits or overflows i32 milli-units",
                    ),
                });
            }
            Ty::Id | Ty::IdRef => (
                is_id(v),
                "expected an id (1-13 lowercase Crockford base32 chars)",
            ),
            Ty::Number => (is_number(v), "expected a number `-?digits(.digits)?`"),
            Ty::Orient => (
                Orient::parse(v).is_some(),
                "expected R0 R90 R180 R270 MR0 MR90 MR180 MR270",
            ),
            Ty::Bool => (v == "true" || v == "false", "expected true or false"),
            Ty::Choice(words) => (words.contains(&v), "value not in the allowed set"),
        };
        if ok {
            Ok(None)
        } else {
            Err((Code::E029, detail))
        }
    }
}

impl Shape {
    /// Checks that `n` values fit this shape's arity (spec §4); the error is the `E029`
    /// detail.
    ///
    /// # Errors
    ///
    /// Fewer values than fixed slots, or a count `rest` does not allow.
    pub fn check_arity(&self, n: usize) -> Result<(), &'static str> {
        let fixed = self.fixed.len();
        assert!(fixed <= 8, "schema shapes are short");
        if n < fixed {
            return Err("too few values");
        }
        let extra = n - fixed;
        let ok = match self.rest {
            Rest::None => extra == 0,
            Rest::Optional(_) => extra <= 1,
            Rest::Many(_) => true,
            Rest::Pairs(_) => extra.is_multiple_of(2),
        };
        if ok {
            Ok(())
        } else {
            Err("wrong number of values")
        }
    }

    /// Returns the type of slot `i`, or `None` past the end of an arity-bounded shape.
    pub fn ty(&self, i: usize) -> Option<Ty> {
        match (self.fixed.get(i), self.rest) {
            (Some(t), _) => Some(*t),
            (None, Rest::None) => None,
            (None, Rest::Optional(t) | Rest::Many(t) | Rest::Pairs(t)) => Some(t),
        }
    }
}

impl RecordSchema {
    /// Finds a known attribute by word, with its index in [`RecordSchema::attrs`].
    pub fn attr(&self, key: &str) -> Option<(usize, &'static AttrSchema)> {
        let found = self.attrs.iter().enumerate().find(|(_, a)| a.key == key);
        assert!(
            found.is_none_or(|(i, a)| i < 32 && a.key == key),
            "attribute bitmask holds 32"
        );
        found
    }

    /// Returns whether the writer treats attribute `key` as known (written first).
    pub(crate) fn knows_attr(&self, key: &str) -> bool {
        self.open || self.attr(key).is_some()
    }
}
