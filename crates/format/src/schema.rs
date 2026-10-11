//! Record schemas as data (spec §4).
//!
//! Every rule the reader enforces and every ordering decision the writer and the merge
//! make comes from the tables in this file. Adding a record kind is one `RecordKind`
//! variant, one `RECORDS` row and one entry in each file kind's list; no function body
//! changes.

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
}

const FILE_WORDS: [&str; 6] = ["shprim", "shcomp", "shbench", "project", "lock", "config"];

impl FileKind {
    /// Every file kind, in declaration order.
    pub const ALL: [FileKind; 6] = [
        FileKind::Shprim,
        FileKind::Shcomp,
        FileKind::Shbench,
        FileKind::Project,
        FileKind::Lock,
        FileKind::Config,
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
    /// A record word unknown to this reader, kept verbatim because the file's
    /// version is newer (spec §5). Its word lives in the document.
    Unknown,
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

/// Rank sentinel of [`RecordKind::Unknown`]: the record takes the rank of the known
/// record before it.
pub const RANK_INHERIT: u8 = u8::MAX;
/// Highest rank any record kind has.
pub const RANK_MAX: u8 = 6;
/// The one rank whose records have no identity (symbol graphics). Merge treats it as a
/// single block.
pub const RANK_POSITIONAL: u8 = 2;

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

impl Shape {
    /// Returns the type of slot `i`, or `None` past the end of an arity-bounded shape.
    pub fn ty(&self, i: usize) -> Option<Ty> {
        match (self.fixed.get(i), self.rest) {
            (Some(t), _) => Some(*t),
            (None, Rest::None) => None,
            (None, Rest::Optional(t) | Rest::Many(t) | Rest::Pairs(t)) => Some(t),
        }
    }
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
    pub fn knows_attr(&self, key: &str) -> bool {
        self.open || self.attr(key).is_some()
    }
}

// ---------------------------------------------------------------- table builders

const A: Ty = Ty::Any;
const C: Ty = Ty::Coord;
const N: Ty = Ty::Number;
const B: Ty = Ty::Bool;

// Table builders run at compile time, so their asserts cost nothing at runtime.

const fn fixed(fixed: &'static [Ty]) -> Shape {
    assert!(
        fixed.len() <= 8,
        "the reader's shape check assumes short shapes"
    );
    Shape {
        fixed,
        rest: Rest::None,
    }
}

const fn attr(key: &'static str, values: &'static [Ty]) -> AttrSchema {
    assert!(!key.is_empty() && key.as_bytes()[0].is_ascii_alphabetic());
    assert!(values.len() <= 8);
    AttrSchema {
        key,
        shape: fixed(values),
        required: false,
        repeat: false,
    }
}

const fn repeat(key: &'static str, values: &'static [Ty]) -> AttrSchema {
    assert!(
        !key.is_empty() && !values.is_empty(),
        "repeatable lines are told apart by value 0"
    );
    AttrSchema {
        key,
        shape: fixed(values),
        required: false,
        repeat: true,
    }
}

const fn required(key: &'static str, values: &'static [Ty]) -> AttrSchema {
    assert!(!key.is_empty() && !values.is_empty());
    AttrSchema {
        key,
        shape: fixed(values),
        required: true,
        repeat: false,
    }
}

const fn rec(
    word: &'static str,
    rank: u8,
    key: KeyRule,
    fields: Shape,
    attrs: &'static [AttrSchema],
) -> RecordSchema {
    assert!(rank <= RANK_MAX && !word.is_empty());
    assert!(
        attrs.len() <= 32,
        "the reader tracks attributes in a u32 bitmask"
    );
    let sorted = matches!(rank, 3 | 6);
    RecordSchema {
        word,
        rank,
        key,
        sorted,
        fields,
        attrs,
        open: false,
        verbatim: false,
    }
}

const fn verbatim(mut r: RecordSchema) -> RecordSchema {
    assert!(!r.verbatim);
    r.verbatim = true;
    r
}

const fn open(mut r: RecordSchema) -> RecordSchema {
    assert!(!r.open);
    r.open = true;
    r
}

const fn sorted(mut r: RecordSchema) -> RecordSchema {
    assert!(
        !r.sorted && !matches!(r.key, KeyRule::Position),
        "positional records have no sort key"
    );
    r.sorted = true;
    r
}

const SOURCE: Ty = Ty::Choice(&["path", "git", "url"]);
const LAYER: AttrSchema = attr("layer", &[N]);
const FILL: AttrSchema = attr("fill", &[B]);
const DOC: AttrSchema = attr("doc", &[A]);
const UNIT: AttrSchema = attr("unit", &[A]);
const MIN: AttrSchema = attr("min", &[A]);
const MAX: AttrSchema = attr("max", &[A]);
const CACHED: AttrSchema = attr("cached", &[A, A]);
const PLACED: &[Ty] = &[Ty::Id, C, C, Ty::Orient, A];
/// The forms of a rank-3 `shape` (spec §4 rank 3), in [`shape_arity`] order.
pub const SHAPE_FORMS: &[&str] = &["line", "rect", "arc", "circle", "poly"];

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

/// Header attributes: the union over all cell kinds (spec §4 rank 0). A kind-specific
/// attribute in another kind (`corner` in a `.shcomp`) is still kept; this list only
/// decides that it is written before unknown attributes.
const HEADER_ATTRS: &[AttrSchema] = &[
    DOC,
    attr("default", &[A]),
    attr("prefix", &[A]),
    attr("category", &[A]),
    attr("provenance", &[A]),
    attr("corner", &[A]),
    attr("config", &[A]),
];

use KeyRule::{Header as KH, Id as KI, Name as KN, Position as KP, TypeName as KT};

/// One row per [`RecordKind`], in declaration order.
static RECORDS: [RecordSchema; RecordKind::Unknown as usize + 1] = [
    // Header: fields are `<version> <kind>`, checked by the lexer, not by shape.
    rec("schemify", 0, KH, fixed(&[A, A]), HEADER_ATTRS),
    // rank 1: symbol interface
    rec(
        "pin",
        1,
        KN,
        fixed(&[A, Ty::Choice(&["in", "out", "inout"]), C, C]),
        &[
            attr("domain", &[Ty::Choice(&["analog", "digital", "real"])]),
            attr("inherit", &[A]),
            attr("number", &[A]),
        ],
    ),
    rec(
        "param",
        1,
        KN,
        fixed(&[A, A]),
        &[
            attr(
                "kind",
                &[Ty::Choice(&["number", "int", "bool", "string", "choice"])],
            ),
            UNIT,
            MIN,
            MAX,
            attr("step", &[A]),
            attr("grid", &[A]),
            AttrSchema {
                key: "choices",
                shape: Shape {
                    fixed: &[A],
                    rest: Rest::Many(A),
                },
                required: false,
                repeat: false,
            },
            attr("computed", &[A]),
            attr("show", &[A]),
            DOC,
            CACHED,
        ],
    ),
    rec(
        "impl",
        1,
        KN,
        fixed(&[A, A]),
        &[
            repeat("format", &[A, A]),
            attr("model", &[A]),
            attr("file", &[A]),
        ],
    ),
    // rank 2: symbol graphics, draw order
    rec(
        "line",
        2,
        KP,
        fixed(&[C, C, C, C]),
        &[LAYER, attr("dash", &[N]), attr("width", &[N])],
    ),
    rec("rect", 2, KP, fixed(&[C, C, C, C]), &[LAYER, FILL]),
    rec("arc", 2, KP, fixed(&[C, C, C, N, N]), &[LAYER, FILL]),
    rec("circle", 2, KP, fixed(&[C, C, C]), &[LAYER, FILL]),
    rec(
        "poly",
        2,
        KP,
        Shape {
            fixed: &[C, C, C, C],
            rest: Rest::Pairs(C),
        },
        &[LAYER, FILL, attr("closed", &[B])],
    ),
    rec(
        "stext",
        2,
        KP,
        fixed(&[C, C, Ty::Orient, A]),
        &[
            attr("size", &[N]),
            attr("hcenter", &[]),
            attr("vcenter", &[]),
            LAYER,
        ],
    ),
    // rank 3: schematic, kinds in writer order, each sorted by id
    open(rec(
        "inst",
        3,
        KI,
        fixed(&[Ty::Id, A]),
        &[
            required("at", &[C, C, Ty::Orient]),
            required("name", &[A]),
            repeat("inherit", &[A, A]),
        ],
    )),
    rec(
        "wire",
        3,
        KI,
        fixed(&[Ty::Id, C, C, C, C]),
        &[attr("bus", &[B])],
    ),
    rec("label", 3, KI, fixed(PLACED), &[]),
    rec("port", 3, KI, fixed(PLACED), &[]),
    rec("noconn", 3, KI, fixed(&[Ty::Id, C, C]), &[]),
    rec("text", 3, KI, fixed(PLACED), &[attr("size", &[N])]),
    rec(
        "shape",
        3,
        KI,
        Shape {
            fixed: &[Ty::Id, Ty::Choice(SHAPE_FORMS)],
            rest: Rest::Many(C),
        },
        &[
            LAYER,
            FILL,
            attr("dash", &[N]),
            attr("width", &[N]),
            attr("closed", &[B]),
        ],
    ),
    open(rec(
        "constraint",
        3,
        KI,
        fixed(&[Ty::Id, A]),
        &[repeat("member", &[Ty::IdRef])],
    )),
    rec(
        "waive",
        3,
        KI,
        fixed(&[Ty::Id, Ty::IdRef, A]),
        &[required("reason", &[A])],
    ),
    // rank 4: docs, document order
    verbatim(rec("prose", 4, KI, fixed(&[Ty::Id]), &[])),
    rec("var", 4, KN, fixed(&[A, A]), &[UNIT, MIN, MAX, DOC, CACHED]),
    // rank 5: impl bodies
    verbatim(rec("body", 5, KN, fixed(&[A]), &[])),
    // rank 6: bench, sorted by (kind, name)
    rec(
        "corner",
        6,
        KN,
        fixed(&[A]),
        &[
            repeat("lib", &[A, A]),
            repeat("include", &[A]),
            repeat("set", &[A, A]),
            attr("temp", &[A]),
        ],
    ),
    rec(
        "config",
        6,
        KN,
        fixed(&[A]),
        &[
            AttrSchema {
                key: "order",
                shape: Shape {
                    fixed: &[A],
                    rest: Rest::Many(A),
                },
                required: false,
                repeat: false,
            },
            repeat("cell", &[A, A]),
            repeat("inst", &[A, A]),
        ],
    ),
    rec(
        "sweep",
        6,
        KN,
        fixed(&[A]),
        &[
            attr("var", &[A]),
            AttrSchema {
                key: "values",
                shape: Shape {
                    fixed: &[A],
                    rest: Rest::Many(A),
                },
                required: false,
                repeat: false,
            },
            attr("lin", &[A, A, A]),
            attr("log", &[A, A, A]),
        ],
    ),
    rec(
        "montecarlo",
        6,
        KN,
        fixed(&[A]),
        &[attr("samples", &[N]), attr("seed", &[N])],
    ),
    rec(
        "optimize",
        6,
        KN,
        fixed(&[A]),
        &[
            attr("algo", &[A]),
            AttrSchema {
                key: "goal",
                shape: Shape {
                    fixed: &[A, Ty::Choice(&["min", "max", "target"])],
                    rest: Rest::Optional(A),
                },
                required: false,
                repeat: true,
            },
            repeat("vary", &[A]),
            attr("budget", &[N]),
        ],
    ),
    rec("output", 6, KN, fixed(&[A, A]), &[UNIT]),
    rec("spec", 6, KN, fixed(&[A]), &[MIN, MAX]),
    verbatim(rec("code", 6, KN, fixed(&[A, A]), &[])),
    // project manifest
    rec(
        "lib",
        1,
        KN,
        Shape {
            fixed: &[A, SOURCE, A],
            rest: Rest::Optional(A),
        },
        &[attr("subdir", &[A])],
    ),
    rec(
        "plugin",
        1,
        KN,
        Shape {
            fixed: &[A, SOURCE, A],
            rest: Rest::Optional(A),
        },
        &[repeat("grant", &[A])],
    ),
    rec("set", 1, KN, fixed(&[A, A]), &[]),
    // lockfile
    sorted(rec(
        "locked",
        1,
        KT,
        fixed(&[Ty::Choice(&["lib", "plugin"]), A, A, A, A]),
        &[],
    )),
    // user config
    rec("key", 1, KN, fixed(&[A, A]), &[]),
    open(rec(
        "sim",
        1,
        KN,
        fixed(&[A]),
        &[
            attr("path", &[A]),
            AttrSchema {
                key: "args",
                shape: Shape {
                    fixed: &[],
                    rest: Rest::Many(A),
                },
                required: false,
                repeat: false,
            },
        ],
    )),
    rec("plugin", 1, KN, fixed(&[A]), &[]),
    // Unknown: kept verbatim; identified by its word and first field.
    RecordSchema {
        word: "",
        rank: RANK_INHERIT,
        key: KN,
        sorted: false,
        fields: Shape {
            fixed: &[],
            rest: Rest::Many(A),
        },
        attrs: &[],
        open: true,
        verbatim: true,
    },
];

use RecordKind as R;

const SYMBOL: [RecordKind; 9] = [
    R::Pin,
    R::Param,
    R::Impl,
    R::Line,
    R::Rect,
    R::Arc,
    R::Circle,
    R::Poly,
    R::Stext,
];
const BENCH: [RecordKind; 8] = [
    R::Corner,
    R::Config,
    R::Sweep,
    R::Montecarlo,
    R::Optimize,
    R::Output,
    R::Spec,
    R::Code,
];

/// Allowed record kinds per [`FileKind`], in declaration order of `FileKind`.
static FILE_RECORDS: [&[RecordKind]; 6] = [
    // shprim: symbol, docs, bodies
    &concat::<9, 3, 12>(SYMBOL, [R::Prose, R::Var, R::Body]),
    // shcomp: symbol, schematic, docs, bodies, code (D17 as amended 2026-10-11: netlist
    // text emitted inside each of the component's definitions)
    &concat::<9, 13, 22>(
        SYMBOL,
        [
            R::Inst,
            R::Wire,
            R::Label,
            R::Port,
            R::Noconn,
            R::Text,
            R::Shape,
            R::Constraint,
            R::Waive,
            R::Prose,
            R::Var,
            R::Body,
            R::Code,
        ],
    ),
    // shbench: schematic without ports, docs, bench
    &concat::<10, 8, 18>(
        [
            R::Inst,
            R::Wire,
            R::Label,
            R::Noconn,
            R::Text,
            R::Shape,
            R::Constraint,
            R::Waive,
            R::Prose,
            R::Var,
        ],
        BENCH,
    ),
    &[R::Lib, R::Plugin, R::Set],
    &[R::Locked],
    &[R::Set, R::Key, R::Sim, R::PluginOrder],
];

const fn concat<const X: usize, const Y: usize, const Z: usize>(
    x: [RecordKind; X],
    y: [RecordKind; Y],
) -> [RecordKind; Z] {
    assert!(X + Y == Z);
    let mut out = [RecordKind::Unknown; Z];
    let mut i = 0;
    while i < X {
        out[i] = x[i];
        i += 1;
    }
    while i < Z {
        out[i] = y[i - X];
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_rows_are_consistent() {
        for fk in FileKind::ALL {
            assert_eq!(FileKind::from_word(fk.word()), Some(fk));
            let kinds = fk.records();
            for (i, k) in kinds.iter().enumerate() {
                assert!(*k != R::Header && *k != R::Unknown);
                // A word resolves to exactly one kind per file kind.
                assert_eq!(fk.lookup(k.word()), Some(*k));
                // Listed order is rank order, so the writer's kind order is the spec's.
                if i > 0 {
                    assert!(kinds[i - 1].rank() <= k.rank());
                    assert!(kinds[i - 1] < *k);
                }
                // A rank is either entirely sorted or entirely file-order.
                let peers = kinds.iter().filter(|p| p.rank() == k.rank());
                assert!(
                    peers
                        .clone()
                        .all(|p| p.schema().sorted == k.schema().sorted)
                );
                // Positional identity is exactly the positional rank.
                assert_eq!(k.key() == KeyRule::Position, k.rank() == RANK_POSITIONAL);
                assert!(k.rank() <= RANK_MAX);
                // Attribute dedup in the reader uses a u32 bitmask.
                assert!(k.schema().attrs.len() <= 32);
            }
        }
        assert_eq!(FileKind::from_word("schematic"), None);
        assert_eq!(FileKind::Shbench.lookup("pin"), None);
        // D17 as amended: a component carries `code`; the other bench tables stay
        // bench-only, and a primitive has neither.
        assert_eq!(FileKind::Shcomp.lookup("code"), Some(R::Code));
        for w in [
            "corner",
            "config",
            "sweep",
            "montecarlo",
            "optimize",
            "output",
            "spec",
        ] {
            assert_eq!(FileKind::Shcomp.lookup(w), None, "{w}");
        }
        assert_eq!(FileKind::Shprim.lookup("code"), None);
        assert_eq!(FileKind::Config.lookup("plugin"), Some(R::PluginOrder));
        assert_eq!(FileKind::Project.lookup("plugin"), Some(R::Plugin));
    }
}
