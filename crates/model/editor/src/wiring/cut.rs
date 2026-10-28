//! Cutting wires: at given points ([`break_at`]), and through a box ([`remove_through`]).
//!
//! Contents:
//! - `pub fn break_at`
//! - `pub fn remove_through`
//! - `pub(super) fn rec_pt`

use std::collections::HashSet;

use schemify_doc::{DocHandle, Document, Op, Project, RecordBuf, TableId, Target};
use schemify_format::RecordKind;

use crate::edit::Ids;
use crate::geometry::Pt;
use crate::scene::ObjKey;

/// Cuts every wire that has one of `points` strictly inside it into pieces ending there.
/// Returns the ops and, per cut wire, the new pieces' end points.
pub fn break_at(doc: &Document, points: &[Pt], ids: &mut Ids<'_>) -> (Vec<Op>, Vec<(Pt, Pt)>) {
    let w = doc.wires();
    let s = doc.strings();
    let mut ops = Vec::new();
    let mut pieces = Vec::new();
    for r in 0..w.id.len() {
        let (a, b) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]));
        let mut inside: Vec<Pt> = points
            .iter()
            .copied()
            .filter(|&p| super::on_interior(p, a, b))
            .collect();
        if inside.is_empty() {
            continue;
        }
        let d = |p: Pt| {
            (i64::from(p.0) - i64::from(a.0)).abs() + (i64::from(p.1) - i64::from(a.1)).abs()
        };
        inside.sort_unstable_by_key(|&p| (d(p), p));
        inside.dedup();
        let rec = doc.record(schemify_doc::Loc {
            table: TableId::Wires,
            row: r as u32,
        });
        let mut chain = vec![a];
        chain.extend(inside);
        chain.push(b);
        let id = rec.fields[0].clone();
        ops.push(Op::Put {
            target: Target::new(RecordKind::Wire, &w.id[r].text(s)),
            rec: super::wire_rec(&id, chain[0], chain[1], &rec.attrs),
        });
        pieces.push((chain[0], chain[1]));
        for leg in chain[1..].windows(2) {
            let nid = ids.draw(doc).text;
            ops.push(Op::Insert {
                rec: super::wire_rec(&nid, leg[0], leg[1], &rec.attrs),
                at: None,
            });
            pieces.push((leg[0], leg[1]));
        }
    }
    (ops, pieces)
}

/// Xschem "Remove wires running through selected inst. pins": cuts wires at the pins of the
/// selected instances, then deletes every piece whose two ends are pins of one selected
/// instance (a wire shorting across the symbol).
pub fn remove_through(
    project: &Project,
    h: DocHandle,
    sel: &[ObjKey],
    ids: &mut Ids<'_>,
) -> Vec<Op> {
    let Some(doc) = project.doc(h) else {
        return Vec::new();
    };
    let per_inst: Vec<Vec<Pt>> = sel
        .iter()
        .filter(|k| k.table == TableId::Instances)
        .map(|&k| {
            let mut p = super::points_of(project, h, doc, k);
            p.sort_unstable();
            p
        })
        .collect();
    let all: Vec<Pt> = per_inst.iter().flatten().copied().collect();
    let shorting = |a: Pt, b: Pt| {
        per_inst
            .iter()
            .any(|p| p.binary_search(&a).is_ok() && p.binary_search(&b).is_ok())
    };
    let (cut, _) = break_at(doc, &all, ids);
    let mut ops = Vec::with_capacity(cut.len());
    let mut cut_ids: HashSet<String> = HashSet::new();
    for op in cut {
        match op {
            Op::Put { target, rec } => {
                cut_ids.insert(rec.fields[0].clone());
                if shorting(rec_pt(&rec, 1), rec_pt(&rec, 3)) {
                    ops.push(Op::Delete(target));
                } else {
                    ops.push(Op::Put { target, rec });
                }
            }
            Op::Insert { rec, at } => {
                if !shorting(rec_pt(&rec, 1), rec_pt(&rec, 3)) {
                    ops.push(Op::Insert { rec, at });
                }
            }
            other => ops.push(other),
        }
    }
    // Uncut wires that already run from pin to pin of one instance.
    let w = doc.wires();
    let s = doc.strings();
    for r in 0..w.id.len() {
        let id = w.id[r].text(s);
        if !cut_ids.contains(&id) && shorting((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r])) {
            ops.push(Op::Delete(Target::new(RecordKind::Wire, &id)));
        }
    }
    ops
}

pub(super) fn rec_pt(rec: &RecordBuf, i: usize) -> Pt {
    let c = |k: usize| {
        rec.fields
            .get(k)
            .and_then(|t| schemify_format::parse_coord(t).ok())
            .unwrap_or(i32::MIN)
    };
    (c(i), c(i + 1))
}
