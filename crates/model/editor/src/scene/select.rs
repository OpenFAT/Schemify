//! Selection over the scene's object table: point hits, box selection, the selected set and
//! its marks. In: the objects, a point or a box. Out: object indices, marks.
//!
//! Contents:
//! - `pub struct Selected`, `Selected::set`, `Selected::contains`, `Selected::universe`,
//!   `Selected::len`, `Selected::is_empty`
//! - `pub fn hit`
//! - `pub(super) fn hit_one`
//! - `pub fn selected_prims`
//! - `pub fn box_select`
//! - `pub fn marks`

use schemify_canvas::{CanvasPrims, Marks, PrimKind};

use super::refresh::stamped;
use super::{Objects, Scene};

/// The selected objects of a scene: a bitset over object indices beside the sorted list of
/// the set ones.
///
/// Why a bitset: every reader asks about one object per primitive or connection point (the
/// marks, the move preview on every frame of a drag, the export subset, the wire-start hit),
/// so a lookup must be O(1); a sorted index list alone would make those O(log selection)
/// each. Why the list: a change clears only the bits set before, so it costs O(old + new
/// selection) plus the growth of the bitset, not O(objects) as a `vec![false; objects]` did.
/// Capacity: [`CanvasPrims::CAPACITY`] objects, so `objects / 64` words.
#[derive(Clone, Debug, Default)]
pub struct Selected {
    words: Vec<u64>,
    pub(super) objs: Vec<u32>,
    pub(super) universe: u32,
}

/// Returns the object under world point `p` within `tol` milli-units: the nearest segment
/// object within `tol`, else the smallest box containing `p` (inflated by `tol`), so wires
/// win over the instance they cross and small symbols over big ones.
///
/// Cost: one integer pass over `objs.geom` (the box each object's hit area fits in, inflated
/// by `tol`), then the exact distance or box test on the few objects that pass it; candidates
/// are tested in ascending order, so ties resolve as a full scan resolves them.
pub fn hit(objs: &Objects, p: (f64, f64), tol: f64) -> Option<u32> {
    assert!(tol >= 0.0 && tol.is_finite());
    assert_eq!(objs.geom.len(), objs.seg.len());
    // Whole milli-units around `p`: an object whose box misses this box is farther than `tol`
    // (its coordinates are integers, so rounding outwards keeps every hit).
    // Clamped to i32, the comparisons below equal the i64 ones (every coordinate is an i32).
    let lim = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (x0, x1) = (lim((p.0 - tol).floor()), lim((p.0 + tol).ceil()));
    let (y0, y1) = (lim((p.1 - tol).floor()), lim((p.1 + tol).ceil()));
    let mut best_seg: Option<(f64, u32)> = None;
    let mut best_box: Option<(f64, u32)> = None;
    // ponytail: a predicted branch per object (≈ 1.6 ns at 10^5–10^6; a 64-bit mask per 64
    // objects measured slower without SSE4.1 min/max); a grid over the objects answers in
    // O(cell) but costs ~40 ns per object to build after every scene change.
    for (i, g) in objs.geom.iter().enumerate() {
        let near = g[0].min(g[2]) <= x1
            && g[0].max(g[2]) >= x0
            && g[1].min(g[3]) <= y1
            && g[1].max(g[3]) >= y0;
        if near {
            hit_one(objs, i, p, tol, &mut best_seg, &mut best_box);
        }
    }
    let r = best_seg.or(best_box).map(|b| b.1);
    assert!(r.is_none_or(|i| (i as usize) < objs.len()));
    r
}

/// The exact test of object `i` against `p` within `tol`, folded into the best segment (by
/// distance) and the best box (by area) so far; the first of equals wins.
pub(super) fn hit_one(
    objs: &Objects,
    i: usize,
    p: (f64, f64),
    tol: f64,
    best_seg: &mut Option<(f64, u32)>,
    best_box: &mut Option<(f64, u32)>,
) {
    let g = objs.geom[i];
    if objs.seg[i] {
        let d = seg_dist(p, g);
        if d <= tol && best_seg.is_none_or(|b| d < b.0) {
            *best_seg = Some((d, i as u32));
        }
    } else {
        let b = objs.bbox(i);
        let inside = p.0 >= f64::from(b[0]) - tol
            && p.0 <= f64::from(b[2]) + tol
            && p.1 >= f64::from(b[1]) - tol
            && p.1 <= f64::from(b[3]) + tol;
        let area = (f64::from(b[2]) - f64::from(b[0])) * (f64::from(b[3]) - f64::from(b[1]));
        if inside && best_box.is_none_or(|bb| area < bb.0) {
            *best_box = Some((area, i as u32));
        }
    }
}

/// The primitive rows `(kind, row)` of the selected objects, by table (lines, rects, arcs,
/// polylines, polygons, texts: kinds 0–5) then row: what [`marks`] and [`push_moved`](super::push_moved) read.
/// Computed once per selection or scene change, so a move preview frame costs O(selection).
///
/// Cost: O(selection · log rows) to find each object's rows by its stamp, plus a sort of the
/// rows found.
pub fn selected_prims(sc: &Scene, selected: &Selected, out: &mut Vec<(u8, u32)>) {
    assert_eq!(selected.universe(), sc.objs.len());
    out.clear();
    for kind in 0..6 {
        let at = out.len();
        let stamps = sc.stamps.cols()[kind];
        for &o in &selected.objs {
            let rows = stamped(stamps, sc.objs.stamp[o as usize]);
            debug_assert!(rows.clone().all(|r| sc.owner.cols()[kind][r] == o));
            out.extend(rows.map(|r| (kind as u8, r as u32)));
        }
        out[at..].sort_unstable();
    }
    assert!(out.len() <= sc.prims.len());
}

/// Objects selected by a box `[x0, y0, x1, y1]` (any corner order): fully inside it, or,
/// with `touch` (xschem's right-to-left drag), any object whose box meets it.
pub fn box_select(objs: &Objects, r: [f64; 4], touch: bool) -> Vec<u32> {
    assert!(r.iter().all(|v| v.is_finite()), "a finite box");
    assert!(objs.len() <= CanvasPrims::CAPACITY);
    // Object boxes are integers, so the f64 box rounds to the i32 one that keeps exactly the
    // same objects: `b <= x1` ⇔ `b <= floor(x1)`, `b >= x0` ⇔ `b >= ceil(x0)`.
    let lim = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (x0, y0) = (lim(r[0].min(r[2]).ceil()), lim(r[1].min(r[3]).ceil()));
    let (x1, y1) = (lim(r[0].max(r[2]).floor()), lim(r[1].max(r[3]).floor()));
    // Branchless compaction: selectivity runs 0–100 % as the box grows, so a branch per
    // object would mispredict; every index is stored, the count advances by the test.
    let mut out = vec![0u32; objs.len()];
    let mut n = 0;
    if touch {
        for (i, g) in objs.geom.iter().enumerate() {
            let (b0, b2) = (g[0].min(g[2]), g[0].max(g[2]));
            let (b1, b3) = (g[1].min(g[3]), g[1].max(g[3]));
            out[n] = i as u32;
            n += usize::from((b0 <= x1) & (b2 >= x0) & (b1 <= y1) & (b3 >= y0));
        }
    } else {
        for (i, g) in objs.geom.iter().enumerate() {
            let (b0, b2) = (g[0].min(g[2]), g[0].max(g[2]));
            let (b1, b3) = (g[1].min(g[3]), g[1].max(g[3]));
            out[n] = i as u32;
            n += usize::from((b0 >= x0) & (b2 <= x1) & (b1 >= y0) & (b3 <= y1));
        }
    }
    out.truncate(n);
    assert!(out.windows(2).all(|w| w[0] < w[1]));
    out
}

impl Selected {
    /// Selects exactly `objs` (any order, duplicates allowed) out of a scene of `universe`
    /// objects. Cost: O(previous + new selection), plus zeroing the words a larger scene adds.
    ///
    /// # Panics
    ///
    /// If `universe` exceeds the scene object capacity or an index is `>= universe`.
    pub fn set(&mut self, universe: usize, objs: impl IntoIterator<Item = u32>) {
        assert!(universe <= CanvasPrims::CAPACITY, "scene object capacity");
        for &o in &self.objs {
            self.words[o as usize / 64] &= !(1u64 << (o % 64));
        }
        self.objs.clear();
        self.objs.extend(objs);
        self.objs.sort_unstable();
        self.objs.dedup();
        self.words.resize(universe.div_ceil(64), 0);
        for &o in &self.objs {
            assert!((o as usize) < universe, "selected object out of the scene");
            self.words[o as usize / 64] |= 1u64 << (o % 64);
        }
        self.universe = universe as u32;
        debug_assert_eq!(
            self.words
                .iter()
                .map(|w| w.count_ones() as usize)
                .sum::<usize>(),
            self.objs.len(),
            "one bit per selected object"
        );
    }

    /// Whether object `o` is selected; false for any index outside the scene (`u32::MAX`, the
    /// owner of an unowned primitive, included).
    pub fn contains(&self, o: u32) -> bool {
        self.words
            .get(o as usize / 64)
            .is_some_and(|w| w >> (o % 64) & 1 != 0)
    }

    /// Number of scene objects the set was made for (the scene's `objs.len()` when current).
    pub fn universe(&self) -> usize {
        self.universe as usize
    }

    /// Number of selected objects.
    pub fn len(&self) -> usize {
        self.objs.len()
    }

    /// True when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.objs.is_empty()
    }
}

/// Recolors every primitive of a selected object in `color`, in table then row order, from
/// the selection's rows ([`selected_prims`] of `sc`). Cost: O(selected primitives).
pub fn marks(sc: &Scene, rows: &[(u8, u32)], color: u32, out: &mut Marks) {
    *out = Marks::default();
    const KINDS: [PrimKind; 5] = [
        PrimKind::Line,
        PrimKind::Rect,
        PrimKind::Arc,
        PrimKind::Polyline,
        PrimKind::Polygon,
    ];
    for &(kind, row) in rows {
        if let Some(&k) = KINDS.get(usize::from(kind)) {
            out.push(k, row, color);
        }
    }
    assert!(out.kind.len() <= sc.prims.len());
}

// ---- private ----

fn seg_dist(p: (f64, f64), g: [i32; 4]) -> f64 {
    let (ax, ay, bx, by) = (
        f64::from(g[0]),
        f64::from(g[1]),
        f64::from(g[2]),
        f64::from(g[3]),
    );
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - ax) * dx + (p.1 - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (ax + t * dx - p.0, ay + t * dy - p.1);
    (qx * qx + qy * qy).sqrt()
}
