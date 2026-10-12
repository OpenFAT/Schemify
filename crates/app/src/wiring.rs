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

use std::collections::{BTreeMap, HashMap, HashSet};

use schemify_doc::{DocHandle, Document, Op, Project, RecordBuf, TableId, Target};
use schemify_format::{RecordKind, format_coord, parse_coord};

use crate::edit::{self, Ids};
use crate::geometry::{Pt, Xform, place, snap};
use crate::scene::ObjKey;

/// Wires one stretch may add (kissing wires and L corners) per selected object.
const ADDED_PER_OBJECT: usize = 64;

/// World positions of the interface pins of instance row `i` (empty when its cell does not
/// resolve), in pin order.
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
    let (o, at) = (inst.orient[i], (inst.x[i], inst.y[i]));
    let out: Vec<Pt> = (0..pins.x.len())
        .map(|k| place(o, at, (pins.x[k], pins.y[k])))
        .collect();
    assert_eq!(out.len(), pins.x.len());
    out
}

/// Whether `p` lies on segment `a`–`b`, end points excluded (exact integer test).
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

fn coord_text(v: i32) -> String {
    let mut s = String::new();
    format_coord(v, &mut s);
    s
}

fn wire_rec(id: &str, a: Pt, b: Pt, attrs: &[schemify_doc::Attr]) -> RecordBuf {
    let mut r = RecordBuf::new(RecordKind::Wire)
        .field(id)
        .field(&coord_text(a.0))
        .field(&coord_text(a.1))
        .field(&coord_text(b.0))
        .field(&coord_text(b.1));
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

/// Every connection point of objects not in `sel` that a wire does not carry: instance
/// pins and label/port/noconn anchors (sorted, distinct).
fn fixed_points(project: &Project, h: DocHandle, doc: &Document, sel: &HashSet<ObjKey>) -> Vec<Pt> {
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

/// Points of a wire from `a` to `b` routed by Manhattan `mode` (0 straight, 1 horizontal
/// first, 2 vertical first), zero-length legs dropped.
pub fn route(a: Pt, b: Pt, mode: u8) -> Vec<Pt> {
    let mut pts = match mode {
        1 if a.0 != b.0 && a.1 != b.1 => vec![a, (b.0, a.1), b],
        2 if a.0 != b.0 && a.1 != b.1 => vec![a, (a.0, b.1), b],
        _ => vec![a, b],
    };
    pts.dedup();
    pts
}

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

/// A wire's line: its reduced direction `(dx, dy)` (coprime, `dx > 0`, or `dx == 0` and
/// `dy > 0`) and the offset `dx·y − dy·x`, which is the same for every point of the line.
type Line = (i64, i64, i64);

/// Attribute lines of a wire as merge sees them: sorted, with `bus` other than `true` left
/// out (it is the default). Wires merge only when these are equal.
type AttrKey = Vec<(String, Vec<String>)>;

fn gcd(mut a: i64, mut b: i64) -> i64 {
    (a, b) = (a.abs(), b.abs());
    // Bounded: Euclid's algorithm takes O(log min(a, b)) steps.
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The line through `a` and `b`.
///
/// # Panics
///
/// Panics if `a == b` (a zero-length wire has no direction; the reader rejects them, E026).
fn line_of(a: Pt, b: Pt) -> Line {
    let (dx, dy) = (
        i64::from(b.0) - i64::from(a.0),
        i64::from(b.1) - i64::from(a.1),
    );
    let g = gcd(dx, dy);
    assert!(g > 0, "a wire has two distinct ends");
    let (mut dx, mut dy) = (dx / g, dy / g);
    if dx < 0 || (dx == 0 && dy < 0) {
        (dx, dy) = (-dx, -dy);
    }
    (dx, dy, offset((dx, dy), a))
}

/// `dx·y − dy·x`: equal for all points of one line of direction `d`.
fn offset(d: (i64, i64), p: Pt) -> i64 {
    d.0 * i64::from(p.1) - d.1 * i64::from(p.0)
}

/// Position of `p` along direction `d` (grows from a wire's first end to its second).
fn along(d: (i64, i64), p: Pt) -> i64 {
    d.0 * i64::from(p.0) + d.1 * i64::from(p.1)
}

/// Whether position `t` lies in one of the sorted, disjoint closed intervals `iv`.
fn covered(iv: &[(i64, i64)], t: i64) -> bool {
    let i = iv.partition_point(|x| x.0 <= t);
    i > 0 && iv[i - 1].1 >= t
}

/// Joins and trims wires (xschem "Join/Trim wires"): collinear wires (axis-aligned or
/// diagonal) with equal attributes that overlap or touch are merged and duplicates disappear;
/// a merged wire is cut where another wire ends on it (a T junction) and where its pieces met
/// at a pin, port or noconn (that joint is kept). Labels never cut. Wires whose attributes
/// differ never merge (`bus false` counts as no attribute); where they overlap, each is cut
/// at the other's ends. Idempotent: on a trimmed document it returns no ops. Connectivity is
/// unchanged (elab connects points on wire interiors). A merged wire carries the attributes
/// of the first wire of its group (by row).
///
/// Cost: O(W log W + E·D) with E the wire ends and D the distinct wire directions.
pub fn join_trim(project: &Project, h: DocHandle, ids: &mut Ids<'_>) -> Vec<Op> {
    let Some(doc) = project.doc(h) else {
        return Vec::new();
    };
    // Pins, ports and noconns (labels excluded: they never cut).
    let mut anchors: Vec<Pt> = fixed_points(project, h, doc, &HashSet::new());
    let labels: Vec<Pt> = (0..doc.labels().id.len())
        .map(|i| (doc.labels().x[i], doc.labels().y[i]))
        .collect();
    anchors.retain(|p| !labels.contains(p));
    let w = doc.wires();
    let s = doc.strings();
    let ends = |r: usize| [(w.x1[r], w.y1[r]), (w.x2[r], w.y2[r])];
    // Groups: (line, attributes) → rows, ordered (BTreeMap) so ids are drawn deterministically.
    let mut by_key: BTreeMap<(Line, AttrKey), Vec<usize>> = BTreeMap::new();
    for r in 0..w.id.len() {
        let [a, b] = ends(r);
        if a == b {
            continue;
        }
        let mut key: AttrKey = doc
            .pool()
            .attrs(w.attrs[r])
            .map(|(k, v)| {
                let v: Vec<String> = v.iter().map(|&x| s.get(x).to_owned()).collect();
                (s.get(k).to_owned(), v)
            })
            .filter(|(k, v)| k != "bus" || v.first().map(String::as_str) == Some("true"))
            .collect();
        key.sort();
        by_key.entry((line_of(a, b), key)).or_default().push(r);
    }
    let groups: Vec<((Line, AttrKey), Vec<usize>)> = by_key.into_iter().collect();
    // Each group's merged intervals along its line (sorted, disjoint).
    let merged: Vec<Vec<(i64, i64)>> = groups
        .iter()
        .map(|((l, _), rows)| {
            let d = (l.0, l.1);
            let mut iv: Vec<(i64, i64)> = rows
                .iter()
                .map(|&r| {
                    let [a, b] = ends(r);
                    let (ta, tb) = (along(d, a), along(d, b));
                    (ta.min(tb), ta.max(tb))
                })
                .collect();
            iv.sort_unstable();
            let mut m: Vec<(i64, i64)> = Vec::with_capacity(iv.len());
            for (lo, hi) in iv {
                match m.last_mut() {
                    Some(x) if lo <= x.1 => x.1 = x.1.max(hi),
                    _ => m.push((lo, hi)),
                }
            }
            m
        })
        .collect();
    let mut on_line: HashMap<Line, Vec<usize>> = HashMap::new();
    for (g, ((l, _), _)) in groups.iter().enumerate() {
        on_line.entry(*l).or_default().push(g);
    }
    let mut dirs: Vec<(i64, i64)> = groups.iter().map(|((l, _), _)| (l.0, l.1)).collect();
    dirs.sort_unstable();
    dirs.dedup();
    // Junctions: wire ends lying on wires of two or more groups (a T, an L, one wire ending
    // on another's crossing, overlapping wires that may not merge). Every group is cut at the
    // junctions on its line — computed before anything merges, so trimming the result again
    // finds the same points (a fixpoint). Bucketed by line for the per-group pass.
    let mut points: Vec<Pt> = (0..w.id.len()).flat_map(ends).collect();
    points.sort_unstable();
    points.dedup();
    let mut cuts_on: HashMap<Line, Vec<i64>> = HashMap::new();
    for p in points {
        let lines: Vec<Line> = dirs
            .iter()
            .map(|&d| (d.0, d.1, offset(d, p)))
            .filter(|l| on_line.contains_key(l))
            .collect();
        let count: usize = lines
            .iter()
            .map(|l| {
                let t = along((l.0, l.1), p);
                on_line[l]
                    .iter()
                    .filter(|&&g| covered(&merged[g], t))
                    .count()
            })
            .sum();
        if count >= 2 {
            for l in lines {
                cuts_on.entry(l).or_default().push(along((l.0, l.1), p));
            }
        }
    }
    let mut ops = Vec::new();
    for (g, ((line, _), rows)) in groups.iter().enumerate() {
        let d = (line.0, line.1);
        // Cut points on this line: the junctions, and the group's own wire ends where a pin,
        // port or noconn sits (kept joints).
        let mut cuts: Vec<i64> = cuts_on.get(line).cloned().unwrap_or_default();
        for &r in rows {
            for p in ends(r) {
                if anchors.binary_search(&p).is_ok() {
                    cuts.push(along(d, p));
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut want: Vec<(i64, i64)> = Vec::new();
        for &(lo, hi) in &merged[g] {
            let mut start = lo;
            for &c in cuts.iter().filter(|&&c| c > lo && c < hi) {
                want.push((start, c));
                start = c;
            }
            want.push((start, hi));
        }
        let span = |r: usize| {
            let [a, b] = ends(r);
            let (ta, tb) = (along(d, a), along(d, b));
            (ta.min(tb), ta.max(tb))
        };
        let mut have: Vec<((i64, i64), usize)> = rows.iter().map(|&r| (span(r), r)).collect();
        have.sort_unstable();
        if have.iter().map(|h| h.0).eq(want.iter().copied()) {
            continue; // already trimmed
        }
        // Positions back to points: every lattice point of the line is p0 + k·d.
        let p0 = ends(rows[0])[0];
        let (t0, n2) = (along(d, p0), d.0 * d.0 + d.1 * d.1);
        let pt = |t: i64| -> Pt {
            assert_eq!(
                (t - t0) % n2,
                0,
                "cut points are lattice points of the line"
            );
            let k = (t - t0) / n2;
            let c = |p: i32, dp: i64| {
                i32::try_from(i64::from(p) + k * dp).expect("on the line between wire ends")
            };
            (c(p0.0, d.0), c(p0.1, d.1))
        };
        let attrs = doc
            .record(schemify_doc::Loc {
                table: TableId::Wires,
                row: rows[0] as u32,
            })
            .attrs;
        // Keep the wires whose span is wanted as is (first occurrence), replace the rest.
        let mut kept: HashSet<(i64, i64)> = HashSet::new();
        for (sp, r) in &have {
            let keep = want.binary_search(sp).is_ok() && kept.insert(*sp);
            if !keep {
                ops.push(Op::Delete(Target::new(RecordKind::Wire, &w.id[*r].text(s))));
            }
        }
        for sp in &want {
            if !kept.contains(sp) {
                let (id, _) = ids.draw(doc);
                ops.push(Op::Insert {
                    rec: wire_rec(&id, pt(sp.0), pt(sp.1), &attrs),
                    at: None,
                });
            }
        }
    }
    ops
}

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
            .filter(|&p| on_interior(p, a, b))
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
            rec: wire_rec(&id, chain[0], chain[1], &rec.attrs),
        });
        pieces.push((chain[0], chain[1]));
        for leg in chain[1..].windows(2) {
            let (nid, _) = ids.draw(doc);
            ops.push(Op::Insert {
                rec: wire_rec(&nid, leg[0], leg[1], &rec.attrs),
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
            let mut p = points_of(project, h, doc, k);
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

fn rec_pt(rec: &RecordBuf, i: usize) -> Pt {
    let c = |k: usize| {
        rec.fields
            .get(k)
            .and_then(|t| parse_coord(t).ok())
            .unwrap_or(i32::MIN)
    };
    (c(i), c(i + 1))
}

/// The wires, labels and ports reachable from `seeds` through shared points (wire ends,
/// anchors, seed instances' pins; a T junction joins a wire end to another wire's interior).
/// With `stop_at_junctions` the walk does not continue through a point where three or more
/// wire ends meet or a T joins (xschem Ctrl+right button).
///
/// Cost: O(visited × W) for the T test; bounded by the wire count.
pub fn connected(
    project: &Project,
    h: DocHandle,
    seeds: &[ObjKey],
    stop_at_junctions: bool,
) -> Vec<ObjKey> {
    let Some(doc) = project.doc(h) else {
        return Vec::new();
    };
    let w = doc.wires();
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
        if stop_at_junctions && junction && !is_seed_point {
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
pub fn align(doc: &Document, sel: &[ObjKey], snap_m: i32) -> Vec<Op> {
    assert!(snap_m > 0, "snap must be positive");
    let keys: Vec<ObjKey> = if sel.is_empty() {
        edit::schematic_keys(doc)
    } else {
        sel.to_vec()
    };
    let snapped = |t: &str| -> Option<String> {
        let v = parse_coord(t).ok()?;
        Some(coord_text(snap(f64::from(v), snap_m)))
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
        if rec.kind == RecordKind::Wire && rec_pt(&rec, 1) == rec_pt(&rec, 3) {
            ops.push(Op::Delete(target));
        } else {
            ops.push(Op::Put { target, rec });
        }
    }
    ops
}

#[cfg(test)]
mod tests {
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
        assert_eq!(line_of((0, 0), (10, 0)), (1, 0, 0));
        assert_eq!(line_of((10, 5), (0, 5)), (1, 0, 5));
        assert_eq!(line_of((0, 0), (0, -7)), (0, 1, 0));
        assert_eq!(line_of((3, 0), (3, 9)), (0, 1, -3));
        assert_eq!(line_of((0, 0), (4, 4)), (1, 1, 0));
        assert_eq!(line_of((6, 6), (2, 2)), line_of((0, 0), (4, 4)));
        assert_ne!(line_of((0, 1), (4, 5)), line_of((0, 0), (4, 4)), "parallel");
        assert_eq!(line_of((0, 0), (6, -4)), (3, -2, 0), "reduced by the gcd");
        assert_eq!(gcd(-12, 18), 6);
        assert!(covered(&[(0, 10), (20, 30)], 10));
        assert!(covered(&[(0, 10), (20, 30)], 20));
        assert!(!covered(&[(0, 10), (20, 30)], 15));
        assert!(!covered(&[(0, 10)], -1));
        assert!(!covered(&[], 0));
    }

    #[test]
    fn routes_are_l_shapes_without_zero_legs() {
        assert_eq!(route((0, 0), (5, 5), 1), [(0, 0), (5, 0), (5, 5)]);
        assert_eq!(route((0, 0), (5, 5), 2), [(0, 0), (0, 5), (5, 5)]);
        assert_eq!(route((0, 0), (5, 0), 1), [(0, 0), (5, 0)]);
        assert_eq!(route((1, 1), (1, 1), 0), [(1, 1)]);
    }
}
