//! Stretch moves: a moved selection drags the wire ends attached to it, and "kissing" pins
//! (an instance pin touching another without a wire) get new wires, routed by mode.
//!
//! Contents:
//! - `pub(super) const ADDED_PER_OBJECT`
//! - `pub(super) fn fixed_points`
//! - `pub struct Stretched`
//! - `pub struct Moved`
//! - `pub fn stretch`

use std::collections::HashSet;

use schemify_doc::{DocHandle, Document, Op, Project, TableId, Target};
use schemify_format::RecordKind;

use crate::edit::{self, Ids};
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;

use super::{inst_pins, on_interior, points_of, route, wire_rec};

/// Wires one stretch may add (kissing wires and L corners) per selected object.
pub(super) const ADDED_PER_OBJECT: usize = 64;

/// The result of a stretch or kissing move.
#[derive(Debug, Default)]
pub struct Stretched {
    /// The transaction's ops: the moved selection, then the stretched and added wires.
    pub ops: Vec<Op>,
    /// Wires that were stretched (kept, one end moved) or added.
    pub wires: Vec<ObjKey>,
}

/// What a move carries: objects moved whole and the moving ends of partially selected wires.
#[derive(Clone, Copy, Debug)]
pub struct Moved<'a> {
    /// Objects moved whole.
    pub sel: &'a [ObjKey],
    /// Selected ends of partially selected wires (xschem's Ctrl+drag box): (wire, end
    /// position). A row whose position is not an end of its wire is ignored.
    pub ends: &'a [(ObjKey, Pt)],
    /// Unselected wires with an end on a moved connection point follow it (xschem
    /// "stretch"); without it only the partial ends move.
    pub follow: bool,
    /// Also adds a wire wherever a moved point touched an unselected pin, anchor or wire
    /// ("kissing"); needs `follow`.
    pub kiss: bool,
}

/// Every connection point of objects not in `sel` that a wire does not carry: instance
/// pins and label/port/noconn anchors (sorted, distinct).
pub(super) fn fixed_points(
    project: &Project,
    h: DocHandle,
    doc: &Document,
    sel: &HashSet<ObjKey>,
) -> Vec<Pt> {
    let mut out = Vec::new();
    let unselected = |table: TableId, id: u32| !sel.contains(&ObjKey { table, key: id });
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        if unselected(TableId::Instances, inst.id[i].0) {
            out.extend(inst_pins(project, h, doc, i));
        }
    }
    for (table, t) in [
        (TableId::Labels, doc.labels()),
        (TableId::Ports, doc.ports()),
    ] {
        for i in 0..t.id.len() {
            if unselected(table, t.id[i].0) {
                out.push((t.x[i], t.y[i]));
            }
        }
    }
    let n = doc.noconns();
    for i in 0..n.id.len() {
        if unselected(TableId::Noconns, n.id[i].0) {
            out.push((n.x[i], n.y[i]));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Moves `m.sel` through `x` (one `put` per record, as `edit::transform`) and keeps it
/// connected (xschem "stretch", `m.follow`): an unselected wire with an end on a moved
/// connection point gets that end moved (both ends: the whole wire moves; routed as an L by
/// Manhattan mode `manhattan` when it would turn diagonal; deleted when it shrinks to a
/// point). The ends `m.ends` of partially selected wires are moved points too, so they move
/// with any move. With `m.kiss`, every moved connection point that touched an unselected
/// pin, anchor or wire interior also gets a new wire from its old to its new position, so
/// no connection is lost.
///
/// # Panics
///
/// Panics if `x` is not a pure translation (stretches follow drags; rotations of attached
/// wires have no single meaning), if `m.kiss` is set without `m.follow`, or if an end row
/// names another table than wires.
pub fn stretch(
    project: &Project,
    h: DocHandle,
    m: Moved<'_>,
    x: Xform,
    manhattan: u8,
    ids: &mut Ids<'_>,
) -> Stretched {
    assert!(x.is_translation(), "stretch moves translate");
    assert!(m.follow || !m.kiss, "kissing implies stretching");
    let Some(doc) = project.doc(h) else {
        return Stretched::default();
    };
    let (sel, kiss) = (m.sel, m.kiss);
    let mut out = Stretched {
        ops: edit::transform(doc, sel, x),
        wires: Vec::new(),
    };
    let selset: HashSet<ObjKey> = sel.iter().copied().collect();
    let mut moved: Vec<Pt> = if m.follow {
        sel.iter()
            .flat_map(|&k| points_of(project, h, doc, k))
            .collect()
    } else {
        Vec::new()
    };
    for &(k, p) in m.ends {
        assert_eq!(k.table, TableId::Wires, "partial selections are wire ends");
        let Some(loc) = edit::loc_of(doc, k) else {
            continue; // deleted since it was selected
        };
        let (r, w) = (loc.row as usize, doc.wires());
        if p == (w.x1[r], w.y1[r]) || p == (w.x2[r], w.y2[r]) {
            moved.push(p);
        }
    }
    moved.sort_unstable();
    moved.dedup();
    let is_moved = |p: Pt| moved.binary_search(&p).is_ok();
    let w = doc.wires();
    let s = doc.strings();
    let budget = ADDED_PER_OBJECT * (sel.len() + m.ends.len()).max(1);
    let mut added = 0usize;
    for r in 0..w.id.len() {
        let key = ObjKey {
            table: TableId::Wires,
            key: w.id[r].0,
        };
        if selset.contains(&key) {
            continue;
        }
        let (a, b) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]));
        let (ma, mb) = (is_moved(a), is_moved(b));
        if !ma && !mb {
            continue;
        }
        let target = Target::new(RecordKind::Wire, &w.id[r].text(s));
        let rec = doc.record(schemify_doc::Loc {
            table: TableId::Wires,
            row: r as u32,
        });
        out.wires.push(key);
        if ma && mb {
            let mut moved_rec = rec;
            if crate::geometry::transform_record(&mut moved_rec, x).is_some() {
                out.ops.push(Op::Put {
                    target,
                    rec: moved_rec,
                });
            }
            continue;
        }
        let (fixed, end) = if ma { (b, x.point(a)) } else { (a, x.point(b)) };
        let legs = route(fixed, end, manhattan);
        if legs.len() < 2 {
            out.ops.push(Op::Delete(target));
            continue;
        }
        let id = rec.fields[0].clone();
        out.ops.push(Op::Put {
            target,
            rec: wire_rec(&id, legs[0], legs[1], &rec.attrs),
        });
        for leg in legs[1..].windows(2) {
            if added < budget {
                let (nid, raw) = ids.draw(doc);
                out.ops.push(Op::Insert {
                    rec: wire_rec(&nid, leg[0], leg[1], &rec.attrs),
                    at: None,
                });
                out.wires.push(ObjKey {
                    table: TableId::Wires,
                    key: raw,
                });
                added += 1;
            }
        }
    }
    if kiss {
        let fixed = fixed_points(project, h, doc, &selset);
        let unselected_wires: Vec<(Pt, Pt)> = (0..w.id.len())
            .filter(|&r| {
                !selset.contains(&ObjKey {
                    table: TableId::Wires,
                    key: w.id[r].0,
                })
            })
            .map(|r| ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r])))
            .collect();
        for &m in &moved {
            let to = x.point(m);
            if to == m || added >= budget {
                continue;
            }
            let touches = fixed.binary_search(&m).is_ok()
                || unselected_wires.iter().any(|&(a, b)| on_interior(m, a, b));
            if !touches {
                continue;
            }
            for leg in route(m, to, manhattan).windows(2) {
                let (nid, raw) = ids.draw(doc);
                out.ops.push(Op::Insert {
                    rec: wire_rec(&nid, leg[0], leg[1], &[]),
                    at: None,
                });
                out.wires.push(ObjKey {
                    table: TableId::Wires,
                    key: raw,
                });
                added += 1;
            }
        }
    }
    out.wires.sort_unstable();
    out.wires.dedup();
    assert!(added <= budget + 2, "bounded additions");
    out
}
