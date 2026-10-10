//! Edit gestures as `doc` edit requests: pure transforms from (document tables, selection,
//! gesture parameters) to op lists. Every gesture becomes one transaction, so one undo
//! (edits spec §1–§2). The app never mutates design data itself (ARCHITECTURE invariant).
//!
//! New objects get ids drawn here (the request may supply them, edits spec §1) so the
//! editor can select what it just created.

use std::collections::HashSet;

use schemify_doc::{Document, Loc, NEW_ID, ObjId, Op, RecordBuf, TableId, Target};
use schemify_format::{IdRng, KeyRule, Orient, RecordKind, format_coord, fresh_id};

use crate::geometry::{Pt, Xform, anchor, transform_record};
use crate::scene::ObjKey;

/// Where a selected object lives in the document, if it still exists.
pub fn loc_of(doc: &Document, key: ObjKey) -> Option<Loc> {
    match key.table {
        TableId::Pins | TableId::Graphics => {
            (key.key < doc.table_len(key.table) as u32).then_some(Loc {
                table: key.table,
                row: key.key,
            })
        }
        t => doc.find_id(ObjId(key.key)).filter(|l| l.table == t),
    }
}

/// The edit target addressing `rec` at `loc` (id, name, or graphic row).
fn target(rec: &RecordBuf, loc: Loc) -> Target {
    let key = match rec.kind.key() {
        KeyRule::Position => loc.row.to_string(),
        _ => rec.fields.first().cloned().unwrap_or_default(),
    };
    Target::new(rec.kind, &key)
}

/// Records of the selection that still exist, with their targets, in selection order.
pub fn selected_records(doc: &Document, sel: &[ObjKey]) -> Vec<(Loc, Target, RecordBuf)> {
    let mut out = Vec::with_capacity(sel.len());
    for &k in sel {
        if let Some(loc) = loc_of(doc, k) {
            let rec = doc.record(loc);
            out.push((loc, target(&rec, loc), rec));
        }
    }
    out
}

/// Deletes the selection. Graphics are deleted from the highest row down so the rows of
/// the ones still to delete stay valid inside the transaction.
pub fn delete(doc: &Document, sel: &[ObjKey]) -> Vec<Op> {
    let mut recs = selected_records(doc, sel);
    recs.sort_by_key(|(loc, _, _)| std::cmp::Reverse((loc.table == TableId::Graphics, loc.row)));
    let ops: Vec<Op> = recs.into_iter().map(|(_, t, _)| Op::Delete(t)).collect();
    assert!(ops.len() <= sel.len());
    ops
}

/// Moves, rotates or flips the selection as a group: one `put` per record.
pub fn transform(doc: &Document, sel: &[ObjKey], x: Xform) -> Vec<Op> {
    selected_records(doc, sel)
        .into_iter()
        .filter_map(|(_, target, mut rec)| {
            transform_record(&mut rec, x)?;
            Some(Op::Put { target, rec })
        })
        .collect()
}

/// Rotates or flips every selected object about its own anchor (xschem Alt+R / Alt+F).
pub fn transform_each(doc: &Document, sel: &[ObjKey], about: fn(Pt) -> Xform) -> Vec<Op> {
    selected_records(doc, sel)
        .into_iter()
        .filter_map(|(_, target, mut rec)| {
            let a = anchor(&rec)?;
            transform_record(&mut rec, about(a))?;
            Some(Op::Put { target, rec })
        })
        .collect()
}

/// Draws fresh ids that are free in `doc` and not yet used by this transaction.
pub struct Ids<'a> {
    rng: &'a mut IdRng,
    used: HashSet<String>,
}

impl<'a> Ids<'a> {
    /// A drawer over `rng`.
    pub fn new(rng: &'a mut IdRng) -> Ids<'a> {
        Ids {
            rng,
            used: HashSet::new(),
        }
    }

    /// Returns a fresh 6-character id and its selection key value.
    pub fn draw(&mut self, doc: &Document) -> (String, u32) {
        let used = &self.used;
        let id = fresh_id(self.rng, |s| {
            used.contains(s)
                || ObjId::find(s, doc.strings()).is_some_and(|i| doc.find_id(i).is_some())
        });
        let text = id.as_str().to_owned();
        let raw = ObjId::decode(&text).expect("generated ids are 6 chars").0;
        self.used.insert(text.clone());
        (text, raw)
    }
}

/// Picks `<prefix><n>` with the smallest `n ≥ 1` not used by an instance of `doc` nor in
/// `taken` (names made earlier in the same transaction), and records it in `taken`.
pub fn unique_name(doc: &Document, prefix: &str, taken: &mut HashSet<String>) -> String {
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

/// Leading letters of an instance name (`M12` → `M`), the prefix copies are renumbered with.
fn name_prefix(name: &str) -> &str {
    let end = name
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(name.len());
    if end == 0 { "X" } else { &name[..end] }
}

/// Copies the selection through `x`: inserts with fresh ids (returned as selection keys),
/// renumbered instance names and suffixed pin names.
pub fn duplicate(
    doc: &Document,
    sel: &[ObjKey],
    x: Xform,
    ids: &mut Ids<'_>,
) -> (Vec<Op>, Vec<ObjKey>) {
    let recs: Vec<RecordBuf> = selected_records(doc, sel)
        .into_iter()
        .map(|(_, _, r)| r)
        .collect();
    insert_copies(doc, recs, x, ids)
}

/// Inserts `recs` (copies from this or another document, e.g. the clipboard) through `x`.
pub fn insert_copies(
    doc: &Document,
    recs: Vec<RecordBuf>,
    x: Xform,
    ids: &mut Ids<'_>,
) -> (Vec<Op>, Vec<ObjKey>) {
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
        if transform_record(&mut rec, x).is_none() {
            continue;
        }
        match rec.kind.key() {
            KeyRule::Id => {
                let (id, raw) = ids.draw(doc);
                rec.fields[0] = id;
                keys.push(ObjKey { table, key: raw });
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
    (ops, keys)
}

fn coord(v: i32) -> String {
    let mut s = String::new();
    format_coord(v, &mut s);
    s
}

/// Wire segments along `pts` (zero-length segments skipped), each with a fresh id.
pub fn wires(doc: &Document, pts: &[Pt], ids: &mut Ids<'_>) -> (Vec<Op>, Vec<ObjKey>) {
    let mut ops = Vec::new();
    let mut keys = Vec::new();
    for w in pts.windows(2) {
        if w[0] == w[1] {
            continue;
        }
        let (id, raw) = ids.draw(doc);
        let rec = RecordBuf::new(RecordKind::Wire)
            .field(&id)
            .field(&coord(w[0].0))
            .field(&coord(w[0].1))
            .field(&coord(w[1].0))
            .field(&coord(w[1].1));
        ops.push(Op::Insert { rec, at: None });
        keys.push(ObjKey {
            table: TableId::Wires,
            key: raw,
        });
    }
    (ops, keys)
}

/// A new instance of `cellref` at `at` named `name`.
pub fn instance(cellref: &str, at: Pt, o: Orient, name: &str, id: &str) -> RecordBuf {
    RecordBuf::new(RecordKind::Inst)
        .field(id)
        .field(cellref)
        .attr("at", &[&coord(at.0), &coord(at.1), o.as_str()])
        .attr("name", &[name])
}

/// A new `label`, `port` or `text` record.
pub fn placed(kind: RecordKind, at: Pt, o: Orient, text: &str, id: &str) -> RecordBuf {
    assert!(matches!(
        kind,
        RecordKind::Label | RecordKind::Port | RecordKind::Text
    ));
    RecordBuf::new(kind)
        .field(id)
        .field(&coord(at.0))
        .field(&coord(at.1))
        .field(o.as_str())
        .field(text)
}

/// A new symbol pin.
pub fn pin(name: &str, at: Pt) -> RecordBuf {
    RecordBuf::new(RecordKind::Pin)
        .field(name)
        .field("inout")
        .field(&coord(at.0))
        .field(&coord(at.1))
}

/// A new symbol `line` or `rect` graphic.
pub fn shape(rect: bool, a: Pt, b: Pt) -> RecordBuf {
    let kind = if rect {
        RecordKind::Rect
    } else {
        RecordKind::Line
    };
    let mut r = RecordBuf::new(kind);
    for v in [a.0, a.1, b.0, b.1] {
        r = r.field(&coord(v));
    }
    r
}

/// The id field placeholder doc replaces on insert (used for records whose id the editor
/// does not need to know).
pub const AUTO_ID: &str = NEW_ID;

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
