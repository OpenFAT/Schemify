//! What changed between two versions of a canvas's tables, as rows removed and added (D39):
//! the instance rows and the label indexes follow an edit in O(change).
//!
//! Six questions:
//! 1. In: the primitive tables a canvas draws, and its marks, once per content change. Out:
//!    per table and kind the rows added and removed, where "kept" rows are a longest common
//!    subsequence of the two versions (so kept rows also keep their order).
//! 2. 10^5–10^6 rows per canvas; an edit changes a handful.
//! 3. Row indices `u32`; a `u64` hash per row stands for the row.
//! 4. One linear pass hashing every row of a table that changed, then a diff of its two hash
//!    sequences; an unchanged table (a selection edits only the marks) costs nothing, and a
//!    table whose producer reports its edits ([`TableChange::Rows`]: rows killed, rows
//!    appended) costs O(change).
//! 5. The snapshot (hashes) lives with the canvas until its next change.
//! 6. Tables are independent (parallelizable; ~1 ms per 10^5 rows, so not yet needed).
//!
//! SIMD triage: hashing is per row over a few SoA columns (variable width for polys and
//! texts); the diff is a chain. Scalar. The diff is Myers' O((N+M)·D) algorithm (Myers 1986,
//! "An O(ND) Difference Algorithm and Its Variations").
//!
//! Contents:
//! - `pub const DIFF_EDITS_MAX: usize`
//! - `pub struct CanvasDiff`, `CanvasDiff::update`, `CanvasDiff::edits_of`
//! - `pub enum TableChange<'a> { Same, Any, Rows(&'a [Vec<u32>; 6]) }`

use crate::row::{TABLES, Table, len, mix, row};
use crate::{CanvasPrims, Marks, PrimKind};

/// Edits (rows removed plus rows added) one table may differ by before the diff gives up and
/// the whole layer is recorded instead.
pub const DIFF_EDITS_MAX: usize = 128;

/// The rows a canvas was drawn from, as hashes, to diff the next version against. One per
/// canvas.
#[derive(Clone, Debug, Default)]
pub struct CanvasDiff {
    /// Per sequence (the six kinds of each input table in order, then the marks): the hashes
    /// of the version the last call saw.
    seqs: Vec<Seq>,
    /// The sequences replaced by the last call, kept for their allocations.
    spare: Vec<Seq>,
    /// Per sequence: the rows the last call found removed and added (see
    /// [`CanvasDiff::edits_of`]).
    last: Vec<(Vec<u32>, Vec<u32>)>,
}

/// How one table changed since the previous [`CanvasDiff::update`].
#[derive(Clone, Copy, Debug)]
pub enum TableChange<'a> {
    /// Not at all: neither hashed nor diffed.
    Same,
    /// In any way: every row is hashed and the two versions diffed.
    Any,
    /// Only by its producer killing rows (`killed[kind]`: rows of the previous version, in
    /// [`CanvasPrims::kill`]'s kind order, now on [`LAYER_DEAD`](crate::LAYER_DEAD); text
    /// rows listed there may instead be rewritten in place with the same box, as texts are
    /// drawn from the table every frame) and appending rows past the previous version's end;
    /// every other row is as it was.
    Rows(&'a [Vec<u32>; 6]),
}

#[derive(Clone, Debug, Default)]
struct Seq {
    hash: Vec<u64>,
    /// Rows of the version the last call saw.
    rows: usize,
    /// Whether `hash` describes that version (a [`TableChange::Rows`] call leaves it stale:
    /// the next hashed diff of the table then finds the change unknown).
    hashed: bool,
}

impl CanvasDiff {
    /// Diffs `tables` (drawn in this order, each with how it changed since the previous
    /// call) and `marks` (recoloring rows of `marks.0`; may have changed when they or
    /// `marks.0` did, flagged by `marks.2`) against the previous call's: every row added or
    /// removed since, read back per table by [`CanvasDiff::edits_of`]. A table reported
    /// [`TableChange::Same`] must be unchanged, and one reported [`TableChange::Rows`] changed
    /// exactly as reported: neither is hashed. Returns false when the change is not small (or
    /// there was no previous call, a different number of tables, a reported table that
    /// shrank, or a hashed diff of a table whose last change was reported): then the edits
    /// are meaningless.
    ///
    /// Cost: O(rows) of the [`TableChange::Any`] tables to hash, plus O(rows + edits²) each
    /// to diff; O(killed + appended rows) for a [`TableChange::Rows`] one.
    ///
    /// # Panics
    ///
    /// If a reported killed row is past the table, or a mark is past its table.
    pub fn update(
        &mut self,
        tables: &[(&CanvasPrims, TableChange<'_>)],
        marks: (&CanvasPrims, &Marks, bool),
    ) -> bool {
        let n = tables.len() * TABLES.len() + 1;
        let first = self.seqs.len() != n;
        if first {
            self.seqs = vec![Seq::default(); n];
        }
        self.spare.resize_with(n, Seq::default);
        self.last.resize_with(n, Default::default);
        for (gone, new) in &mut self.last {
            gone.clear();
            new.clear();
        }
        // Once one sequence is unknown the call returns false: later ones are still hashed
        // (the next call diffs against them) but no longer diffed.
        let mut known = !first;
        for (j, &(t, change)) in tables.iter().enumerate() {
            for (kind, table) in TABLES.into_iter().enumerate() {
                let k = j * TABLES.len() + kind;
                match change {
                    TableChange::Same if !first => {}
                    TableChange::Rows(killed) => {
                        known = self.reported(k, len(t, table), &killed[kind], known);
                    }
                    TableChange::Same | TableChange::Any => {
                        fill(&mut self.spare[k], t, table);
                        known = self.diffed(k, known);
                    }
                }
            }
        }
        if marks.2 || first {
            fill_marks(&mut self.spare[n - 1], marks.0, marks.1);
            known = self.diffed(n - 1, known);
        }
        assert_eq!(self.seqs.len(), n);
        assert_eq!(self.last.len(), n);
        known
    }

    /// The rows of input table `table` (an index into the last [`CanvasDiff::update`]'s
    /// `tables`) that call found removed (old indices) and added (new indices), ascending, per
    /// kind in [`CanvasPrims`] order: what [`PrimIndex::apply`](crate::PrimIndex::apply)
    /// follows. Empty for a table flagged unchanged; meaningless when that call returned
    /// false.
    ///
    /// # Panics
    ///
    /// If `table` is not an input of the last call.
    pub fn edits_of(&self, table: usize) -> [(&[u32], &[u32]); 6] {
        assert!(
            self.last.len() > (table + 1) * TABLES.len(),
            "a table of the last call"
        );
        std::array::from_fn(|k| {
            let (gone, new) = &self.last[table * TABLES.len() + k];
            (&gone[..], &new[..])
        })
    }
}

// ---- private ----

impl CanvasDiff {
    /// Takes a producer's report for sequence `k`, now `now` rows long: the killed rows go
    /// out and in again (their content changed), the appended rows in. Leaves the hashes
    /// stale. Returns whether the edits are still known.
    fn reported(&mut self, k: usize, now: usize, killed: &[u32], known: bool) -> bool {
        let s = &mut self.seqs[k];
        let was = s.rows;
        (s.rows, s.hashed) = (now, false);
        let known = known && now >= was;
        if known {
            let (gone, new) = &mut self.last[k];
            // A row appended and killed since the last call is among the appended.
            for &r in killed.iter().filter(|&&r| (r as usize) < was) {
                assert!((r as usize) < now, "rows are killed, never removed");
                gone.push(r);
                new.push(r);
            }
            new.extend(was as u32..now as u32);
            gone.sort_unstable();
            new.sort_unstable();
        }
        known
    }

    /// Diffs the freshly hashed `spare[k]` against `seqs[k]` (when still `known`), then makes
    /// it the current version. Returns whether the edits are still known.
    fn diffed(&mut self, k: usize, known: bool) -> bool {
        let (old, new) = (&self.seqs[k], &self.spare[k]);
        assert!(new.hashed, "a diffed sequence is hashed");
        let (gone, added) = &mut self.last[k];
        let known = known && old.hashed && edits(&old.hash, &new.hash, gone, added);
        std::mem::swap(&mut self.seqs[k], &mut self.spare[k]);
        known
    }
}

/// Refills `s` with the hashes of `table`'s rows: one tight loop per kind.
fn fill(s: &mut Seq, t: &CanvasPrims, table: Table) {
    let n = len(t, table);
    s.hash.clear();
    s.hash.reserve(n);
    (s.rows, s.hashed) = (n, true);
    // Matched outside the loop so each loop inlines one kind's `row`.
    let h = &mut s.hash;
    match table {
        Table::Lines => h.extend((0..n).map(|i| row(t, Table::Lines, i).0)),
        Table::Rects => h.extend((0..n).map(|i| row(t, Table::Rects, i).0)),
        Table::Arcs => h.extend((0..n).map(|i| row(t, Table::Arcs, i).0)),
        Table::Polylines => h.extend((0..n).map(|i| row(t, Table::Polylines, i).0)),
        Table::Polygons => h.extend((0..n).map(|i| row(t, Table::Polygons, i).0)),
        Table::Texts => h.extend((0..n).map(|i| row(t, Table::Texts, i).0)),
    }
    assert_eq!(s.hash.len(), n);
}

/// The marks as rows: what they recolor (the marked row's content, not its index, which
/// moves when other rows are removed) and in which color.
fn fill_marks(s: &mut Seq, prims: &CanvasPrims, m: &Marks) {
    assert_eq!(m.index.len(), m.kind.len(), "index column in step");
    assert_eq!(m.color.len(), m.kind.len(), "color column in step");
    s.hash.clear();
    (s.rows, s.hashed) = (m.kind.len(), true);
    for i in 0..m.kind.len() {
        let table = match m.kind[i] {
            PrimKind::Line => Table::Lines,
            PrimKind::Rect => Table::Rects,
            PrimKind::Arc => Table::Arcs,
            PrimKind::Polyline => Table::Polylines,
            PrimKind::Polygon => Table::Polygons,
        };
        let (h, _) = row(prims, table, m.index[i] as usize);
        s.hash.push(mix(h, i64::from(m.color[i])));
    }
    assert_eq!(s.hash.len(), m.kind.len());
}

/// Pushes the indices of `a` (removed) and `b` (added) outside one longest common
/// subsequence of the two, ascending: Myers' diff after trimming the common prefix and
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
    let down = |v: &[isize], d: isize, k: isize| {
        k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize])
    };
    'search: for d in 0..=dmax {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            let mut x = if down(&v, d, k) {
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
    // or one removal (x moved), then slid down its snake. The walk runs backwards, so each
    // list comes out descending and is reversed at the end.
    let (g0, n0) = (gone.len(), new.len());
    let (mut x, mut y) = (n, m);
    for d in (1..=dend).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let down = down(v, d, k);
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
    gone[g0..].reverse();
    new[n0..].reverse();
    assert_eq!(gone.len() - g0 + new.len() - n0, dend as usize);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::rng;

    /// Oracle: dropping `gone` from `a` and `new` from `b` leaves the same sequence, as long
    /// as a quadratic LCS says it can be; both lists ascend.
    #[test]
    fn edits_are_a_minimal_script() {
        let mut next = rng(0x9e37_79b9_7f4a_7c15);
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
            assert!(gone.windows(2).all(|w| w[0] < w[1]), "round {round}");
            assert!(new.windows(2).all(|w| w[0] < w[1]), "round {round}");
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

    /// A reported edit gives exactly the killed rows and the appended ones; negative space: a
    /// reported table that shrank, and a hashed diff right after a reported one (its hashes
    /// are stale), are unknown.
    #[test]
    fn reported_rows_give_their_edits() {
        let mut t = CanvasPrims::default();
        for i in 0..10 {
            t.line(i * 100, 0, i * 100 + 50, 10, 1);
        }
        let (mut d, marks) = (CanvasDiff::default(), Marks::default());
        assert!(!d.update(&[(&t, TableChange::Any)], (&t, &marks, true)));
        t.kill(0, 3);
        t.line(5_000, 5_000, 5_010, 5_020, 4);
        let mut killed: [Vec<u32>; 6] = Default::default();
        killed[0].push(3);
        assert!(d.update(&[(&t, TableChange::Rows(&killed))], (&t, &marks, false)));
        assert_eq!(d.edits_of(0)[0], (&[3][..], &[3, 10][..]));
        assert!(!d.update(&[(&t, TableChange::Any)], (&t, &marks, false)));
        assert!(d.update(&[(&t, TableChange::Any)], (&t, &marks, false)));
        let mut shrunk = t.clone();
        shrunk.lines.x0.pop();
        shrunk.lines.y0.pop();
        shrunk.lines.x1.pop();
        shrunk.lines.y1.pop();
        shrunk.lines.layer.pop();
        shrunk.lines.style.pop();
        let none: [Vec<u32>; 6] = Default::default();
        let shrank = (&shrunk, TableChange::Rows(&none));
        assert!(!d.update(&[shrank], (&shrunk, &marks, false)));
    }

    /// A hashed table's edits and a mark recolor are found; an unchanged table reports none;
    /// a different number of tables starts over (unknown).
    #[test]
    fn hashed_tables_and_marks_give_their_edits() {
        let mut t = CanvasPrims::default();
        for i in 0..10 {
            t.rect(i * 100, 0, i * 100 + 50, 10, 4);
        }
        let mut marks = Marks::default();
        marks.push(PrimKind::Rect, 2, 0xff00_00ff);
        let mut d = CanvasDiff::default();
        let other = CanvasPrims::default();
        let same = (&other, TableChange::Same);
        assert!(!d.update(&[(&t, TableChange::Any), same], (&t, &marks, true)));
        t.rects.x0[5] = -1;
        marks.color[0] = 0x00ff_00ff;
        assert!(d.update(&[(&t, TableChange::Any), same], (&t, &marks, true)));
        assert_eq!(d.edits_of(0)[1], (&[5][..], &[5][..]));
        assert_eq!(d.edits_of(1)[1], (&[][..], &[][..]), "unchanged");
        assert_eq!(d.last[2 * TABLES.len()], (vec![0], vec![0]), "the mark");
        let one = [(&t, TableChange::Any)];
        assert!(!d.update(&one, (&t, &marks, false)), "fewer tables");
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
