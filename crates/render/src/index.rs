//! A world-space grid over a canvas's primitive tables, so drawing one tile or patch of a large
//! canvas looks at the rows near it instead of culling every row (D39: a tile costs what it
//! shows).
//!
//! Six questions:
//! 1. In: one [`CanvasPrims`] version. Out: per grid cell and kind, the rows whose world box
//!    touches it; a query returns a box's rows sorted (drawing order kept).
//! 2. 10^5–10^7 rows; about 16 rows per cell, at most 2^16 cells.
//! 3. Row indices `u32` (tables hold < 2^22 rows per kind); cells `u32` CSR offsets.
//! 4. Built in two linear passes (count, fill); queried per tile or patch.
//! 5. Lives until the table changes in a way it cannot follow: edit scripts
//!    ([`PrimIndex::apply`]) a bounded number of times, kills and appends
//!    ([`PrimIndex::follow`], dead rows draw nothing) until a bounded number of rows was
//!    appended; the owner rebuilds it then.
//! 6. Kinds are independent (parallelizable; not needed at ~10 ms per 10^6 rows).

use crate::canvas::CanvasPrims;
use crate::damage::{TABLES, len, row};

/// Rows a row may touch before it is kept in the cell-free list (drawn whenever queried).
const SPAN_CELLS: u64 = 16;
/// Most cells of one grid.
const CELLS_MAX: usize = 1 << 16;
/// Edit scripts an index follows (see [`PrimIndex::apply`]) before it asks to be rebuilt:
/// each one costs a query a binary search and a pass over its additions per row.
const EDITS_MAX: usize = 16;
/// Rows added since the build an index keeps before it asks to be rebuilt (each query tests
/// every one).
const ADDED_MAX: usize = 4096;
/// Cell entries per kind an index that follows appended rows ([`PrimIndex::follow`]) keeps
/// beside its build before it asks to be rebuilt (each costs a query nothing unless its cell
/// is queried; the bound keeps a follow's merge pass small).
const EXTRA_MAX: usize = 1 << 16;

/// Row indices per primitive kind, ascending.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    /// Rows of [`CanvasPrims::lines`].
    pub lines: Vec<u32>,
    /// Rows of [`CanvasPrims::rects`].
    pub rects: Vec<u32>,
    /// Rows of [`CanvasPrims::arcs`].
    pub arcs: Vec<u32>,
    /// Rows of [`CanvasPrims::polylines`].
    pub polylines: Vec<u32>,
    /// Rows of [`CanvasPrims::polygons`].
    pub polygons: Vec<u32>,
    /// Rows of [`CanvasPrims::texts`].
    pub texts: Vec<u32>,
    /// A query's bitmap of listed rows, all zero between queries.
    seen: Vec<u64>,
}

impl Rows {
    /// The kinds in [`CanvasPrims`] order, and the bitmap.
    fn kinds(&mut self) -> ([&mut Vec<u32>; 6], &mut Vec<u64>) {
        (
            [
                &mut self.lines,
                &mut self.rects,
                &mut self.arcs,
                &mut self.polylines,
                &mut self.polygons,
                &mut self.texts,
            ],
            &mut self.seen,
        )
    }

    /// Total rows.
    pub fn len(&self) -> usize {
        self.lines.len()
            + self.rects.len()
            + self.arcs.len()
            + self.polylines.len()
            + self.polygons.len()
            + self.texts.len()
    }

    /// Whether no row is listed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The grid of one table version. Build it with [`PrimIndex::build`]; it describes that
/// version, plus the later versions [`PrimIndex::apply`] or [`PrimIndex::follow`] told it
/// about (never both).
#[derive(Clone, Debug, Default)]
pub struct PrimIndex {
    /// World origin of cell (0, 0) and the cell edge, milli-units.
    x0: i64,
    y0: i64,
    cell: i64,
    cols: usize,
    rows: usize,
    /// Per kind: CSR offsets (`cells + 1`) into `items`.
    start: [Vec<u32>; 6],
    items: [Vec<u32>; 6],
    /// Per kind: rows touching more than [`SPAN_CELLS`] cells.
    wide: [Vec<u32>; 6],
    /// Edit scripts since the build, oldest first: per kind the rows removed (indices before
    /// the edit) and added (after), ascending.
    edits: Vec<[(Vec<u32>, Vec<u32>); 6]>,
    /// Per kind: the rows added since the build that are still there, current indices.
    added: [Vec<u32>; 6],
    /// Per kind: rows appended since the build ([`PrimIndex::follow`]) as `(cell, row)`,
    /// sorted.
    extra: [Vec<(u32, u32)>; 6],
    /// Per kind: the rows this index has seen (the table's length at the build or the last
    /// follow).
    covered: [u32; 6],
}

/// Where kept row `i` (not in `gone`) lands after an edit script: its rank among the kept
/// rows, pushed past each added row at or before it (`gone`, `new` ascending).
fn remap(i: u32, gone: &[u32], new: &[u32]) -> Option<u32> {
    if gone.binary_search(&i).is_ok() {
        return None;
    }
    let mut j = i - gone.partition_point(|&g| g < i) as u32;
    for &p in new {
        if p > j {
            break;
        }
        j += 1;
    }
    Some(j)
}

impl PrimIndex {
    /// Grids the rows of `t`, about 16 rows per cell.
    ///
    /// Cost: O(rows) time, two passes; O(rows) memory.
    pub fn build(t: &CanvasPrims) -> PrimIndex {
        let n = t.len();
        let mut ext = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        let mut boxes: [Vec<[i32; 4]>; 6] = Default::default();
        for (k, table) in TABLES.into_iter().enumerate() {
            boxes[k] = (0..len(t, table)).map(|i| row(t, table, i).1).collect();
            for b in &boxes[k] {
                ext = [
                    ext[0].min(i64::from(b[0])),
                    ext[1].min(i64::from(b[1])),
                    ext[2].max(i64::from(b[2])),
                    ext[3].max(i64::from(b[3])),
                ];
            }
        }
        let covered = std::array::from_fn(|k| boxes[k].len() as u32);
        if n == 0 {
            return PrimIndex {
                cell: 1,
                cols: 1,
                rows: 1,
                start: Default::default(),
                covered,
                ..PrimIndex::default()
            };
        }
        let (w, h) = (ext[2] - ext[0] + 1, ext[3] - ext[1] + 1);
        let cells = (n / 16).clamp(1, CELLS_MAX) as f64;
        // Square cells, as many as `cells` over the extent.
        let mut cell = (((w as f64) * (h as f64) / cells).sqrt().ceil() as i64).max(1);
        let grid = |cell: i64| {
            (
                ((w + cell - 1) / cell).max(1) as usize,
                ((h + cell - 1) / cell).max(1) as usize,
            )
        };
        // Rounding up both sides may overshoot the cell budget: widen the cells until it holds.
        while grid(cell).0 * grid(cell).1 > CELLS_MAX {
            cell += cell / 8 + 1;
        }
        let (cols, rows) = grid(cell);
        let mut ix = PrimIndex {
            x0: ext[0],
            y0: ext[1],
            cell,
            cols,
            rows,
            covered,
            ..PrimIndex::default()
        };
        for (k, boxes) in boxes.iter().enumerate() {
            let mut count = vec![0u32; cols * rows + 1];
            let dead = |i: usize| is_dead(t, TABLES[k], i);
            for (i, b) in boxes.iter().enumerate() {
                if dead(i) {
                    continue;
                }
                let r = ix.range(*b);
                if span(r) > SPAN_CELLS {
                    ix.wide[k].push(i as u32);
                    continue;
                }
                for cy in r[1]..=r[3] {
                    for cx in r[0]..=r[2] {
                        count[cy * cols + cx + 1] += 1;
                    }
                }
            }
            for c in 1..count.len() {
                count[c] += count[c - 1];
            }
            let mut items = vec![0u32; count[cols * rows] as usize];
            let mut at = count.clone();
            for (i, b) in boxes.iter().enumerate() {
                if dead(i) {
                    continue;
                }
                let r = ix.range(*b);
                if span(r) > SPAN_CELLS {
                    continue;
                }
                for cy in r[1]..=r[3] {
                    for cx in r[0]..=r[2] {
                        let c = cy * cols + cx;
                        items[at[c] as usize] = i as u32;
                        at[c] += 1;
                    }
                }
            }
            ix.start[k] = count;
            ix.items[k] = items;
        }
        assert!(ix.cols * ix.rows <= CELLS_MAX);
        ix
    }

    /// Cells (inclusive `[cx0, cy0, cx1, cy1]`) a world box touches, clamped to the grid.
    fn range(&self, b: [i32; 4]) -> [usize; 4] {
        let c = |v: i32, o: i64, n: usize| {
            ((i64::from(v) - o).div_euclid(self.cell)).clamp(0, n as i64 - 1) as usize
        };
        [
            c(b[0], self.x0, self.cols),
            c(b[1], self.y0, self.rows),
            c(b[2], self.x0, self.cols),
            c(b[3], self.y0, self.rows),
        ]
    }

    /// Follows one edit of the table: `edits[k]` lists, for kind `k` (in [`CanvasPrims`]
    /// order), the rows removed (indices in the old version) and added (in the new version
    /// `t`), as [`CanvasDiff`](crate::canvas::CanvasDiff) finds them; every other row kept
    /// its content and order. Returns false, changing nothing, once the index has followed
    /// enough edits that a rebuild is due (the caller then rebuilds or stops using it).
    ///
    /// Cost: O(added rows since the build); a query after it costs O(log removed + added)
    /// more per listed row and edit.
    pub fn apply(&mut self, edits: [(&[u32], &[u32]); 6]) -> bool {
        let added: usize = self.added.iter().map(Vec::len).sum::<usize>()
            + edits.iter().map(|e| e.1.len()).sum::<usize>();
        let followed = self.extra.iter().any(|e| !e.is_empty());
        if self.edits.len() >= EDITS_MAX || added > ADDED_MAX || followed {
            return false;
        }
        let mut step: [(Vec<u32>, Vec<u32>); 6] = Default::default();
        for (k, (gone, new)) in edits.into_iter().enumerate() {
            let (mut g, mut n) = (gone.to_vec(), new.to_vec());
            g.sort_unstable();
            n.sort_unstable();
            let a = &mut self.added[k];
            a.retain_mut(|r| match remap(*r, &g, &n) {
                Some(j) => {
                    *r = j;
                    true
                }
                None => false,
            });
            a.extend_from_slice(&n);
            a.sort_unstable();
            step[k] = (g, n);
        }
        self.edits.push(step);
        assert!(self.edits.len() <= EDITS_MAX);
        true
    }

    /// Follows a table its producer edits only by killing rows
    /// ([`CanvasPrims::kill`]) and appending rows: the rows past the ones this index has seen
    /// are gridded in (killed rows stay listed and draw nothing), so the index stays current
    /// for as long as the table is edited that way. Returns false when it cannot follow (it
    /// followed edit scripts through [`PrimIndex::apply`], the table shrank, or more than a
    /// bounded number of rows were appended since the build): the caller then rebuilds it.
    ///
    /// Cost: O(appended rows · log) plus one merge pass over the rows appended earlier.
    pub fn follow(&mut self, t: &CanvasPrims) -> bool {
        let shrank = TABLES
            .into_iter()
            .enumerate()
            .any(|(k, tb)| (len(t, tb) as u32) < self.covered[k]);
        if !self.edits.is_empty() || shrank {
            return false;
        }
        for (k, table) in TABLES.into_iter().enumerate() {
            let (from, n) = (self.covered[k] as usize, len(t, table));
            let mut new: Vec<(u32, u32)> = Vec::new();
            for i in from..n {
                let Some(b) = live(t, table, i) else { continue };
                let r = self.range(b);
                if span(r) > SPAN_CELLS {
                    self.wide[k].push(i as u32);
                    continue;
                }
                for cy in r[1]..=r[3] {
                    for cx in r[0]..=r[2] {
                        new.push(((cy * self.cols + cx) as u32, i as u32));
                    }
                }
            }
            self.covered[k] = n as u32;
            if new.is_empty() {
                continue;
            }
            if self.extra[k].len() + new.len() > EXTRA_MAX {
                return false;
            }
            new.sort_unstable();
            merge(&mut self.extra[k], &new);
        }
        true
    }

    /// Replaces `out` with the rows of `t` (the version this index describes) whose cells
    /// touch world box `b`: a superset of the rows drawn inside it, ascending per kind.
    ///
    /// Cost: O(k + n/64) for the k rows listed in those cells out of n (a bitmap pass), or
    /// O(k log k) when k is small next to n; see [`PrimIndex::apply`] for followed edits.
    pub fn query(&self, t: &CanvasPrims, b: [i32; 4], out: &mut Rows) {
        assert!(b[0] <= b[2] && b[1] <= b[3], "inverted box");
        let r = self.range(b);
        let (kinds, seen) = out.kinds();
        for (k, rows) in kinds.into_iter().enumerate() {
            rows.clear();
            rows.extend_from_slice(&self.wide[k]);
            if !self.start[k].is_empty() {
                for cy in r[1]..=r[3] {
                    let c0 = cy * self.cols + r[0];
                    let c1 = cy * self.cols + r[2] + 1;
                    let (a, z) = (self.start[k][c0] as usize, self.start[k][c1] as usize);
                    rows.extend_from_slice(&self.items[k][a..z]);
                }
            }
            let extra = &self.extra[k];
            if !extra.is_empty() {
                for cy in r[1]..=r[3] {
                    let (c0, c1) = (
                        (cy * self.cols + r[0]) as u32,
                        (cy * self.cols + r[2]) as u32,
                    );
                    let lo = extra.partition_point(|e| e.0 < c0);
                    rows.extend(extra[lo..].iter().take_while(|e| e.0 <= c1).map(|e| e.1));
                }
            }
            if !self.edits.is_empty() {
                // Rows of the build through every edit since (dropped when removed), then
                // the rows added since that meet `b`.
                rows.retain_mut(|row| {
                    let mut i = *row;
                    for e in &self.edits {
                        match remap(i, &e[k].0, &e[k].1) {
                            Some(j) => i = j,
                            None => return false,
                        }
                    }
                    *row = i;
                    true
                });
                let meets =
                    |q: [i32; 4]| q[0] <= b[2] && q[2] >= b[0] && q[1] <= b[3] && q[3] >= b[1];
                let table = TABLES[k];
                rows.extend(
                    self.added[k]
                        .iter()
                        .filter(|&&i| meets(row(t, table, i as usize).1)),
                );
                debug_assert!(rows.iter().all(|&i| (i as usize) < len(t, table)));
            }
            ascending(rows, seen);
        }
    }
}

/// Sorts `rows` ascending without duplicates: through the bitmap `seen` (all zero, left so)
/// over their span when they are many next to it (a far zoom's tile lists ~10^5 rows, twice or
/// more each), else a sort.
fn ascending(rows: &mut Vec<u32>, seen: &mut Vec<u64>) {
    let (lo, hi) = rows
        .iter()
        .fold((u32::MAX, 0), |(lo, hi), &r| (lo.min(r), hi.max(r)));
    let words = ((hi.saturating_sub(lo)) / 64 + 1) as usize;
    if rows.len() < 64 || words > rows.len() * 4 {
        rows.sort_unstable();
        rows.dedup();
        return;
    }
    let k = rows.len();
    if seen.len() < words {
        seen.resize(words, 0);
    }
    for &r in rows.iter() {
        let i = (r - lo) as usize;
        seen[i / 64] |= 1 << (i % 64);
    }
    rows.clear();
    for (w, word) in seen[..words].iter_mut().enumerate() {
        let mut m = std::mem::take(word);
        while m != 0 {
            rows.push(lo + (w * 64) as u32 + m.trailing_zeros());
            m &= m - 1;
        }
    }
    assert!(rows.len() <= k && rows.windows(2).all(|w| w[0] < w[1]));
}

/// Row `i`'s world box, or `None` when the row is dead.
fn live(t: &CanvasPrims, table: crate::damage::Table, i: usize) -> Option<[i32; 4]> {
    (!is_dead(t, table, i)).then(|| row(t, table, i).1)
}

/// Whether row `i` was killed ([`CanvasPrims::kill`]).
fn is_dead(t: &CanvasPrims, table: crate::damage::Table, i: usize) -> bool {
    use crate::damage::Table;
    let layer = match table {
        Table::Lines => t.lines.layer[i],
        Table::Rects => t.rects.layer[i],
        Table::Arcs => t.arcs.layer[i],
        Table::Polylines => t.polylines.layer[i],
        Table::Polygons => t.polygons.layer[i],
        Table::Texts => t.texts.layer[i],
    };
    layer == crate::canvas::LAYER_DEAD
}

/// Merges the sorted `new` into the sorted `into` in one backwards pass (no reallocation past
/// one growth).
fn merge(into: &mut Vec<(u32, u32)>, new: &[(u32, u32)]) {
    let (n, m) = (into.len(), new.len());
    into.resize(n + m, (0, 0));
    let (mut i, mut j, mut w) = (n, m, n + m);
    while j > 0 {
        if i > 0 && into[i - 1] > new[j - 1] {
            into[w - 1] = into[i - 1];
            i -= 1;
        } else {
            into[w - 1] = new[j - 1];
            j -= 1;
        }
        w -= 1;
    }
    debug_assert!(into.windows(2).all(|p| p[0] <= p[1]));
}

fn span(r: [usize; 4]) -> u64 {
    ((r[2] - r[0] + 1) * (r[3] - r[1] + 1)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: a query lists every row whose box meets the query box (no row drawn there is
    /// missed), ascending and without duplicates.
    #[test]
    fn queries_cover_every_row_they_should() {
        let mut t = CanvasPrims::default();
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |m: i32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % m as u64) as i32
        };
        for _ in 0..3000 {
            let (x, y) = (next(1_000_000), next(600_000));
            match next(4) {
                0 => t.line(x, y, x + next(50_000), y + next(9_000), 1),
                1 => t.rect(x, y, x + next(20_000), y + next(20_000), 4),
                2 => t.arc(x, y, next(10_000), 0, 360_000, 4),
                _ => t.text(x, y, 5_000, 3, "abc"),
            }
        }
        t.line(-5_000_000, 0, 5_000_000, 0, 1); // spans every cell
        let ix = PrimIndex::build(&t);
        let mut got = Rows::default();
        for _ in 0..200 {
            let (x, y) = (next(1_100_000) - 50_000, next(700_000) - 50_000);
            let b = [x, y, x + next(200_000), y + next(200_000)];
            ix.query(&t, b, &mut got);
            let meets = |r: [i32; 4]| r[0] <= b[2] && r[2] >= b[0] && r[1] <= b[3] && r[3] >= b[1];
            let lists = [
                &got.lines,
                &got.rects,
                &got.arcs,
                &got.polylines,
                &got.polygons,
                &got.texts,
            ];
            for (k, table) in TABLES.into_iter().enumerate() {
                let rows = lists[k];
                assert!(
                    rows.windows(2).all(|w| w[0] < w[1]),
                    "ascending, no duplicate"
                );
                for i in 0..len(&t, table) {
                    if meets(row(&t, table, i).1) {
                        assert!(rows.binary_search(&(i as u32)).is_ok(), "kind {k} row {i}");
                    }
                }
            }
        }
        assert!(
            got.lines.contains(&(t.lines.x0.len() as u32 - 1)),
            "a wide row is listed"
        );
    }

    /// Oracle: after random edit scripts (rows removed, added and moved, found by the canvas
    /// diff), a query still lists every row of the current table whose box meets the query
    /// box, ascending, and nothing past the table's end.
    #[test]
    fn queries_follow_applied_edits() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |m: i32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % m as u64) as i32
        };
        let mut t = CanvasPrims::default();
        for _ in 0..2000 {
            let (x, y) = (next(1_000_000), next(600_000));
            match next(3) {
                0 => t.line(x, y, x + next(30_000), y + next(9_000), 1),
                1 => t.rect(x, y, x + next(20_000), y + next(20_000), 4),
                _ => t.text(x, y, 5_000, 3, "abc"),
            }
        }
        let mut ix = PrimIndex::build(&t);
        let mut diff = crate::canvas::CanvasDiff::default();
        let mut boxes = Vec::new();
        assert!(!diff.update(
            &[(&t, crate::canvas::TableChange::Any)],
            (&t, &crate::canvas::Marks::default(), true),
            &mut boxes
        ));
        let mut got = Rows::default();
        let mut followed = 0;
        for step in 0..40 {
            // Remove a few lines (swap-remove, as the scene does) and add some rows.
            for _ in 0..next(4) {
                let n = t.lines.x0.len();
                let i = next(n as i32) as usize;
                for c in [
                    &mut t.lines.x0,
                    &mut t.lines.y0,
                    &mut t.lines.x1,
                    &mut t.lines.y1,
                ] {
                    c.swap_remove(i);
                }
                t.lines.layer.swap_remove(i);
                t.lines.style.swap_remove(i);
            }
            for _ in 0..next(5) {
                let (x, y) = (next(1_000_000), next(600_000));
                if next(2) == 0 {
                    t.line(x, y, x + next(30_000), y, 1);
                } else {
                    t.rect(x, y, x + next(20_000), y + next(20_000), 4);
                }
            }
            boxes.clear();
            let marks = crate::canvas::Marks::default();
            assert!(
                diff.update(
                    &[(&t, crate::canvas::TableChange::Any)],
                    (&t, &marks, false),
                    &mut boxes
                ),
                "step {step}"
            );
            let e = diff.edits_of(0);
            if !ix.apply(e) {
                ix = PrimIndex::build(&t);
            } else {
                followed += 1;
            }
            for _ in 0..20 {
                let (x, y) = (next(1_100_000) - 50_000, next(700_000) - 50_000);
                let b = [x, y, x + next(200_000), y + next(200_000)];
                ix.query(&t, b, &mut got);
                let meets =
                    |r: [i32; 4]| r[0] <= b[2] && r[2] >= b[0] && r[1] <= b[3] && r[3] >= b[1];
                let lists = [
                    &got.lines,
                    &got.rects,
                    &got.arcs,
                    &got.polylines,
                    &got.polygons,
                    &got.texts,
                ];
                for (k, table) in TABLES.into_iter().enumerate() {
                    let rows = lists[k];
                    assert!(rows.windows(2).all(|w| w[0] < w[1]), "ascending");
                    assert!(
                        rows.iter().all(|&i| (i as usize) < len(&t, table)),
                        "in the table"
                    );
                    for i in 0..len(&t, table) {
                        if meets(row(&t, table, i).1) {
                            assert!(
                                rows.binary_search(&(i as u32)).is_ok(),
                                "step {step} kind {k} row {i}"
                            );
                        }
                    }
                }
            }
        }
        assert!(followed >= 20, "{followed} edits followed");
    }

    /// Oracle: an index that follows a table edited by kills and appends (as the editor's
    /// scene edits it) lists, after every edit, every live row whose box meets the query box,
    /// ascending and within the table, without a rebuild; rows outside the built extent
    /// included. Negative space: a table that shrank is refused.
    #[test]
    fn queries_follow_killed_and_appended_rows() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |m: i32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % m as u64) as i32
        };
        let mut t = CanvasPrims::default();
        for _ in 0..2000 {
            let (x, y) = (next(1_000_000), next(600_000));
            match next(3) {
                0 => t.line(x, y, x + next(30_000), y + next(9_000), 1),
                1 => t.rect(x, y, x + next(20_000), y + next(20_000), 4),
                _ => t.text(x, y, 5_000, 3, "abc"),
            }
        }
        let mut ix = PrimIndex::build(&t);
        let mut got = Rows::default();
        for step in 0..60 {
            for _ in 0..next(4) {
                let kind = [0u8, 1, 5][next(3) as usize];
                let n = len(&t, TABLES[usize::from(kind)]);
                t.kill(kind, next(n as i32) as usize);
            }
            for _ in 0..next(6) {
                // Some land outside the extent the index was built over.
                let (x, y) = (next(1_400_000) - 200_000, next(900_000) - 150_000);
                match next(3) {
                    0 => t.line(x, y, x + next(30_000), y, 1),
                    1 => t.rect(x, y, x + next(20_000), y + next(20_000), 4),
                    _ => t.text(x, y, 5_000, 3, "abcd"),
                }
            }
            assert!(ix.follow(&t), "step {step}");
            for _ in 0..20 {
                let (x, y) = (next(1_500_000) - 250_000, next(1_000_000) - 200_000);
                let b = [x, y, x + next(200_000), y + next(200_000)];
                ix.query(&t, b, &mut got);
                let meets =
                    |r: [i32; 4]| r[0] <= b[2] && r[2] >= b[0] && r[1] <= b[3] && r[3] >= b[1];
                let lists = [
                    &got.lines,
                    &got.rects,
                    &got.arcs,
                    &got.polylines,
                    &got.polygons,
                    &got.texts,
                ];
                for (k, table) in TABLES.into_iter().enumerate() {
                    let rows = lists[k];
                    assert!(rows.windows(2).all(|w| w[0] < w[1]), "ascending");
                    assert!(rows.iter().all(|&i| (i as usize) < len(&t, table)));
                    for i in 0..len(&t, table) {
                        if live(&t, table, i).is_some_and(meets) {
                            assert!(
                                rows.binary_search(&(i as u32)).is_ok(),
                                "step {step} kind {k} row {i}"
                            );
                        }
                    }
                }
            }
        }
        let mut shrunk = t.clone();
        shrunk.lines.x0.pop();
        shrunk.lines.y0.pop();
        shrunk.lines.x1.pop();
        shrunk.lines.y1.pop();
        shrunk.lines.layer.pop();
        shrunk.lines.style.pop();
        assert!(
            !ix.follow(&shrunk),
            "a shrunk table is not a kill-and-append edit"
        );
    }

    /// Oracle: both routes of `ascending` (bitmap for many rows, sort for few) equal a sort
    /// and dedup.
    #[test]
    fn ascending_equals_sort_and_dedup() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        for (n, span) in [
            (10, 1000),
            (100, 50),
            (5000, 3000),
            (5000, 1 << 30),
            (70, 64),
        ] {
            let mut rows: Vec<u32> = (0..n)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    7 + (rng % span) as u32
                })
                .collect();
            let mut want = rows.clone();
            want.sort_unstable();
            want.dedup();
            let mut seen = Vec::new();
            ascending(&mut rows, &mut seen);
            assert_eq!(rows, want, "{n} rows over {span}");
            assert!(seen.iter().all(|&w| w == 0), "the bitmap is left zero");
        }
    }
}
