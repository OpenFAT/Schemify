//! Join and trim: collinear overlapping wires with the same attributes merge into one, and
//! wires cut where a connection point lies on them (interval cover per line).
//!
//! Contents:
//! - `pub(super) type Line`
//! - `pub(super) fn gcd`
//! - `pub(super) fn line_of`
//! - `pub(super) fn offset`
//! - `pub(super) fn along`
//! - `pub(super) fn covered`
//! - `pub fn join_trim`

use std::collections::{BTreeMap, HashMap, HashSet};

use schemify_doc::{DocHandle, Loc, Op, Project, TableId, Target};
use schemify_format::RecordKind;

use crate::edit::Ids;
use crate::geometry::Pt;

use super::stretch;

/// A wire's line: its reduced direction `(dx, dy)` (coprime, `dx > 0`, or `dx == 0` and
/// `dy > 0`) and the offset `dx·y − dy·x`, which is the same for every point of the line.
pub(super) type Line = (i64, i64, i64);

/// Attribute lines of a wire as merge sees them: sorted, with `bus` other than `true` left
/// out (it is the default). Wires merge only when these are equal.
type AttrKey = Vec<(String, Vec<String>)>;

pub(super) fn gcd(mut a: i64, mut b: i64) -> i64 {
    let (a0, b0) = (a, b);
    (a, b) = (a.abs(), b.abs());
    // Bounded: Euclid's algorithm takes O(log min(a, b)) steps.
    while b != 0 {
        (a, b) = (b, a % b);
    }
    assert!(a >= 0);
    debug_assert!(a == 0 || (a0 % a == 0 && b0 % a == 0), "a common divisor");
    a
}

/// The line through `a` and `b`.
///
/// # Panics
///
/// Panics if `a == b` (a zero-length wire has no direction; the reader rejects them, E026).
pub(super) fn line_of(a: Pt, b: Pt) -> Line {
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
    assert!(dx > 0 || (dx == 0 && dy > 0), "the canonical direction");
    (dx, dy, offset((dx, dy), a))
}

/// `dx·y − dy·x`: equal for all points of one line of direction `d`.
pub(super) fn offset(d: (i64, i64), p: Pt) -> i64 {
    d.0 * i64::from(p.1) - d.1 * i64::from(p.0)
}

/// Position of `p` along direction `d` (grows from a wire's first end to its second).
pub(super) fn along(d: (i64, i64), p: Pt) -> i64 {
    d.0 * i64::from(p.0) + d.1 * i64::from(p.1)
}

/// Whether position `t` lies in one of the sorted, disjoint closed intervals `iv`.
pub(super) fn covered(iv: &[(i64, i64)], t: i64) -> bool {
    debug_assert!(
        iv.windows(2).all(|w| w[0].1 < w[1].0),
        "sorted, disjoint intervals"
    );
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
///
/// # Panics
///
/// Never: a point between two wire ends fits in `i32`.
pub fn join_trim(project: &Project, h: DocHandle, ids: &mut Ids<'_>) -> Vec<Op> {
    let Some(doc) = project.doc(h) else {
        return Vec::new();
    };
    // Pins, ports and noconns (labels excluded: they never cut).
    let mut anchors: Vec<Pt> = stretch::fixed_points(project, h, doc, &HashSet::new());
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
            assert!(
                !m.is_empty() && m.len() <= rows.len(),
                "one interval per wire at most"
            );
            debug_assert!(
                m.windows(2).all(|w| w[0].1 < w[1].0),
                "sorted, disjoint intervals"
            );
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
        assert!(want.len() >= merged[g].len(), "cuts only split intervals");
        debug_assert!(want.is_sorted());
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
            .record(Loc {
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
                let id = ids.draw(doc).text;
                ops.push(Op::Insert {
                    rec: super::wire_rec(&id, pt(sp.0), pt(sp.1), &attrs),
                    at: None,
                });
            }
        }
    }
    ops
}
