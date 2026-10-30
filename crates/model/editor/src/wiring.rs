//! `wiring`: wire-topology edits as `doc` op lists — stretch and "kissing" moves, join/trim,
//! cutting wires, connected-wire selection and align-to-grid. Pure transforms from (document
//! tables, instance interfaces, parameters) to ops; the editor commits each result as one
//! transaction (one undo), like `edit`.
//!
//! Data design:
//! 1. In: the active document's rank-3 tables, its instances' interface pins (through
//!    `Project::interface`), a selection, a transform. Out: `Op` lists and object keys.
//! 2. How many: up to 2^22 wires per cell; interactive edits touch a selection's worth.
//! 3. Widths: coordinates `i32` milli-units (D33); rows `usize` while transforming.
//! 4. Access: connection points sorted once per call (`Vec<Pt>` + binary search); wires
//!    scanned linearly.
//! 5. Lifetime: per command.
//! 6. Parallel: no (interactive, cold path).
//!
//! Connectivity follows elab (netlist spec §3): objects connect where a connection point
//! (instance pin, wire end, label/port/noconn anchor) equals another or lies on a wire.
//!
//! Contents:
//! - `pub fn inst_pins`
//! - `pub fn on_interior`
//! - `pub fn route`
//! - `pub enum Junctions`, `pub fn connected`
//! - `pub fn align`
//! - `pub(crate) fn wires_in_step`, `pub(crate) fn instances_in_step`,
//!   `pub(crate) fn placed_in_step`

mod cut;
mod join;
mod stretch;

use std::collections::HashSet;

use schemify_doc::{
    Attr, DocHandle, Document, Instances, Op, Placed, Project, RecordBuf, TableId, Wires,
};
use schemify_format::RecordKind;

use crate::geometry::Pt;
use crate::scene::ObjKey;
use crate::{edit, geometry};

pub use cut::{break_at, remove_through};
pub use join::join_trim;
pub use stretch::{Moved, Stretched, stretch};

/// World positions of the interface pins of instance row `i` (empty when its cell does not
/// resolve), in pin order.
///
/// # Panics
///
/// If `i` is not an instance row of `doc`.
#[must_use]
pub fn inst_pins(project: &Project, h: DocHandle, doc: &Document, i: usize) -> Vec<Pt> {
    let inst = doc.instances();
    assert!(i < inst.id.len(), "instance row {i}");
    let cellref = doc.strings().get(inst.cell[i]);
    let Some(iface) = project
        .resolve(h, cellref)
        .ok()
        .and_then(|r| project.interface(r))
    else {
        return Vec::new();
    };
    let pins = iface.pins();
    assert_eq!(pins.y.len(), pins.x.len(), "pin columns in step");
    let (o, at) = (inst.orient[i], (inst.x[i], inst.y[i]));
    (0..pins.x.len())
        .map(|k| geometry::place(o, at, (pins.x[k], pins.y[k])))
        .collect()
}

/// Whether `p` lies on segment `a`–`b`, end points excluded (exact integer test).
#[must_use]
pub fn on_interior(p: Pt, a: Pt, b: Pt) -> bool {
    if p == a || p == b {
        return false;
    }
    let (px, py) = (i64::from(p.0), i64::from(p.1));
    let (ax, ay, bx, by) = (
        i64::from(a.0),
        i64::from(a.1),
        i64::from(b.0),
        i64::from(b.1),
    );
    let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
    cross == 0 && px >= ax.min(bx) && px <= ax.max(bx) && py >= ay.min(by) && py <= ay.max(by)
}

/// Points of a wire from `a` to `b` routed by Manhattan `mode` (0 straight, 1 horizontal
/// first, 2 vertical first), zero-length legs dropped.
///
/// # Panics
///
/// If `mode` is past 2.
#[must_use]
pub fn route(a: Pt, b: Pt, mode: u8) -> Vec<Pt> {
    assert!(
        mode <= 2,
        "Manhattan mode {mode}: straight, horizontal or vertical first"
    );
    let mut pts = match mode {
        1 if a.0 != b.0 && a.1 != b.1 => vec![a, (b.0, a.1), b],
        2 if a.0 != b.0 && a.1 != b.1 => vec![a, (a.0, b.1), b],
        _ => vec![a, b],
    };
    pts.dedup();
    assert!(
        pts.windows(2).all(|l| l[0].0 == l[1].0 || l[0].1 == l[1].1) || mode == 0,
        "a routed wire's legs are Manhattan"
    );
    pts
}

/// Whether a [`connected`] walk continues through a junction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Junctions {
    /// The walk continues through every point.
    Cross,
    /// The walk does not continue through a point where three or more wire ends meet or a
    /// T joins (xschem Ctrl+right button).
    Stop,
}

/// The wires, labels and ports reachable from `seeds` through shared points (wire ends,
/// anchors, seed instances' pins; a T junction joins a wire end to another wire's interior),
/// across or up to junctions as `junctions` says.
///
/// Cost: O(visited × W) for the T test; bounded by the wire count.
///
/// # Panics
///
/// Never: the assert checks that each point is visited once.
#[must_use]
pub fn connected(
    project: &Project,
    h: DocHandle,
    seeds: &[ObjKey],
    junctions: Junctions,
) -> Vec<ObjKey> {
    let Some(doc) = project.doc(h) else {
        return Vec::new();
    };
    let w = doc.wires();
    assert!(wires_in_step(w), "wire columns in step");
    let ends = |r: usize| [(w.x1[r], w.y1[r]), (w.x2[r], w.y2[r])];
    // Wire-end index: point → wire rows.
    let mut at: Vec<(Pt, usize)> = (0..w.id.len())
        .flat_map(|r| ends(r).map(|p| (p, r)))
        .collect();
    at.sort_unstable();
    let rows_at = |p: Pt| {
        let lo = at.partition_point(|e| e.0 < p);
        let hi = at.partition_point(|e| e.0 <= p);
        at[lo..hi].iter().map(|e| e.1).collect::<Vec<_>>()
    };
    let mut seen = vec![false; w.id.len()];
    let mut queue: Vec<Pt> = seeds
        .iter()
        .flat_map(|&k| points_of(project, h, doc, k))
        .collect();
    let mut visited_pts: HashSet<Pt> = HashSet::new();
    // Every visited point is a wire end or a seed point.
    let bound = at.len() + queue.len() + 1;
    // Bounded: every point is expanded once and every wire marked once.
    while let Some(p) = queue.pop() {
        if !visited_pts.insert(p) {
            continue;
        }
        let mut here = rows_at(p);
        // T junction: wires whose interior holds p.
        let mut interior: Vec<usize> = (0..w.id.len())
            .filter(|&r| {
                let [a, b] = ends(r);
                on_interior(p, a, b)
            })
            .collect();
        let junction = here.len() + 2 * interior.len() >= 3;
        let is_seed_point = visited_pts.len() == 1;
        if junctions == Junctions::Stop && junction && !is_seed_point {
            continue;
        }
        here.append(&mut interior);
        for r in here {
            if !seen[r] {
                seen[r] = true;
                let [a, b] = ends(r);
                queue.extend([a, b]);
                // Wires ending on this one's interior (T junctions) branch off it.
                queue.extend(at.iter().map(|e| e.0).filter(|&p| on_interior(p, a, b)));
            }
        }
        assert!(visited_pts.len() <= bound, "points visited once each");
    }
    let mut out: Vec<ObjKey> = (0..w.id.len())
        .filter(|&r| seen[r])
        .map(|r| ObjKey {
            table: TableId::Wires,
            key: w.id[r].0,
        })
        .collect();
    // Labels and ports on a reached point or along a reached wire.
    let on_seen = |p: Pt| {
        (0..w.id.len()).any(|r| {
            let [a, b] = ends(r);
            seen[r] && on_interior(p, a, b)
        })
    };
    for (table, t) in [
        (TableId::Labels, doc.labels()),
        (TableId::Ports, doc.ports()),
    ] {
        assert!(placed_in_step(t), "{table:?} columns in step");
        for i in 0..t.id.len() {
            let p = (t.x[i], t.y[i]);
            if visited_pts.contains(&p) || on_seen(p) {
                out.push(ObjKey {
                    table,
                    key: t.id[i].0,
                });
            }
        }
    }
    out.extend(seeds.iter().copied());
    out.sort_unstable();
    out.dedup();
    out
}

/// Snaps every coordinate of the records `sel` (every rank-3 object when empty) to
/// multiples of `snap` (xschem Alt+U). A wire that collapses to a point is deleted.
///
/// # Panics
///
/// Panics if `snap` is not positive.
#[must_use]
pub fn align(doc: &Document, sel: &[ObjKey], snap_m: i32) -> Vec<Op> {
    assert!(snap_m > 0, "snap must be positive");
    let keys: Vec<ObjKey> = if sel.is_empty() {
        edit::schematic_keys(doc)
    } else {
        sel.to_vec()
    };
    let snapped = |t: &str| -> Option<String> {
        let v = schemify_format::parse_coord(t).ok()?;
        Some(geometry::coord(geometry::snap(f64::from(v), snap_m)))
    };
    let mut ops = Vec::new();
    for (_, target, mut rec) in edit::selected_records(doc, &keys) {
        let before = rec.clone();
        let slots: Vec<usize> = match rec.kind {
            RecordKind::Wire => vec![1, 2, 3, 4],
            RecordKind::Label | RecordKind::Port | RecordKind::Text | RecordKind::Noconn => {
                vec![1, 2]
            }
            RecordKind::Pin => vec![2, 3],
            RecordKind::Line | RecordKind::Rect => vec![0, 1, 2, 3],
            RecordKind::Circle | RecordKind::Arc | RecordKind::Stext => vec![0, 1],
            RecordKind::Poly => (0..rec.fields.len()).collect(),
            RecordKind::Shape => match rec.fields[1].as_str() {
                "arc" | "circle" => vec![2, 3],
                _ => (2..rec.fields.len()).collect(),
            },
            RecordKind::Inst => {
                if let Some(a) = rec.attrs.iter_mut().find(|a| a.key == "at") {
                    for v in a.values.iter_mut().take(2) {
                        if let Some(s) = snapped(v) {
                            *v = s;
                        }
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        };
        for i in slots {
            if let Some(s) = rec.fields.get(i).and_then(|t| snapped(t)) {
                rec.fields[i] = s;
            }
        }
        if rec == before {
            continue;
        }
        if rec.kind == RecordKind::Wire && cut::rec_pt(&rec, 1) == cut::rec_pt(&rec, 3) {
            ops.push(Op::Delete(target));
        } else {
            ops.push(Op::Put { target, rec });
        }
    }
    ops
}

/// Whether the wire table's columns are in step: the `doc` invariant every scan here
/// indexes by.
pub(crate) fn wires_in_step(w: &Wires) -> bool {
    let n = w.id.len();
    w.x1.len() == n && w.y1.len() == n && w.x2.len() == n && w.y2.len() == n
}

/// Whether the instance table's columns are in step.
pub(crate) fn instances_in_step(t: &Instances) -> bool {
    let n = t.id.len();
    t.cell.len() == n
        && t.name.len() == n
        && t.x.len() == n
        && t.y.len() == n
        && t.orient.len() == n
}

/// Whether a label, port or text table's columns are in step.
pub(crate) fn placed_in_step(t: &Placed) -> bool {
    let n = t.id.len();
    t.x.len() == n && t.y.len() == n && t.orient.len() == n && t.text.len() == n
}

// ---- private ----

fn wire_rec(id: &str, a: Pt, b: Pt, attrs: &[Attr]) -> RecordBuf {
    let mut r = RecordBuf::new(RecordKind::Wire)
        .field(id)
        .field(&geometry::coord(a.0))
        .field(&geometry::coord(a.1))
        .field(&geometry::coord(b.0))
        .field(&geometry::coord(b.1));
    r.attrs = attrs.to_vec();
    r
}

/// The connection points of object `key`: an instance's pins, a wire's ends, a label's,
/// port's or noconn's anchor; nothing for texts and shapes.
fn points_of(project: &Project, h: DocHandle, doc: &Document, key: ObjKey) -> Vec<Pt> {
    let Some(loc) = edit::loc_of(doc, key) else {
        return Vec::new();
    };
    let r = loc.row as usize;
    match key.table {
        TableId::Instances => inst_pins(project, h, doc, r),
        TableId::Wires => {
            let w = doc.wires();
            vec![(w.x1[r], w.y1[r]), (w.x2[r], w.y2[r])]
        }
        TableId::Labels => vec![(doc.labels().x[r], doc.labels().y[r])],
        TableId::Ports => vec![(doc.ports().x[r], doc.ports().y[r])],
        TableId::Noconns => vec![(doc.noconns().x[r], doc.noconns().y[r])],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::join;
    use super::*;

    #[test]
    fn interior_points_exclude_ends_and_off_line_points() {
        let (a, b) = ((0, 0), (10, 0));
        assert!(on_interior((5, 0), a, b));
        assert!(!on_interior((0, 0), a, b));
        assert!(!on_interior((5, 1), a, b));
        assert!(!on_interior((11, 0), a, b));
        assert!(on_interior((2, 2), (0, 0), (4, 4)), "diagonal");
    }

    #[test]
    fn lines_are_canonical_in_both_directions_and_on_diagonals() {
        assert_eq!(join::line_of((0, 0), (10, 0)), (1, 0, 0));
        assert_eq!(join::line_of((10, 5), (0, 5)), (1, 0, 5));
        assert_eq!(join::line_of((0, 0), (0, -7)), (0, 1, 0));
        assert_eq!(join::line_of((3, 0), (3, 9)), (0, 1, -3));
        assert_eq!(join::line_of((0, 0), (4, 4)), (1, 1, 0));
        assert_eq!(join::line_of((6, 6), (2, 2)), join::line_of((0, 0), (4, 4)));
        assert_ne!(
            join::line_of((0, 1), (4, 5)),
            join::line_of((0, 0), (4, 4)),
            "parallel"
        );
        assert_eq!(
            join::line_of((0, 0), (6, -4)),
            (3, -2, 0),
            "reduced by the gcd"
        );
        assert_eq!(join::gcd(-12, 18), 6);
        assert!(join::covered(&[(0, 10), (20, 30)], 10));
        assert!(join::covered(&[(0, 10), (20, 30)], 20));
        assert!(!join::covered(&[(0, 10), (20, 30)], 15));
        assert!(!join::covered(&[(0, 10)], -1));
        assert!(!join::covered(&[], 0));
    }

    #[test]
    fn routes_are_l_shapes_without_zero_legs() {
        assert_eq!(route((0, 0), (5, 5), 1), [(0, 0), (5, 0), (5, 5)]);
        assert_eq!(route((0, 0), (5, 5), 2), [(0, 0), (0, 5), (5, 5)]);
        assert_eq!(route((0, 0), (5, 0), 1), [(0, 0), (5, 0)]);
        assert_eq!(route((1, 1), (1, 1), 0), [(1, 1)]);
    }
}
