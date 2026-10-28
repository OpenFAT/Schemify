//! Shared test helpers: a seeded RNG, fixtures, request shorthands and a random edit
//! generator (`disk.rs`: the journal tests' simulated disk; `model.rs`: the naive model
//! the stateful property tests compare against). Everything derives from one u64 seed,
//! so any failure replays from the printed seed.
//!
//! Contents:
//! - fixtures `OTA`, `NFET`, `BENCH`, `MANIFEST`, `LOCK`, `MESSY`, `SEED_DOC`
//! - `pub struct Mix`: `new`, `next`, `below`, `chance`, `pick`, `word`
//! - `pub fn coord`, `pub fn open`, `pub fn tx`, `pub fn req`, `pub fn run`, `pub fn canon`
//! - `pub fn gen_record`, `pub fn gen_op`

#![allow(dead_code)] // each test binary uses a subset

pub mod disk;
pub mod model;

use schemify_doc::{
    DocHandle, Document, Edit, EditRequest, Mode, Op, Outcome, Project, RecordBuf, TableId, Target,
    Transaction,
};
use schemify_format::{KeyRule, RecordKind};

pub const OTA: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/ota5t.shcomp"
));
pub const NFET: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/nfet_01v8.shprim"
));
pub const BENCH: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/ota_ac.shbench"
));
pub const MANIFEST: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/schemify.project"
));
pub const LOCK: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/schemify.lock"
));
pub const MESSY: &str = include_str!(concat!(
    env!("SCHEMIFY_ROOT"),
    "/crates/protocol/format/tests/fixtures/ota5t.messy.shcomp"
));

/// A minimal `.shcomp` the generators edit: one schematic impl they never touch.
pub const SEED_DOC: &str = "schemify 1 shcomp\n  doc seed\n\nimpl sch schematic\n";

/// splitmix64.
pub struct Mix(u64);

const NAMES: &[&str] = &["A", "B", "C", "D", "VDD"];
const KEYS: &[&str] = &["W", "L", "nf", "m"];
const VALUES: &[&str] = &["1", "2", "0.5", "1u", "\"quoted value\"", "a b"];

impl Mix {
    pub fn new(seed: u64) -> Mix {
        Mix(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        self.next() % n
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
    pub fn word(&mut self, xs: &[&'static str]) -> &'static str {
        xs[self.below(xs.len() as u64) as usize]
    }
}

pub fn coord(milli: i32) -> String {
    let mut s = String::new();
    schemify_format::format_coord(milli, &mut s);
    s
}

/// Opens `text` as project file `relpath` in a fresh project.
pub fn open(text: &str, relpath: &str) -> (Project, DocHandle) {
    let mut p =
        Project::new(None, None, 7).expect("a project without manifest or lock always opens");
    let h = p
        .open("", relpath, text.as_bytes(), None)
        .expect("fixture text opens")
        .handle;
    (p, h)
}

pub fn tx(ops: Vec<Op>) -> Edit {
    Edit::Tx(Transaction {
        label: "t".into(),
        mode: Mode::Commit,
        ops,
    })
}

pub fn req(doc: DocHandle, edit: Edit) -> EditRequest {
    EditRequest {
        doc,
        edit,
        unix_ms: 1,
    }
}

pub fn run(p: &mut Project, h: DocHandle, edit: Edit) -> Outcome {
    schemify_doc::apply_edits(p, &[req(h, edit)]).remove(0)
}

pub fn canon(p: &Project, h: DocHandle) -> String {
    p.doc(h)
        .expect("the handle names an open document")
        .canonical()
}

pub fn gen_record(rng: &mut Mix, doc: &Document) -> RecordBuf {
    let c = |rng: &mut Mix| coord(small(rng));
    match rng.below(9) {
        0 | 1 => {
            let mut r = RecordBuf::new(RecordKind::Inst)
                .field(&new_id(rng, doc))
                .field(rng.word(&["./cella", "core:res"]))
                .attr("at", &[&c(rng), &c(rng), rng.word(&["R0", "MR90"])])
                .attr("name", &[&format!("X{}", rng.below(50))]);
            for k in KEYS {
                if rng.chance(40) {
                    r = r.attr(k, &[rng.word(VALUES)]);
                }
            }
            if rng.chance(10) {
                r.comments.push(" placed".into());
            }
            r
        }
        2 | 3 => RecordBuf::new(RecordKind::Wire)
            .field(&new_id(rng, doc))
            .field(&c(rng))
            .field(&c(rng))
            .field(&c(rng))
            .field(&c(rng)),
        4 => RecordBuf::new(RecordKind::Label)
            .field(&new_id(rng, doc))
            .field(&c(rng))
            .field(&c(rng))
            .field("R0")
            .field(rng.word(&["vin", "d[3:0]", "net with space"])),
        5 => RecordBuf::new(RecordKind::Pin)
            .field(rng.word(NAMES))
            .field(rng.word(&["in", "out", "inout"]))
            .field(&c(rng))
            .field(&c(rng)),
        6 => RecordBuf::new(RecordKind::Param)
            .field(rng.word(KEYS))
            .field(rng.word(VALUES))
            .attr("unit", &["um"]),
        7 => {
            let k = *rng.pick(&[RecordKind::Line, RecordKind::Rect]);
            RecordBuf::new(k)
                .field(&c(rng))
                .field(&c(rng))
                .field(&c(rng))
                .field(&c(rng))
        }
        _ => {
            if rng.chance(50) {
                RecordBuf::new(RecordKind::Var)
                    .field(rng.word(&["gm", "id", "vov"]))
                    .field(rng.word(VALUES))
            } else {
                RecordBuf::new(RecordKind::Prose)
                    .field(&new_id(rng, doc))
                    .verbatim("# note")
                    .verbatim("")
                    .verbatim("$g_m$ text")
            }
        }
    }
}

/// One random op against the current `doc`: mostly valid, sometimes violating one rule.
pub fn gen_op(rng: &mut Mix, doc: &Document) -> Op {
    let kinds = [
        RecordKind::Inst,
        RecordKind::Wire,
        RecordKind::Label,
        RecordKind::Pin,
        RecordKind::Param,
        RecordKind::Line,
        RecordKind::Var,
        RecordKind::Prose,
    ];
    // Ops on records need records: pick among kinds that have some (a missing target
    // still comes up through `target`'s 8%), else insert.
    let have = |ks: &[RecordKind]| -> Vec<RecordKind> {
        ks.iter()
            .copied()
            .filter(|k| !keys(doc, *k).is_empty())
            .collect()
    };
    let choice = rng.below(10);
    let pool = match choice {
        3 => have(&kinds),
        4 => have(&[
            RecordKind::Wire,
            RecordKind::Label,
            RecordKind::Pin,
            RecordKind::Var,
        ]),
        5..=7 => have(&[RecordKind::Inst]),
        8 | 9 => have(&[
            RecordKind::Pin,
            RecordKind::Line,
            RecordKind::Var,
            RecordKind::Wire,
        ]),
        _ => Vec::new(),
    };
    let header_edit = (5..=6).contains(&choice) && rng.chance(15);
    match choice {
        0..=2 | 3.. if pool.is_empty() && !header_edit => {
            let rec = gen_record(rng, doc);
            let ordered = TableId::of(rec.kind)
                .expect("a document's record kinds all map to tables")
                .ordered();
            let at = (rng.chance(30) && ordered).then(|| rng.below(4) as u32);
            Op::Insert { rec, at }
        }
        3 => {
            let kind = *rng.pick(&pool);
            Op::Delete(target(doc, rng, kind))
        }
        4 => {
            let kind = *rng.pick(&pool);
            let t = target(doc, rng, kind);
            let (index, value) = match kind {
                RecordKind::Wire => (1 + rng.below(4) as u32, coord(small(rng))),
                RecordKind::Label => (if rng.chance(10) { 0 } else { 4 }, "vout".to_owned()),
                RecordKind::Pin if rng.chance(30) => (0, rng.word(NAMES).to_string()),
                RecordKind::Pin => (2, coord(small(rng))),
                _ => (if rng.chance(10) { 5 } else { 1 }, "2*gm".to_owned()),
            };
            Op::SetField {
                target: t,
                index,
                value,
            }
        }
        5 | 6 => {
            if header_edit {
                return Op::SetAttr {
                    target: Target::header(),
                    key: "doc".into(),
                    values: vec![rng.word(VALUES).to_string()],
                };
            }
            let t = target(doc, rng, RecordKind::Inst);
            if rng.chance(30) {
                let at = vec![coord(small(rng)), coord(small(rng)), "R180".into()];
                Op::SetAttr {
                    target: t,
                    key: "at".into(),
                    values: at,
                }
            } else {
                Op::SetAttr {
                    target: t,
                    key: rng.word(KEYS).to_string(),
                    values: vec![rng.word(VALUES).to_string()],
                }
            }
        }
        7 => Op::RemoveAttr {
            target: target(doc, rng, RecordKind::Inst),
            key: if rng.chance(10) {
                "at".into()
            } else {
                rng.word(KEYS).to_string()
            },
            first: None,
        },
        _ => {
            let kind = *rng.pick(&pool);
            let len = keys(doc, kind).len() as u64;
            Op::Reorder {
                target: target(doc, rng, kind),
                to: rng.below(len + 2) as u32, // sometimes out of range (E043)
            }
        }
    }
}

// ---- private ----

/// Keys (id/name/index) of `kind` records currently in `doc`.
fn keys(doc: &Document, kind: RecordKind) -> Vec<String> {
    let s = doc.strings();
    match kind {
        RecordKind::Inst => doc.instances().id.iter().map(|i| i.text(s)).collect(),
        RecordKind::Wire => doc.wires().id.iter().map(|i| i.text(s)).collect(),
        RecordKind::Label => doc.labels().id.iter().map(|i| i.text(s)).collect(),
        RecordKind::Pin => doc
            .pins()
            .name
            .iter()
            .map(|n| s.get(*n).to_owned())
            .collect(),
        RecordKind::Param => doc
            .params()
            .fields
            .iter()
            .map(|f| s.get(f[0]).to_owned())
            .collect(),
        RecordKind::Var | RecordKind::Prose => (0..doc.doc_blocks().kind.len())
            .filter(|&r| doc.doc_blocks().kind[r] == kind)
            .map(|r| s.get(doc.doc_blocks().fields[r][0]).to_owned())
            .collect(),
        RecordKind::Line | RecordKind::Rect => (0..doc.graphics().shape.len())
            .filter(|&r| doc.graphics().shape[r] == kind)
            .map(|r| r.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Picks an existing key of `kind`, or (sometimes, or when none exist) a missing one.
fn target(doc: &Document, rng: &mut Mix, kind: RecordKind) -> Target {
    let ks = keys(doc, kind);
    if ks.is_empty() || rng.chance(8) {
        let missing = match kind.key() {
            KeyRule::Id => "zzzzzz",
            KeyRule::Position => "999",
            _ => "missing",
        };
        return Target::new(kind, missing);
    }
    Target::new(kind, &rng.pick(&ks).clone())
}

fn new_id(rng: &mut Mix, doc: &Document) -> String {
    // Sometimes reuse an existing id (E041).
    let all: Vec<String> = [
        RecordKind::Inst,
        RecordKind::Wire,
        RecordKind::Label,
        RecordKind::Prose,
    ]
    .iter()
    .flat_map(|k| keys(doc, *k))
    .collect();
    if !all.is_empty() && rng.chance(5) {
        return rng.pick(&all).clone();
    }
    const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    (0..6).map(|_| A[rng.below(32) as usize] as char).collect()
}

fn small(rng: &mut Mix) -> i32 {
    (rng.below(9) as i32 - 4) * 5_000 + if rng.chance(10) { 125 } else { 0 }
}
