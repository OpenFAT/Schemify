//! The record schema tables: spec §4's record tables as `const` data, one row per
//! [`RecordKind`] and one allowed-kind list per `FileKind`.
//!
//! Adding a record kind is one `RecordKind` variant, one `RECORDS` row and one entry in
//! each file kind's list; no function body changes. Table builders are `const fn`, so
//! their asserts run at compile time and cost nothing at run time.
//!
//! Contents:
//! - `pub(crate) static RECORDS: [RecordSchema; _]`
//! - `pub(crate) static FILE_RECORDS: [&[RecordKind]; 8]`

use crate::schema::{
    AttrSchema, KeyRule, RANK_INHERIT, RANK_MAX, RecordKind, RecordSchema, Rest, SHAPE_FORMS,
    Shape, Ty,
};

use KeyRule::{Header as KH, Id as KI, Name as KN, Position as KP, TypeName as KT};
use RecordKind as R;

/// One row per [`RecordKind`], in declaration order.
#[rustfmt::skip]
pub(crate) static RECORDS: [RecordSchema; RecordKind::Unknown as usize + 1] = [
    // Header: fields are `<version> <kind>`, checked by the lexer, not by shape.
    rec("schemify", 0, KH, fixed(&[A, A]), HEADER_ATTRS),
    // rank 1: symbol interface
    rec("pin", 1, KN, fixed(&[A, Ty::Choice(&["in", "out", "inout"]), C, C]), &[
        attr("domain", &[Ty::Choice(&["analog", "digital", "real"])]),
        attr("inherit", &[A]),
        attr("number", &[A]),
    ]),
    rec("param", 1, KN, fixed(&[A, A]), &[
        attr("kind", &[Ty::Choice(&["number", "int", "bool", "string", "choice"])]),
        UNIT, MIN, MAX, attr("step", &[A]), attr("grid", &[A]), many("choices", &[A]),
        attr("computed", &[A]), attr("show", &[A]), attr("valid", &[A]), DOC, CACHED,
    ]),
    rec("impl", 1, KN, fixed(&[A, A]), &[
        repeat("format", &[A, A]), attr("model", &[A]), attr("file", &[A]),
    ]),
    // rank 2: symbol graphics, draw order
    rec("line", 2, KP, fixed(&[C, C, C, C]), &[LAYER, DASH, WIDTH]),
    rec("rect", 2, KP, fixed(&[C, C, C, C]), &[LAYER, FILL, DASH, WIDTH]),
    rec("arc", 2, KP, fixed(&[C, C, C, N, N]), &[LAYER, FILL, DASH, WIDTH]),
    rec("circle", 2, KP, fixed(&[C, C, C]), &[LAYER, FILL, DASH, WIDTH]),
    rec("poly", 2, KP, Shape { fixed: &[C, C, C, C], rest: Rest::Pairs(C) }, GRAPHIC_CLOSED),
    rec("stext", 2, KP, fixed(&[C, C, Ty::Orient, A]), &[
        attr("size", &[N]), attr("hcenter", &[]), attr("vcenter", &[]), LAYER,
    ]),
    // rank 3: schematic, kinds in writer order, each sorted by id
    open(rec("inst", 3, KI, fixed(&[Ty::Id, A]), &[
        required("at", &[C, C, Ty::Orient]),
        required("name", &[A]),
        repeat("inherit", &[A, A]),
        repeat("cached", &[A, A, A]),
    ])),
    rec("wire", 3, KI, fixed(&[Ty::Id, C, C, C, C]), &[attr("bus", &[B])]),
    rec("label", 3, KI, fixed(PLACED), &[]),
    rec("port", 3, KI, fixed(PLACED), &[]),
    rec("noconn", 3, KI, fixed(&[Ty::Id, C, C]), &[]),
    rec("text", 3, KI, fixed(PLACED), &[attr("size", &[N])]),
    rec("shape", 3, KI, Shape { fixed: &[Ty::Id, Ty::Choice(SHAPE_FORMS)], rest: Rest::Many(C) },
        GRAPHIC_CLOSED),
    open(rec("constraint", 3, KI, fixed(&[Ty::Id, A]), &[repeat("member", &[Ty::IdRef])])),
    rec("waive", 3, KI, fixed(&[Ty::Id, Ty::IdRef, A]), &[required("reason", &[A])]),
    // rank 4: docs, document order
    verbatim(rec("prose", 4, KI, fixed(&[Ty::Id]), &[])),
    rec("var", 4, KN, fixed(&[A, A]), &[
        UNIT, MIN, MAX, DOC, CACHED,
        // `dist gauss|uniform <spread> [rel]` (run spec §3).
        AttrSchema {
            key: "dist",
            shape: Shape {
                fixed: &[Ty::Choice(&["gauss", "uniform"]), A],
                rest: Rest::Optional(Ty::Choice(&["rel"])),
            },
            required: false,
            repeat: false,
        },
    ]),
    // rank 5: impl bodies
    verbatim(rec("body", 5, KN, fixed(&[A]), &[])),
    // rank 6: bench, sorted by (kind, name)
    rec("corner", 6, KN, fixed(&[A]), &[
        repeat("lib", &[A, A]), repeat("include", &[A]), repeat("set", &[A, A]),
        attr("temp", &[A]),
    ]),
    rec("config", 6, KN, fixed(&[A]), &[
        many("order", &[A]), repeat("cell", &[A, A]), repeat("inst", &[A, A]),
    ]),
    rec("sweep", 6, KN, fixed(&[A]), &[
        attr("var", &[A]), many("values", &[A]), attr("lin", &[A, A, A]),
        attr("log", &[A, A, A]),
    ]),
    rec("montecarlo", 6, KN, fixed(&[A]), &[attr("samples", &[N]), attr("seed", &[N])]),
    rec("optimize", 6, KN, fixed(&[A]), &[
        attr("algo", &[A]),
        // `<output> min|max|target <expr> [weight <w>]`: run checks the tail.
        AttrSchema { repeat: true, ..many("goal", &[A, Ty::Choice(&["min", "max", "target"])]) },
        repeat("vary", &[A]), repeat("config", &[A]), repeat("corner", &[A]),
        attr("budget", &[N]), attr("seed", &[N]),
    ]),
    rec("output", 6, KN, fixed(&[A, A]), &[UNIT]),
    rec("spec", 6, KN, fixed(&[A]), &[MIN, MAX]),
    verbatim(rec("code", 6, KN, fixed(&[A, A]), &[])),
    // project manifest
    rec("lib", 1, KN, Shape { fixed: &[A, LIB_SOURCE, A], rest: Rest::Optional(A) },
        &[attr("subdir", &[A])]),
    rec("plugin", 1, KN, Shape { fixed: &[A, SOURCE, A], rest: Rest::Optional(A) },
        &[repeat("grant", &[A])]),
    rec("set", 1, KN, fixed(&[A, A]), &[]),
    // lockfile
    sorted(rec("locked", 1, KT, fixed(&[Ty::Choice(&["lib", "plugin"]), A, A, A, A]), &[])),
    // user config
    rec("key", 1, KN, fixed(&[A, A]), &[]),
    open(rec("sim", 1, KN, fixed(&[A]), &[attr("path", &[A]), many("args", &[])])),
    rec("plugin", 1, KN, fixed(&[A]), &[]),
    // techmap
    rec("class", 1, KT, fixed(&[A, A]), &[
        required("cell", &[A]),
        repeat("param", &[A, A]),
        attr("vdd", &[A]),
        repeat("limits", &[A, Ty::Choice(&["min", "max"]), A]),
    ]),
    // Unknown: kept verbatim; identified by its word and first field.
    RecordSchema {
        word: "",
        rank: RANK_INHERIT,
        key: KN,
        sorted: false,
        fields: Shape { fixed: &[], rest: Rest::Many(A) },
        attrs: &[],
        open: true,
        verbatim: true,
    },
];

/// Allowed record kinds per [`FileKind`](crate::FileKind), in declaration order of `FileKind`, each list
/// in canonical writer order.
#[rustfmt::skip]
pub(crate) static FILE_RECORDS: [&[RecordKind]; 8] = [
    // shprim: symbol, docs, bodies
    &concat::<9, 3, 12>(SYMBOL, [R::Prose, R::Var, R::Body]),
    // shcomp: symbol, schematic, docs, bodies, code (D17 as amended 2026-10-11: netlist
    // text emitted inside each of the component's definitions)
    &concat::<9, 13, 22>(SYMBOL, [
        R::Inst, R::Wire, R::Label, R::Port, R::Noconn, R::Text, R::Shape, R::Constraint,
        R::Waive, R::Prose, R::Var, R::Body, R::Code,
    ]),
    // shbench: schematic without ports, docs, bench
    &concat::<10, 8, 18>([
        R::Inst, R::Wire, R::Label, R::Noconn, R::Text, R::Shape, R::Constraint, R::Waive,
        R::Prose, R::Var,
    ], BENCH),
    &[R::Lib, R::Plugin, R::Set],
    &[R::Locked],
    &[R::Set, R::Key, R::Sim, R::PluginOrder],
    // journal: no known words, every record is free-form
    &[],
    // techmap
    &[R::Class],
];

const A: Ty = Ty::Any;
const C: Ty = Ty::Coord;
const N: Ty = Ty::Number;
const B: Ty = Ty::Bool;
const SOURCE: Ty = Ty::Choice(&["path", "git", "url"]);
/// A `lib` source: the plugin sources plus `map` (`lib tech map <relpath>`, pdk spec §5).
const LIB_SOURCE: Ty = Ty::Choice(&["path", "git", "url", "map"]);
const LAYER: AttrSchema = attr("layer", &[N]);
const FILL: AttrSchema = attr("fill", &[B]);
const DASH: AttrSchema = attr("dash", &[N]);
const WIDTH: AttrSchema = attr("width", &[N]);
const DOC: AttrSchema = attr("doc", &[A]);
const UNIT: AttrSchema = attr("unit", &[A]);
const MIN: AttrSchema = attr("min", &[A]);
const MAX: AttrSchema = attr("max", &[A]);
const CACHED: AttrSchema = attr("cached", &[A, A]);
/// Stroke and fill attributes of a graphic that can close (`poly`, rank-3 `shape`).
const GRAPHIC_CLOSED: &[AttrSchema] = &[LAYER, FILL, DASH, WIDTH, attr("closed", &[B])];
const PLACED: &[Ty] = &[Ty::Id, C, C, Ty::Orient, A];
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

// ---- private ----

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
    assert!(!key.is_empty());
    assert!(
        key.as_bytes()[0].is_ascii_alphabetic(),
        "an attribute key is a word"
    );
    AttrSchema {
        key,
        shape: fixed(values),
        required: false,
        repeat: false,
    }
}

const fn repeat(key: &'static str, values: &'static [Ty]) -> AttrSchema {
    assert!(
        !values.is_empty(),
        "repeatable lines are told apart by value 0"
    );
    AttrSchema {
        repeat: true,
        ..attr(key, values)
    }
}

const fn required(key: &'static str, values: &'static [Ty]) -> AttrSchema {
    assert!(!values.is_empty(), "a required line carries a value");
    AttrSchema {
        required: true,
        ..attr(key, values)
    }
}

/// An attribute of one or more values: `fixed`, then any number of `Any`.
const fn many(key: &'static str, fixed: &'static [Ty]) -> AttrSchema {
    let base = attr(key, fixed);
    AttrSchema {
        shape: Shape {
            fixed,
            rest: Rest::Many(A),
        },
        ..base
    }
}

const fn rec(
    word: &'static str,
    rank: u8,
    key: KeyRule,
    fields: Shape,
    attrs: &'static [AttrSchema],
) -> RecordSchema {
    assert!(rank <= RANK_MAX);
    assert!(!word.is_empty(), "only `Unknown` has no word");
    assert!(
        attrs.len() <= 32,
        "the reader tracks attributes in a u32 bitmask"
    );
    RecordSchema {
        word,
        rank,
        key,
        sorted: matches!(rank, 3 | 6),
        fields,
        attrs,
        open: false,
        verbatim: false,
    }
}

const fn verbatim(r: RecordSchema) -> RecordSchema {
    assert!(!r.verbatim);
    RecordSchema {
        verbatim: true,
        ..r
    }
}

const fn open(r: RecordSchema) -> RecordSchema {
    assert!(!r.open);
    RecordSchema { open: true, ..r }
}

const fn sorted(r: RecordSchema) -> RecordSchema {
    assert!(!r.sorted);
    assert!(
        !matches!(r.key, KeyRule::Position),
        "positional records have no sort key"
    );
    RecordSchema { sorted: true, ..r }
}

const fn concat<const X: usize, const Y: usize, const Z: usize>(
    x: [RecordKind; X],
    y: [RecordKind; Y],
) -> [RecordKind; Z] {
    assert!(X + Y == Z);
    let mut out = [RecordKind::Unknown; Z];
    let mut i = 0;
    while i < Z {
        out[i] = if i < X { x[i] } else { y[i - X] };
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FileKind, RANK_POSITIONAL};

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

    /// Oracle: spec §4's tables, read by hand. Each row of a kind is spot-checked for
    /// its word, rank, identity and the attributes the spec names.
    #[test]
    fn rows_match_the_spec_tables() {
        let rows = [
            (R::Pin, "pin", 1, KeyRule::Name),
            (R::Line, "line", 2, KeyRule::Position),
            (R::Inst, "inst", 3, KeyRule::Id),
            (R::Waive, "waive", 3, KeyRule::Id),
            (R::Prose, "prose", 4, KeyRule::Id),
            (R::Body, "body", 5, KeyRule::Name),
            (R::Code, "code", 6, KeyRule::Name),
            (R::Locked, "locked", 1, KeyRule::TypeName),
            (R::Class, "class", 1, KeyRule::TypeName),
        ];
        for (kind, word, rank, key) in rows {
            assert_eq!((kind.word(), kind.rank(), kind.key()), (word, rank, key));
        }
        let inst = R::Inst.schema();
        assert!(inst.open && inst.sorted && !inst.verbatim);
        assert!(inst.attr("at").is_some_and(|(_, a)| a.required));
        assert!(inst.attr("cached").is_some_and(|(_, a)| a.repeat));
        assert!(R::Locked.schema().sorted && !R::Lib.schema().sorted);
        assert!(R::Prose.schema().verbatim && R::Code.schema().verbatim);
        assert!(!R::Var.schema().verbatim);
        assert_eq!(R::Unknown.rank(), RANK_INHERIT);
    }
}
