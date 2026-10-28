//! Edit gestures as `doc` edit requests: pure transforms from (document tables, selection,
//! gesture parameters) to op lists. Every gesture becomes one transaction, so one undo
//! (edits spec §1–§2). The app never mutates design data itself (ARCHITECTURE invariant).
//!
//! New objects get ids drawn here (the request may supply them, edits spec §1) so the
//! editor can select what it just created.
//!
//! Contents:
//! - `pub struct Ids`, `Ids::new`, `Ids::draw`, `pub struct FreshId`
//! - `pub struct Inserted`
//! - `pub enum Toward`
//! - `pub const AUTO_ID`
//! - `pub fn loc_of`
//! - `pub fn selected_records`
//! - `pub fn schematic_keys`
//! - `pub fn delete`
//! - `pub fn transform`
//! - `pub fn transform_each`
//! - `pub fn unique_name`
//! - `pub fn duplicate`
//! - `pub fn insert_copies`
//! - `pub fn wires`
//! - `pub fn instance`
//! - `pub fn placed`
//! - `pub fn pin`
//! - `pub fn shape`

use std::cmp::Reverse;
use std::collections::HashSet;

use schemify_doc::{Document, Loc, NEW_ID, ObjId, Op, RecordBuf, TableId, Target};
use schemify_format::{IdRng, KeyRule, Orient, RecordKind};

use crate::geometry;
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;

/// Draws fresh ids that are free in `doc` and not yet used by this transaction.
pub struct Ids<'rng> {
    rng: &'rng mut IdRng,
    used: HashSet<String>,
}

/// An id [`Ids::draw`] handed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshId {
    /// The id text, for the record's id field.
    pub text: String,
    /// Its value, for a selection key ([`ObjKey::key`]).
    pub key: u32,
}

/// Records inserted by one gesture and the selection keys of those that have ids.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inserted {
    /// One `insert` per record.
    pub ops: Vec<Op>,
    /// Keys of the inserted records that carry ids, in insert order.
    pub keys: Vec<ObjKey>,
}

/// Which way a row moves one place in its table's order (a pin in port order, a docs
/// block on the page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// One place earlier (a panel's ↑).
    Earlier,
    /// One place later (a panel's ↓).
    Later,
}

/// The id field placeholder doc replaces on insert (used for records whose id the editor
/// does not need to know).
pub const AUTO_ID: &str = NEW_ID;

/// Where a selected object lives in the document, if it still exists.
///
/// # Panics
///
/// Never: the asserts check that the found row is live and of `key`'s table.
#[must_use]
pub fn loc_of(doc: &Document, key: ObjKey) -> Option<Loc> {
    let loc = match key.table {
        TableId::Pins | TableId::Graphics => ((key.key as usize) < doc.table_len(key.table))
            .then_some(Loc {
                table: key.table,
                row: key.key,
            }),
        t => doc.find_id(ObjId(key.key)).filter(|l| l.table == t),
    };
    if let Some(l) = loc {
        assert_eq!(l.table, key.table);
        assert!((l.row as usize) < doc.table_len(l.table), "a live row");
    }
    loc
}

/// Records of the selection that still exist, with their targets, in selection order.
///
/// # Panics
///
/// Never: the assert checks that a stale key is skipped.
#[must_use]
pub fn selected_records(doc: &Document, sel: &[ObjKey]) -> Vec<(Loc, Target, RecordBuf)> {
    let mut out = Vec::with_capacity(sel.len());
    for &k in sel {
        if let Some(loc) = loc_of(doc, k) {
            let rec = doc.record(loc);
            out.push((loc, target(&rec, loc), rec));
        }
    }
    assert!(
        out.len() <= sel.len(),
        "a stale key is skipped, never added"
    );
    out
}

/// Keys of every drawn rank-3 object of `doc` (instances, wires, labels, ports, noconns,
/// texts, shapes), table by table in row order: what select-all, merge and align see.
///
/// # Panics
///
/// Only on a broken internal invariant (one key per row).
#[must_use]
pub fn schematic_keys(doc: &Document) -> Vec<ObjKey> {
    let mut out = Vec::new();
    let tables: [(TableId, &[ObjId]); 7] = [
        (TableId::Instances, &doc.instances().id),
        (TableId::Wires, &doc.wires().id),
        (TableId::Labels, &doc.labels().id),
        (TableId::Ports, &doc.ports().id),
        (TableId::Noconns, &doc.noconns().id),
        (TableId::Texts, &doc.texts().id),
        (TableId::Shapes, &doc.shapes().id),
    ];
    let rows: usize = tables.iter().map(|t| t.1.len()).sum();
    for (table, ids) in tables {
        out.extend(ids.iter().map(|id| ObjKey { table, key: id.0 }));
    }
    assert_eq!(out.len(), rows);
    out
}

/// Deletes the selection. Graphics are deleted from the highest row down so the rows of
/// the ones still to delete stay valid inside the transaction.
///
/// # Panics
///
/// Only on a broken internal invariant (one delete per live selected record).
#[must_use]
pub fn delete(doc: &Document, sel: &[ObjKey]) -> Vec<Op> {
    let mut recs = selected_records(doc, sel);
    recs.sort_by_key(|(loc, _, _)| Reverse((loc.table == TableId::Graphics, loc.row)));
    let graphics = recs
        .iter()
        .filter(|(loc, _, _)| loc.table == TableId::Graphics);
    let rows: Vec<u32> = graphics.map(|(loc, _, _)| loc.row).collect();
    assert!(
        rows.windows(2).all(|w| w[0] >= w[1]),
        "graphics deleted highest row first"
    );
    let ops: Vec<Op> = recs.into_iter().map(|(_, t, _)| Op::Delete(t)).collect();
    assert!(ops.len() <= sel.len());
    ops
}

/// Moves, rotates or flips the selection as a group: one `put` per record.
///
/// # Panics
///
/// Only on a broken internal invariant (one `put` per live selected record at most).
#[must_use]
pub fn transform(doc: &Document, sel: &[ObjKey], x: Xform) -> Vec<Op> {
    let ops: Vec<Op> = selected_records(doc, sel)
        .into_iter()
        .filter_map(|(_, target, mut rec)| {
            geometry::transform_record(&mut rec, x)?;
            Some(Op::Put { target, rec })
        })
        .collect();
    assert!(ops.len() <= sel.len());
    ops
}

/// Rotates or flips every selected object about its own anchor (xschem Alt+R / Alt+F).
///
/// # Panics
///
/// Only on a broken internal invariant (one `put` per live selected record at most).
pub fn transform_each(doc: &Document, sel: &[ObjKey], about: fn(Pt) -> Xform) -> Vec<Op> {
    let ops: Vec<Op> = selected_records(doc, sel)
        .into_iter()
        .filter_map(|(_, target, mut rec)| {
            let a = geometry::anchor(&rec)?;
            geometry::transform_record(&mut rec, about(a))?;
            Some(Op::Put { target, rec })
        })
        .collect();
    assert!(ops.len() <= sel.len());
    ops
}

impl<'rng> Ids<'rng> {
    /// A drawer over `rng`.
    pub fn new(rng: &'rng mut IdRng) -> Ids<'rng> {
        Ids {
            rng,
            used: HashSet::new(),
        }
    }

    /// Returns a fresh 6-character id, free in `doc` and not drawn before by this drawer.
    ///
    /// # Panics
    ///
    /// Only if `schemify_format::fresh_id` hands out an id that does not decode (a bug
    /// there).
    pub fn draw(&mut self, doc: &Document) -> FreshId {
        let used = &self.used;
        let id = schemify_format::fresh_id(self.rng, |s| {
            used.contains(s)
                || ObjId::find(s, doc.strings()).is_some_and(|i| doc.find_id(i).is_some())
        });
        let text = id.as_str().to_owned();
        let key = ObjId::decode(&text).expect("generated ids are 6 chars").0;
        assert!(
            self.used.insert(text.clone()),
            "a drawn id is new to this transaction"
        );
        FreshId { text, key }
    }
}

/// Picks `<prefix><n>` with the smallest `n ≥ 1` not used by an instance of `doc` nor in
/// `taken` (names made earlier in the same transaction), and records it in `taken`.
///
/// # Panics
///
/// An empty `prefix`.
pub fn unique_name(doc: &Document, prefix: &str, taken: &mut HashSet<String>) -> String {
    assert!(
        !prefix.is_empty(),
        "an instance name starts with its prefix"
    );
    let s = doc.strings();
    let names: HashSet<&str> = doc.instances().name.iter().map(|&n| s.get(n)).collect();
    // Bounded: at most one more candidate than there are names.
    for n in 1..=names.len() + taken.len() + 1 {
        let cand = format!("{prefix}{n}");
        if !names.contains(cand.as_str()) && !taken.contains(&cand) {
            taken.insert(cand.clone());
            return cand;
        }
    }
    unreachable!("pigeonhole: one of the candidates is free")
}

/// Copies the selection through `x`: inserts with fresh ids (returned as selection keys),
/// renumbered instance names and suffixed pin names.
pub fn duplicate(doc: &Document, sel: &[ObjKey], x: Xform, ids: &mut Ids<'_>) -> Inserted {
    let recs: Vec<RecordBuf> = selected_records(doc, sel)
        .into_iter()
        .map(|(_, _, r)| r)
        .collect();
    insert_copies(doc, recs, x, ids)
}

/// Inserts `recs` (copies from this or another document, e.g. the clipboard) through `x`.
/// Records the document kind does not allow, or that `x` cannot carry, are skipped.
///
/// # Panics
///
/// Only on a broken internal invariant (one insert per kept record, one key per id).
pub fn insert_copies(
    doc: &Document,
    recs: Vec<RecordBuf>,
    x: Xform,
    ids: &mut Ids<'_>,
) -> Inserted {
    let given = recs.len();
    let mut names = HashSet::new();
    let mut ops = Vec::with_capacity(recs.len());
    let mut keys = Vec::new();
    let kind = doc.kind();
    for mut rec in recs {
        let Some(table) = TableId::of(rec.kind) else {
            continue;
        };
        // Only geometry kinds the document kind allows (format spec §4): rank 3 outside
        // `.shprim`, pins and graphics outside `.shbench`.
        let legal = match rec.kind.schema().rank {
            3 => kind != schemify_format::FileKind::Shprim,
            1 => rec.kind == RecordKind::Pin && kind != schemify_format::FileKind::Shbench,
            2 => kind != schemify_format::FileKind::Shbench,
            _ => false,
        };
        if !legal {
            continue;
        }
        if geometry::transform_record(&mut rec, x).is_none() {
            continue;
        }
        match rec.kind.key() {
            KeyRule::Id => {
                let id = ids.draw(doc);
                rec.fields[0] = id.text;
                keys.push(ObjKey { table, key: id.key });
            }
            KeyRule::Name if rec.kind == RecordKind::Pin => {
                let base = rec.fields[0].clone();
                let pins = doc.pins();
                let s = doc.strings();
                let mut n = 1;
                let free = |c: &str, names: &HashSet<String>| {
                    !names.contains(c) && !pins.name.iter().any(|&p| s.get(p) == c)
                };
                // Bounded: at most one candidate more than there are names taken.
                while !free(&format!("{base}_{n}"), &names) {
                    assert!(n <= pins.name.len() + names.len(), "a free pin name exists");
                    n += 1;
                }
                rec.fields[0] = format!("{base}_{n}");
                names.insert(rec.fields[0].clone());
            }
            KeyRule::Position => {}
            _ => continue,
        }
        if rec.kind == RecordKind::Inst
            && let Some(a) = rec.attrs.iter_mut().find(|a| a.key == "name")
            && let Some(v) = a.values.first_mut()
        {
            let prefix = name_prefix(v).to_owned();
            *v = unique_name(doc, &prefix, &mut names);
        }
        ops.push(Op::Insert { rec, at: None });
    }
    assert!(ops.len() <= given, "copies are skipped, never added");
    assert!(
        keys.len() <= ops.len(),
        "one key per inserted record with an id"
    );
    Inserted { ops, keys }
}

/// Wire segments along `pts` (zero-length segments skipped), each with a fresh id.
///
/// # Panics
///
/// Only on a broken internal invariant (one key per inserted wire).
pub fn wires(doc: &Document, pts: &[Pt], ids: &mut Ids<'_>) -> Inserted {
    let mut ops = Vec::new();
    let mut keys = Vec::new();
    for w in pts.windows(2) {
        if w[0] == w[1] {
            continue;
        }
        let id = ids.draw(doc);
        let rec = RecordBuf::new(RecordKind::Wire)
            .field(&id.text)
            .field(&geometry::coord(w[0].0))
            .field(&geometry::coord(w[0].1))
            .field(&geometry::coord(w[1].0))
            .field(&geometry::coord(w[1].1));
        ops.push(Op::Insert { rec, at: None });
        keys.push(ObjKey {
            table: TableId::Wires,
            key: id.key,
        });
    }
    assert_eq!(ops.len(), keys.len());
    assert!(
        ops.len() < pts.len().max(1),
        "a wire per non-degenerate segment"
    );
    Inserted { ops, keys }
}

/// A new instance of `cellref` at `at` named `name`.
///
/// # Panics
///
/// An empty `cellref`, `name` or `id`.
#[must_use]
pub fn instance(cellref: &str, at: Pt, o: Orient, name: &str, id: &str) -> RecordBuf {
    assert!(!cellref.is_empty(), "an instance names its cell");
    assert!(!name.is_empty());
    assert!(!id.is_empty(), "the id field or `AUTO_ID`");
    RecordBuf::new(RecordKind::Inst)
        .field(id)
        .field(cellref)
        .attr(
            "at",
            &[&geometry::coord(at.0), &geometry::coord(at.1), o.as_str()],
        )
        .attr("name", &[name])
}

/// A new `label`, `port` or `text` record.
///
/// # Panics
///
/// `kind` is another kind, or `id` is empty.
#[must_use]
pub fn placed(kind: RecordKind, at: Pt, o: Orient, text: &str, id: &str) -> RecordBuf {
    assert!(matches!(
        kind,
        RecordKind::Label | RecordKind::Port | RecordKind::Text
    ));
    assert!(!id.is_empty(), "the id field or `AUTO_ID`");
    RecordBuf::new(kind)
        .field(id)
        .field(&geometry::coord(at.0))
        .field(&geometry::coord(at.1))
        .field(o.as_str())
        .field(text)
}

/// A new symbol pin.
///
/// # Panics
///
/// An empty `name`.
#[must_use]
pub fn pin(name: &str, at: Pt) -> RecordBuf {
    assert!(!name.is_empty(), "a pin is keyed by its name");
    RecordBuf::new(RecordKind::Pin)
        .field(name)
        .field("inout")
        .field(&geometry::coord(at.0))
        .field(&geometry::coord(at.1))
}

/// A new symbol `line` or `rect` graphic from corner or end `a` to `b`.
///
/// # Panics
///
/// `kind` is neither [`RecordKind::Line`] nor [`RecordKind::Rect`].
#[must_use]
pub fn shape(kind: RecordKind, a: Pt, b: Pt) -> RecordBuf {
    assert!(
        matches!(kind, RecordKind::Line | RecordKind::Rect),
        "a two-point graphic, got {kind:?}"
    );
    let mut r = RecordBuf::new(kind);
    for v in [a.0, a.1, b.0, b.1] {
        r = r.field(&geometry::coord(v));
    }
    assert_eq!(r.fields.len(), 4);
    r
}

// ---- private ----

/// The edit target addressing `rec` at `loc` (id, name, or graphic row).
fn target(rec: &RecordBuf, loc: Loc) -> Target {
    assert_eq!(
        TableId::of(rec.kind),
        Some(loc.table),
        "the record lives at `loc`"
    );
    let key = match rec.kind.key() {
        KeyRule::Position => loc.row.to_string(),
        _ => rec.fields.first().cloned().unwrap_or_default(),
    };
    assert!(!key.is_empty(), "every addressable record has a key");
    Target::new(rec.kind, &key)
}

/// Leading letters of an instance name (`M12` → `M`), the prefix copies are renumbered with.
fn name_prefix(name: &str) -> &str {
    let end = name
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(name.len());
    let prefix = if end == 0 { "X" } else { &name[..end] };
    assert!(!prefix.is_empty());
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_prefixes_and_unique_names() {
        assert_eq!(name_prefix("M12"), "M");
        assert_eq!(name_prefix("x1[3:0]"), "x");
        assert_eq!(name_prefix("12"), "X");
        let doc = Document::new(schemify_format::FileKind::Shcomp);
        let mut taken = HashSet::new();
        assert_eq!(unique_name(&doc, "R", &mut taken), "R1");
        assert_eq!(unique_name(&doc, "R", &mut taken), "R2");
    }
}
