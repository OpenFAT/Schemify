//! What changed between two versions of a canvas's tables, as world boxes (D39): an edit
//! re-rasters only those regions of the canvas layer ([`crate::canvas::draw_cached`]).
//!
//! Six questions:
//! 1. In: the primitive tables a canvas layer draws, and its marks, once per content change.
//!    Out: world boxes covering every row that was added or removed, where "kept" rows are a
//!    longest common subsequence of the two versions (so kept rows also keep their order:
//!    outside the boxes a fresh recording draws exactly what the layer holds).
//! 2. 10^5–10^6 rows per canvas; an edit changes a handful.
//! 3. Boxes in i32 milli-units (D33); a `u64` hash per row stands for the row.
//! 4. One linear pass hashing every row of a table that changed, then a diff of its two hash
//!    sequences; an unchanged table (a selection edits only the marks) costs nothing.
//! 5. The snapshot (hashes, boxes) lives with the canvas until its next change.
//! 6. Tables are independent (parallelizable; ~1 ms per 10^5 rows, so not yet needed).
//!
//! SIMD triage: hashing is per row over a few SoA columns (variable width for polys and
//! texts); the diff is a chain. Scalar.

use crate::canvas::{CanvasPrims, Marks, PrimKind};

/// Edits (rows removed plus rows added) one table may differ by before the diff gives up and
/// the whole layer is recorded instead.
pub const DIFF_EDITS_MAX: usize = 128;

/// The rows a canvas layer was drawn from, as hashes and world boxes, to diff the next
/// version against. One per cached canvas.
#[derive(Clone, Debug, Default)]
pub struct CanvasDiff {
    /// Per table: the six kinds of each input in order, then the marks.
    seqs: Vec<Seq>,
    /// The sequences replaced by the last call, kept for their allocations.
    spare: Vec<Seq>,
    /// Per sequence: the rows the last call found removed and added (see
    /// [`CanvasDiff::edits_of`]).
    last: Vec<(Vec<u32>, Vec<u32>)>,
}

#[derive(Clone, Debug, Default)]
struct Seq {
    hash: Vec<u64>,
    boxes: Vec<[i32; 4]>,
}

/// The six tables of [`CanvasPrims`], in drawing order within a kind.
#[derive(Clone, Copy)]
pub(crate) enum Table {
    Lines,
    Rects,
    Arcs,
    Polylines,
    Polygons,
    Texts,
}

pub(crate) const TABLES: [Table; 6] = [
    Table::Lines,
    Table::Rects,
    Table::Arcs,
    Table::Polylines,
    Table::Polygons,
    Table::Texts,
];

impl CanvasDiff {
    /// Diffs `tables` (drawn in this order, each with whether it may have changed since the
    /// previous call) and `marks` (recoloring rows of `marks.0`; may have changed when they
    /// or `marks.0` did) against the previous call's, and appends to `out` the world boxes
    /// of every row added or removed since. A table flagged unchanged must be unchanged: it
    /// is neither hashed nor diffed. Returns false when the change is not small (or there was
    /// no previous call, or a different number of tables): then `out` is meaningless.
    ///
    /// Cost: O(rows) of the changed tables to hash, plus O(rows + edits²) each to diff.
    pub fn update(
        &mut self,
        tables: &[(&CanvasPrims, bool)],
        marks: (&CanvasPrims, &Marks, bool),
        out: &mut Vec<[i32; 4]>,
    ) -> bool {
        let n = tables.len() * TABLES.len() + 1;
        let first = self.seqs.len() != n;
        if first {
            self.seqs = vec![Seq::default(); n];
        }
        self.spare.resize_with(n, Seq::default);
        self.last.resize_with(n, Default::default);
        for (g, a) in &mut self.last {
            g.clear();
            a.clear();
        }
        let (mut gone, mut new) = (Vec::new(), Vec::new());
        let mut known = !first;
        let jobs = tables
            .iter()
            .flat_map(|&(t, changed)| TABLES.map(|table| (Some((t, table)), changed)))
            .chain([(None, marks.2)]);
        for (k, (job, changed)) in jobs.enumerate() {
            if !changed && !first {
                continue;
            }
            let s = &mut self.spare[k];
            match job {
                Some((t, table)) => fill(s, t, table),
                None => fill_marks(s, marks.0, marks.1),
            }
            let (a, b) = (&self.seqs[k], &self.spare[k]);
            gone.clear();
            new.clear();
            known = known && edits(&a.hash, &b.hash, &mut gone, &mut new);
            if known {
                out.extend(gone.iter().map(|&i| a.boxes[i as usize]));
                out.extend(new.iter().map(|&i| b.boxes[i as usize]));
                self.last[k].0.extend_from_slice(&gone);
                self.last[k].1.extend_from_slice(&new);
            }
            std::mem::swap(&mut self.seqs[k], &mut self.spare[k]);
        }
        assert_eq!(self.seqs.len(), n);
        known
    }
}

impl CanvasDiff {
    /// The rows of input table `table` (an index into the last [`CanvasDiff::update`]'s
    /// `tables`) that call found removed (old indices) and added (new indices), per kind in
    /// [`CanvasPrims`] order: what [`PrimIndex::apply`](crate::canvas::PrimIndex::apply)
    /// follows. Empty for a table flagged unchanged; meaningless when that call returned
    /// false.
    pub fn edits_of(&self, table: usize) -> [(&[u32], &[u32]); 6] {
        assert!(
            self.last.len() > (table + 1) * TABLES.len(),
            "a table of the last call"
        );
        std::array::from_fn(|k| {
            let (g, a) = &self.last[table * TABLES.len() + k];
            (&g[..], &a[..])
        })
    }
}

/// Refills `s` with the rows of `table`: one tight loop per kind.
fn fill(s: &mut Seq, t: &CanvasPrims, table: Table) {
    let n = len(t, table);
    s.hash.clear();
    s.boxes.clear();
    s.hash.reserve(n);
    s.boxes.reserve(n);
    let mut push = |(h, b): (u64, [i32; 4])| {
        s.hash.push(h);
        s.boxes.push(b);
    };
    match table {
        Table::Lines => (0..n).for_each(|i| push(row(t, Table::Lines, i))),
        Table::Rects => (0..n).for_each(|i| push(row(t, Table::Rects, i))),
        Table::Arcs => (0..n).for_each(|i| push(row(t, Table::Arcs, i))),
        Table::Polylines => (0..n).for_each(|i| push(row(t, Table::Polylines, i))),
        Table::Polygons => (0..n).for_each(|i| push(row(t, Table::Polygons, i))),
        Table::Texts => (0..n).for_each(|i| push(row(t, Table::Texts, i))),
    }
    assert_eq!(s.hash.len(), n);
}

pub(crate) fn len(t: &CanvasPrims, table: Table) -> usize {
    match table {
        Table::Lines => t.lines.x0.len(),
        Table::Rects => t.rects.x0.len(),
        Table::Arcs => t.arcs.cx.len(),
        Table::Polylines => t.polylines.end.len(),
        Table::Polygons => t.polygons.end.len(),
        Table::Texts => t.texts.x.len(),
    }
}

/// Folds one field into a row hash (deterministic on every target).
fn mix(h: u64, v: i64) -> u64 {
    (h.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95)
}

fn fold(seed: i64, fields: &[i32]) -> u64 {
    fields
        .iter()
        .fold(mix(0, seed), |h, &v| mix(h, i64::from(v)))
}

/// Row `i` of `table`: its hash (everything drawn about it) and its world box.
#[inline]
pub(crate) fn row(t: &CanvasPrims, table: Table, i: usize) -> (u64, [i32; 4]) {
    let span =
        |x0: i32, y0: i32, x1: i32, y1: i32| [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    match table {
        Table::Lines => {
            let l = &t.lines;
            let f = [l.x0[i], l.y0[i], l.x1[i], l.y1[i], i32::from(l.layer[i])];
            (fold(1, &f), span(f[0], f[1], f[2], f[3]))
        }
        Table::Rects => {
            let r = &t.rects;
            let f = [r.x0[i], r.y0[i], r.x1[i], r.y1[i], i32::from(r.layer[i])];
            (fold(2, &f), span(f[0], f[1], f[2], f[3]))
        }
        Table::Arcs => {
            let a = &t.arcs;
            let (cx, cy, r) = (a.cx[i], a.cy[i], a.r[i]);
            let f = [cx, cy, r, a.start[i], a.sweep[i], i32::from(a.layer[i])];
            let b = [
                cx.saturating_sub(r),
                cy.saturating_sub(r),
                cx.saturating_add(r),
                cy.saturating_add(r),
            ];
            (fold(3, &f), b)
        }
        Table::Polylines | Table::Polygons => {
            let (seed, p) = match table {
                Table::Polylines => (4, &t.polylines),
                _ => (5, &t.polygons),
            };
            let start = if i == 0 { 0 } else { p.end[i - 1] as usize };
            let (xs, ys) = (
                &p.x[start..p.end[i] as usize],
                &p.y[start..p.end[i] as usize],
            );
            assert!(xs.len() >= 2);
            let h = xs
                .iter()
                .zip(ys)
                .fold(fold(seed, &[i32::from(p.layer[i])]), |h, (&x, &y)| {
                    mix(mix(h, i64::from(x)), i64::from(y))
                });
            let b = [
                *xs.iter().min().expect("2 points"),
                *ys.iter().min().expect("2 points"),
                *xs.iter().max().expect("2 points"),
                *ys.iter().max().expect("2 points"),
            ];
            (h, b)
        }
        Table::Texts => {
            let tx = &t.texts;
            let start = if i == 0 { 0 } else { tx.end[i - 1] as usize };
            let bytes = &tx.text.as_bytes()[start..tx.end[i] as usize];
            let rot = tx.rot.get(i).copied().unwrap_or(0);
            let f = [
                tx.x[i],
                tx.y[i],
                tx.size[i],
                i32::from(tx.layer[i]),
                i32::from(rot),
            ];
            let h = bytes.iter().fold(fold(6, &f), |h, &b| mix(h, i64::from(b)));
            // Labels hang right and down from their anchor: at most an em per byte across and
            // two lines down (what `draw_texts` treats as visible), padded by an em; a turned
            // label reaches as far in any direction.
            let (x, y, em) = (i64::from(f[0]), i64::from(f[1]), i64::from(f[2]));
            let reach = em * (bytes.len() as i64 + 1).max(3);
            let c = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            let b = if rot == 0 {
                [c(x - em), c(y - em), c(x + reach), c(y + 3 * em)]
            } else {
                [c(x - reach), c(y - reach), c(x + reach), c(y + reach)]
            };
            (h, b)
        }
    }
}

/// The marks as rows: what they recolor (the marked row's content, not its index, which
/// moves when other rows are removed) and in which color.
fn fill_marks(s: &mut Seq, prims: &CanvasPrims, m: &Marks) {
    assert!(m.kind.len() == m.index.len() && m.kind.len() == m.color.len());
    s.hash.clear();
    s.boxes.clear();
    for i in 0..m.kind.len() {
        let table = match m.kind[i] {
            PrimKind::Line => Table::Lines,
            PrimKind::Rect => Table::Rects,
            PrimKind::Arc => Table::Arcs,
            PrimKind::Polyline => Table::Polylines,
            PrimKind::Polygon => Table::Polygons,
        };
        let (h, b) = row(prims, table, m.index[i] as usize);
        s.hash.push(mix(h, i64::from(m.color[i])));
        s.boxes.push(b);
    }
    assert_eq!(s.hash.len(), m.kind.len());
}

/// Pushes the indices of `a` (removed) and `b` (added) outside one longest common
/// subsequence of the two: Myers' O((N+M)·D) diff after trimming the common prefix and
/// suffix. Returns false, leaving the lists partial, past [`DIFF_EDITS_MAX`] edits or a work
/// budget of a few passes over the rows.
fn edits(a: &[u64], b: &[u64], gone: &mut Vec<u32>, new: &mut Vec<u32>) -> bool {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a1, b1) = (&a[pre..], &b[pre..]);
    let suf = a1
        .iter()
        .rev()
        .zip(b1.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a1, b1) = (&a1[..a1.len() - suf], &b1[..b1.len() - suf]);
    let (n, m) = (a1.len() as isize, b1.len() as isize);
    let at = |i: isize| (pre as isize + i) as u32;
    let dmax = DIFF_EDITS_MAX as isize;
    if n == 0 || m == 0 {
        // Only removals or only additions: no search needed.
        gone.extend((0..n).map(at));
        new.extend((0..m).map(at));
        return n + m <= dmax;
    }
    // v[k + off]: furthest x on diagonal k (x - y = k); one snapshot of v per edit count.
    let off = dmax + 1;
    let mut v = vec![0isize; (2 * off + 1) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut budget = 4 * (n + m) + 4096;
    let mut end = None;
    let pick = |v: &[isize], d: isize, k: isize| {
        k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize])
    };
    'search: for d in 0..=dmax {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            let mut x = if pick(&v, d, k) {
                v[(off + k + 1) as usize]
            } else {
                v[(off + k - 1) as usize] + 1
            };
            let (x0, mut y) = (x, x - k);
            while x < n && y < m && a1[x as usize] == b1[y as usize] {
                x += 1;
                y += 1;
            }
            budget -= 1 + x - x0;
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                end = Some(d);
                break 'search;
            }
        }
        if budget < 0 {
            return false;
        }
    }
    let Some(dend) = end else {
        return false;
    };
    // Walk back from (n, m): each step came off diagonal k ± 1 as one addition (y moved)
    // or one removal (x moved), then slid down its snake.
    let (mut x, mut y) = (n, m);
    for d in (1..=dend).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let down = pick(v, d, k);
        let pk = if down { k + 1 } else { k - 1 };
        let px = v[(off + pk) as usize];
        let py = px - pk;
        if down {
            new.push(at(py));
        } else {
            gone.push(at(px));
        }
        (x, y) = (px, py);
    }
    assert_eq!(gone.len() + new.len(), dend as usize);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: dropping `gone` from `a` and `new` from `b` leaves the same sequence, as long
    /// as a quadratic LCS says it can be.
    #[test]
    fn edits_are_a_minimal_script() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |m: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % m
        };
        for round in 0..400 {
            let n = next(60) as usize;
            let a: Vec<u64> = (0..n).map(|_| next(6)).collect();
            let mut b = a.clone();
            for _ in 0..next(6) {
                match next(3) {
                    0 if !b.is_empty() => {
                        let i = next(b.len() as u64) as usize;
                        b.swap_remove(i);
                    }
                    1 if !b.is_empty() => {
                        b.remove(next(b.len() as u64) as usize);
                    }
                    _ => b.insert(next(b.len() as u64 + 1) as usize, next(6)),
                }
            }
            let (mut gone, mut new) = (Vec::new(), Vec::new());
            assert!(edits(&a, &b, &mut gone, &mut new), "round {round}");
            let kept = |s: &[u64], drop: &[u32]| -> Vec<u64> {
                (0..s.len())
                    .filter(|i| !drop.contains(&(*i as u32)))
                    .map(|i| s[i])
                    .collect()
            };
            let (ka, kb) = (kept(&a, &gone), kept(&b, &new));
            assert_eq!(ka, kb, "round {round}: kept rows agree");
            let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
            for i in 0..a.len() {
                for j in 0..b.len() {
                    lcs[i + 1][j + 1] = if a[i] == b[j] {
                        lcs[i][j] + 1
                    } else {
                        lcs[i][j + 1].max(lcs[i + 1][j])
                    };
                }
            }
            assert_eq!(ka.len(), lcs[a.len()][b.len()], "round {round}: minimal");
        }
    }

    #[test]
    fn a_large_change_gives_up() {
        let a: Vec<u64> = (0..10_000).collect();
        let b: Vec<u64> = (0..10_000).map(|i| i * 7 % 10_000).collect();
        assert!(!edits(&a, &b, &mut Vec::new(), &mut Vec::new()));
        let (mut gone, mut new) = (Vec::new(), Vec::new());
        assert!(edits(&a, &a, &mut gone, &mut new));
        assert!(gone.is_empty() && new.is_empty());
    }
}
