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
//! - `pub enum TableChange<'report> { Same, Any, Rows(&'report [Vec<u32>; 6]) }`
//! - `pub(crate) struct RowEdits { removed, added }`

use std::array;
use std::mem;

use crate::row::{self, TABLES, Table};
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
    sequences: Vec<Sequence>,
    /// The sequences replaced by the last call, kept for their allocations.
    spare: Vec<Sequence>,
    /// Per sequence: the rows the last call found removed and added (see
    /// [`CanvasDiff::edits_of`]).
    last: Vec<RowEdits>,
}

/// How one table changed since the previous [`CanvasDiff::update`].
#[derive(Clone, Copy, Debug)]
pub enum TableChange<'report> {
    /// Not at all: neither hashed nor diffed.
    Same,
    /// In any way: every row is hashed and the two versions diffed.
    Any,
    /// Only by its producer killing rows (`killed[kind]`: rows of the previous version, in
    /// [`CanvasPrims::kill`]'s kind order, now on [`LAYER_DEAD`](crate::LAYER_DEAD); text
    /// rows listed there may instead be rewritten in place with the same box, as texts are
    /// drawn from the table every frame) and appending rows past the previous version's end;
    /// every other row is as it was.
    Rows(&'report [Vec<u32>; 6]),
}

/// One kind's edit script: the rows removed (indices before the edit) and added (after),
/// each ascending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowEdits {
    pub(crate) removed: Vec<u32>,
    pub(crate) added: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
struct Sequence {
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
    /// If a reported killed row is past the table, a mark is past its table, or the marks'
    /// columns are out of step.
    pub fn update(
        &mut self,
        tables: &[(&CanvasPrims, TableChange<'_>)],
        marks: (&CanvasPrims, &Marks, bool),
    ) -> bool {
        let (marked, marks, marks_changed) = marks;
        let n = tables.len() * TABLES.len() + 1;
        let first = self.sequences.len() != n;
        if first {
            self.sequences = vec![Sequence::default(); n];
        }
        self.spare.resize_with(n, Sequence::default);
        self.last.resize_with(n, RowEdits::default);
        for edits in &mut self.last {
            edits.removed.clear();
            edits.added.clear();
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
                        known = self.reported(k, row::len(t, table), &killed[kind], known);
                    }
                    TableChange::Same | TableChange::Any => {
                        fill(&mut self.spare[k], t, table);
                        known = self.diffed(k, known);
                    }
                }
            }
        }
        if marks_changed || first {
            fill_marks(&mut self.spare[n - 1], marked, marks);
            known = self.diffed(n - 1, known);
        }
        assert_eq!(self.sequences.len(), n);
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
    #[must_use]
    pub fn edits_of(&self, table: usize) -> [(&[u32], &[u32]); 6] {
        assert!(
            self.last.len() > (table + 1) * TABLES.len(),
            "table {table} is not an input of the last call"
        );
        array::from_fn(|kind| {
            let edits = &self.last[table * TABLES.len() + kind];
            // The other side of the producers' contract (`reported`, `edits`): ascending.
            debug_assert!(edits.removed.is_sorted(), "removed rows ascend");
            debug_assert!(edits.added.is_sorted(), "added rows ascend");
            (&edits.removed[..], &edits.added[..])
        })
    }
}

// ---- private ----

impl CanvasDiff {
    /// Takes a producer's report for sequence `k`, now `now` rows long: the killed rows go
    /// out and in again (their content changed), the appended rows in. Leaves the hashes
    /// stale. Returns whether the edits are still known.
    fn reported(&mut self, k: usize, now: usize, killed: &[u32], known: bool) -> bool {
        assert!(now <= CanvasPrims::CAPACITY, "row indices fit u32");
        let sequence = &mut self.sequences[k];
        let was = sequence.rows;
        (sequence.rows, sequence.hashed) = (now, false);
        let known = known && now >= was;
        if known {
            let RowEdits { removed, added } = &mut self.last[k];
            assert!(
                removed.is_empty(),
                "edits of sequence {k} cleared by `update`"
            );
            assert!(
                added.is_empty(),
                "edits of sequence {k} cleared by `update`"
            );
            // A row appended and killed since the last call is among the appended.
            let killed_before = killed.iter().filter(|&&r| (r as usize) < was);
            for &r in killed_before {
                assert!((r as usize) < now, "rows are killed, never removed");
                removed.push(r);
                added.push(r);
            }
            added.extend(was as u32..now as u32);
            removed.sort_unstable();
            added.sort_unstable();
            // Every removed row comes back (killed in place), so there are at least as many
            // added.
            assert!(removed.len() <= added.len(), "a killed row is also added");
        }
        known
    }

    /// Diffs the freshly hashed `spare[k]` against `sequences[k]` (when still `known`), then
    /// makes it the current version. Returns whether the edits are still known.
    fn diffed(&mut self, k: usize, known: bool) -> bool {
        let (old, new) = (&self.sequences[k], &self.spare[k]);
        assert!(new.hashed, "a diffed sequence is hashed");
        let RowEdits { removed, added } = &mut self.last[k];
        assert!(
            removed.is_empty(),
            "edits of sequence {k} cleared by `update`"
        );
        assert!(
            added.is_empty(),
            "edits of sequence {k} cleared by `update`"
        );
        let known = known && old.hashed && edits(&old.hash, &new.hash, removed, added);
        mem::swap(&mut self.sequences[k], &mut self.spare[k]);
        known
    }
}

/// Refills `sequence` with the hashes of `table`'s rows: one tight loop per kind.
fn fill(sequence: &mut Sequence, t: &CanvasPrims, table: Table) {
    let n = row::len(t, table);
    assert!(n <= CanvasPrims::CAPACITY, "row indices fit u32");
    sequence.hash.clear();
    sequence.hash.reserve(n);
    (sequence.rows, sequence.hashed) = (n, true);
    // Matched outside the loop so each loop inlines one kind's `row::read`.
    let h = &mut sequence.hash;
    match table {
        Table::Lines => h.extend((0..n).map(|i| row::read(t, Table::Lines, i).hash)),
        Table::Rects => h.extend((0..n).map(|i| row::read(t, Table::Rects, i).hash)),
        Table::Arcs => h.extend((0..n).map(|i| row::read(t, Table::Arcs, i).hash)),
        Table::Polylines => h.extend((0..n).map(|i| row::read(t, Table::Polylines, i).hash)),
        Table::Polygons => h.extend((0..n).map(|i| row::read(t, Table::Polygons, i).hash)),
        Table::Texts => h.extend((0..n).map(|i| row::read(t, Table::Texts, i).hash)),
    }
    assert_eq!(sequence.hash.len(), n);
}

/// The marks as rows: what they recolor (the marked row's content, not its index, which
/// moves when other rows are removed) and in which color.
fn fill_marks(sequence: &mut Sequence, prims: &CanvasPrims, marks: &Marks) {
    assert_eq!(marks.index.len(), marks.kind.len(), "index column in step");
    assert_eq!(marks.color.len(), marks.kind.len(), "color column in step");
    sequence.hash.clear();
    (sequence.rows, sequence.hashed) = (marks.kind.len(), true);
    for i in 0..marks.kind.len() {
        let table = match marks.kind[i] {
            PrimKind::Line => Table::Lines,
            PrimKind::Rect => Table::Rects,
            PrimKind::Arc => Table::Arcs,
            PrimKind::Polyline => Table::Polylines,
            PrimKind::Polygon => Table::Polygons,
        };
        let marked = marks.index[i] as usize;
        assert!(
            marked < row::len(prims, table),
            "mark {i} recolors row {marked} past its {table:?} table"
        );
        let hash = row::read(prims, table, marked).hash;
        sequence
            .hash
            .push(row::mix(hash, i64::from(marks.color[i])));
    }
    assert_eq!(sequence.hash.len(), marks.kind.len());
}

/// Pushes the indices of `a` (removed) and `b` (added) outside one longest common
/// subsequence of the two, ascending: Myers' diff after trimming the common prefix and
/// suffix. Returns false, leaving the lists partial, past [`DIFF_EDITS_MAX`] edits or a work
/// budget of a few passes over the rows.
fn edits(a: &[u64], b: &[u64], removed: &mut Vec<u32>, added: &mut Vec<u32>) -> bool {
    assert!(a.len() <= CanvasPrims::CAPACITY, "old row indices fit u32");
    assert!(b.len() <= CanvasPrims::CAPACITY, "new row indices fit u32");
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a1, b1) = (&a[prefix..], &b[prefix..]);
    let suffix = a1
        .iter()
        .rev()
        .zip(b1.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a1, b1) = (&a1[..a1.len() - suffix], &b1[..b1.len() - suffix]);
    let (n, m) = (a1.len() as isize, b1.len() as isize);
    let at = |i: isize| (prefix as isize + i) as u32;
    let d_max = DIFF_EDITS_MAX as isize;
    if n == 0 || m == 0 {
        // Only removals or only additions: no search needed.
        removed.extend((0..n).map(at));
        added.extend((0..m).map(at));
        return n + m <= d_max;
    }
    // v[k + off]: furthest x on diagonal k (x - y = k); one snapshot of v per edit count.
    let off = d_max + 1;
    let mut v = vec![0isize; (2 * off + 1) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut budget = 4 * (n + m) + 4096;
    let mut end = None;
    let down = |v: &[isize], d: isize, k: isize| {
        k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize])
    };
    'search: for d in 0..=d_max {
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
    let Some(d_end) = end else {
        return false;
    };
    assert!(
        trace.len() <= DIFF_EDITS_MAX + 1,
        "one snapshot per edit count"
    );
    // Walk back from (n, m): each step came off diagonal k ± 1 as one addition (y moved)
    // or one removal (x moved), then slid down its snake. The walk runs backwards, so each
    // list comes out descending and is reversed at the end.
    let (removed_from, added_from) = (removed.len(), added.len());
    let (mut x, mut y) = (n, m);
    for d in (1..=d_end).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let down = down(v, d, k);
        let pk = if down { k + 1 } else { k - 1 };
        let px = v[(off + pk) as usize];
        let py = px - pk;
        if down {
            added.push(at(py));
        } else {
            removed.push(at(px));
        }
        (x, y) = (px, py);
    }
    removed[removed_from..].reverse();
    added[added_from..].reverse();
    assert_eq!(
        removed.len() - removed_from + added.len() - added_from,
        d_end as usize
    );
    debug_assert!(removed[removed_from..].is_sorted(), "removed rows ascend");
    debug_assert!(added[added_from..].is_sorted(), "added rows ascend");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: dropping `removed` from `a` and `added` from `b` leaves the same sequence, as long
    /// as a quadratic LCS says it can be; both lists ascend.
    #[test]
    fn edits_are_a_minimal_script() {
        let mut next = crate::tests::rng(0x9e37_79b9_7f4a_7c15);
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
            let (mut removed, mut added) = (Vec::new(), Vec::new());
            assert!(edits(&a, &b, &mut removed, &mut added), "round {round}");
            assert!(removed.windows(2).all(|w| w[0] < w[1]), "round {round}");
            assert!(added.windows(2).all(|w| w[0] < w[1]), "round {round}");
            let kept = |s: &[u64], drop: &[u32]| -> Vec<u64> {
                (0..s.len())
                    .filter(|i| !drop.contains(&(*i as u32)))
                    .map(|i| s[i])
                    .collect()
            };
            let (kept_a, kept_b) = (kept(&a, &removed), kept(&b, &added));
            assert_eq!(kept_a, kept_b, "round {round}: kept rows agree");
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
            assert_eq!(
                kept_a.len(),
                lcs[a.len()][b.len()],
                "round {round}: minimal"
            );
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
        let mark = RowEdits {
            removed: vec![0],
            added: vec![0],
        };
        assert_eq!(d.last[2 * TABLES.len()], mark, "the mark");
        let one = [(&t, TableChange::Any)];
        assert!(!d.update(&one, (&t, &marks, false)), "fewer tables");
    }

    #[test]
    fn a_large_change_gives_up() {
        let a: Vec<u64> = (0..10_000).collect();
        let b: Vec<u64> = (0..10_000).map(|i| i * 7 % 10_000).collect();
        assert!(!edits(&a, &b, &mut Vec::new(), &mut Vec::new()));
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        assert!(edits(&a, &a, &mut removed, &mut added));
        assert!(removed.is_empty());
        assert!(added.is_empty());
    }
}
