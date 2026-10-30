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
//!
//! Contents:
//! - `pub struct Rows { lines, rects, arcs, polylines, polygons, texts }`, `Rows::len`,
//!   `Rows::is_empty`
//! - `pub struct PrimIndex`, `PrimIndex::build`, `PrimIndex::apply`, `PrimIndex::follow`,
//!   `PrimIndex::query`

use std::array;
use std::mem;

use crate::damage::RowEdits;
use crate::row::{self, TABLES};
use crate::{CanvasPrims, LAYER_DEAD};

/// Row indices per primitive kind, ascending: a query's answer, reused across queries.
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

/// The grid of one table version. Build it with [`PrimIndex::build`]; it describes that
/// version, plus the later versions [`PrimIndex::apply`] or [`PrimIndex::follow`] told it
/// about (never both).
#[derive(Clone, Debug, Default)]
pub struct PrimIndex {
    grid: Grid,
    /// Per kind: CSR offsets (`cells + 1`) into `items`; empty when no row was gridded.
    start: [Vec<u32>; 6],
    items: [Vec<u32>; 6],
    /// Per kind: rows touching more than [`SPAN_CELLS`] cells, listed by every query.
    wide: [Vec<u32>; 6],
    /// Edit scripts since the build, oldest first, per kind.
    edits: Vec<[RowEdits; 6]>,
    /// Per kind: the rows added since the build that are still there, current indices.
    added: [Vec<u32>; 6],
    /// Per kind: rows appended since the build ([`PrimIndex::follow`]), sorted by cell then
    /// row.
    extra: [Vec<CellRow>; 6],
    /// Per kind: the rows this index has seen (the table's length at the build or the last
    /// follow).
    covered: [u32; 6],
}

/// Square cells over the live rows' extent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Grid {
    /// World origin of cell (0, 0), milli-units.
    x0: i64,
    y0: i64,
    /// The cell edge, milli-units; 0 only before a build.
    cell: i64,
    cols: usize,
    rows: usize,
}

/// One appended row's entry in one cell. Field order is the sort order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct CellRow {
    cell: u32,
    row: u32,
}

/// Cells a row may touch before it is kept in the cell-free list (drawn whenever queried).
const SPAN_CELLS: usize = 16;
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
// ponytail: past 2^16 appended cell entries (thousands of edits) the index is rebuilt, ~7 ms
// per 10^5 rows, on the next recorded tile; a per-cell growable list would remove the cap.
const EXTRA_MAX: usize = 1 << 16;
/// Widenings [`grid`] may take to fit [`CELLS_MAX`]: each grows the cell by an eighth, and
/// the thinnest `i32` extent needs under 64.
const WIDEN_MAX: usize = 256;

const _: () = assert!(CELLS_MAX <= u32::MAX as usize, "cells are u32");
const _: () = assert!(
    CanvasPrims::CAPACITY <= u32::MAX as usize,
    "row indices are u32"
);

impl Rows {
    /// Total rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
            + self.rects.len()
            + self.arcs.len()
            + self.polylines.len()
            + self.polygons.len()
            + self.texts.len()
    }

    /// Whether no row is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl PrimIndex {
    /// Grids the live rows of `t`, about 16 rows per cell over their extent; dead rows are
    /// neither gridded nor widen the extent (their columns hold the deleted objects').
    ///
    /// Cost: O(rows) time, two passes; O(rows) memory.
    ///
    /// # Panics
    ///
    /// If a table holds more than [`CanvasPrims::CAPACITY`] rows, or a chain has fewer than
    /// 2 points.
    #[must_use]
    pub fn build(t: &CanvasPrims) -> PrimIndex {
        let boxes: [Vec<[i32; 4]>; 6] = TABLES.map(|table| {
            (0..row::len(t, table))
                .map(|i| row::read(t, table, i).bounds)
                .collect()
        });
        let covered: [u32; 6] = array::from_fn(|k| {
            assert!(
                boxes[k].len() <= CanvasPrims::CAPACITY,
                "kind {k} over capacity"
            );
            boxes[k].len() as u32
        });
        let alive = |k: usize| {
            boxes[k]
                .iter()
                .zip(row::layers(t, TABLES[k]))
                .enumerate()
                .filter_map(|(i, (b, &layer))| (layer != LAYER_DEAD).then_some((i, *b)))
        };
        // The extent of the live rows.
        let (mut n, mut extent) = (0, [i64::MAX, i64::MAX, i64::MIN, i64::MIN]);
        let live_rows = (0..TABLES.len()).flat_map(alive);
        for (_, b) in live_rows {
            n += 1;
            extent = [
                extent[0].min(i64::from(b[0])),
                extent[1].min(i64::from(b[1])),
                extent[2].max(i64::from(b[2])),
                extent[3].max(i64::from(b[3])),
            ];
        }
        if n == 0 {
            return PrimIndex {
                grid: Grid {
                    cell: 1,
                    cols: 1,
                    rows: 1,
                    ..Grid::default()
                },
                covered,
                ..PrimIndex::default()
            };
        }
        let mut index = PrimIndex {
            grid: grid(extent, n),
            covered,
            ..PrimIndex::default()
        };
        let cells = index.grid.cols * index.grid.rows;
        for k in 0..TABLES.len() {
            // Count, prefix-sum, fill: a CSR list per cell.
            let mut start = vec![0u32; cells + 1];
            for (i, b) in alive(k) {
                if !index.cells(b, |c| start[c + 1] += 1) {
                    index.wide[k].push(i as u32);
                }
            }
            for c in 1..start.len() {
                start[c] += start[c - 1];
            }
            let mut items = vec![0u32; start[cells] as usize];
            let mut at = start.clone();
            for (i, b) in alive(k) {
                index.cells(b, |c| {
                    items[at[c] as usize] = i as u32;
                    at[c] += 1;
                });
            }
            assert_eq!(at[cells - 1], start[cells], "every slot filled");
            debug_assert_eq!(
                at[..cells],
                start[1..],
                "each cell filled to the next's start"
            );
            (index.start[k], index.items[k]) = (start, items);
        }
        index
    }

    /// Follows one edit of the table: `edits[k]` lists, for kind `k` (in [`CanvasPrims`]
    /// order), the rows removed (indices in the old version) and added (in the new version
    /// `t`), as [`CanvasDiff`](crate::CanvasDiff) finds them; every other row kept
    /// its content and order. Returns false, changing nothing, once the index has followed
    /// enough edits that a rebuild is due, or after [`PrimIndex::follow`] (the caller then
    /// rebuilds or stops using it).
    ///
    /// Cost: O(added rows since the build); a query after it costs O(log removed + added)
    /// more per listed row and edit.
    ///
    /// # Panics
    ///
    /// Only on a bug in this index: the asserts check that it follows a bounded number of
    /// edits and that the rows added since the build stay ascending and distinct.
    pub fn apply(&mut self, edits: [(&[u32], &[u32]); 6]) -> bool {
        let added: usize = self.added.iter().map(Vec::len).sum::<usize>()
            + edits.iter().map(|e| e.1.len()).sum::<usize>();
        let followed = self.extra.iter().any(|e| !e.is_empty());
        if self.edits.len() >= EDITS_MAX || added > ADDED_MAX || followed {
            return false;
        }
        let step: [RowEdits; 6] = edits.map(|(removed, added)| {
            let mut edit = RowEdits {
                removed: removed.to_vec(),
                added: added.to_vec(),
            };
            edit.removed.sort_unstable();
            edit.added.sort_unstable();
            edit
        });
        for (rows, edit) in self.added.iter_mut().zip(&step) {
            // Keep the still-present rows at their new index; drop the removed ones.
            rows.retain_mut(|r| match remap(*r, &edit.removed, &edit.added) {
                Some(j) => {
                    *r = j;
                    true
                }
                None => false,
            });
            rows.extend_from_slice(&edit.added);
            rows.sort_unstable();
            // A kept row never lands on an added one (`remap`), so the union is distinct.
            debug_assert!(rows.windows(2).all(|w| w[0] < w[1]), "added rows distinct");
        }
        self.edits.push(step);
        assert!(self.edits.len() <= EDITS_MAX);
        assert!(
            self.added.iter().map(Vec::len).sum::<usize>() <= ADDED_MAX,
            "the rows added since the build stay under the rebuild bound"
        );
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
    ///
    /// # Panics
    ///
    /// If a table holds more than [`CanvasPrims::CAPACITY`] rows.
    pub fn follow(&mut self, t: &CanvasPrims) -> bool {
        let shrank = TABLES
            .into_iter()
            .enumerate()
            .any(|(k, table)| (row::len(t, table) as u32) < self.covered[k]);
        if !self.edits.is_empty() || shrank {
            return false;
        }
        for (k, table) in TABLES.into_iter().enumerate() {
            let (from, n) = (self.covered[k] as usize, row::len(t, table));
            assert!(from <= n, "a table that shrank was refused above");
            assert!(n <= CanvasPrims::CAPACITY, "kind {k} over capacity");
            let mut new: Vec<CellRow> = Vec::new();
            for i in from..n {
                let Some(b) = row::live(t, table, i) else {
                    continue;
                };
                let entry = |c: usize| CellRow {
                    cell: c as u32,
                    row: i as u32,
                };
                if !self.cells(b, |c| new.push(entry(c))) {
                    self.wide[k].push(i as u32);
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
            assert!(
                self.extra[k].len() <= EXTRA_MAX,
                "appended entries under the bound"
            );
        }
        true
    }

    /// Replaces `out` with the rows of `t` (the version this index describes) whose cells
    /// touch world box `bounds`: a superset of the rows drawn inside it, ascending per kind.
    ///
    /// Cost: O(k + n/64) for the k rows listed in those cells out of n (a bitmap pass), or
    /// O(k log k) when k is small next to n; see [`PrimIndex::apply`] for followed edits.
    ///
    /// # Panics
    ///
    /// If `bounds` is inverted (`bounds[2] < bounds[0]` or `bounds[3] < bounds[1]`), or the
    /// index was never built.
    pub fn query(&self, t: &CanvasPrims, bounds: [i32; 4], out: &mut Rows) {
        assert!(bounds[0] <= bounds[2], "inverted box in x");
        assert!(bounds[1] <= bounds[3], "inverted box in y");
        let r = self.range(bounds);
        let cols = self.grid.cols;
        let Rows {
            lines,
            rects,
            arcs,
            polylines,
            polygons,
            texts,
            seen,
        } = out;
        let kinds = [lines, rects, arcs, polylines, polygons, texts];
        for (k, rows) in kinds.into_iter().enumerate() {
            rows.clear();
            rows.extend_from_slice(&self.wide[k]);
            for cy in r[1]..=r[3] {
                let (c0, c1) = (cy * cols + r[0], cy * cols + r[2]);
                if !self.start[k].is_empty() {
                    let (a, z) = (self.start[k][c0], self.start[k][c1 + 1]);
                    rows.extend_from_slice(&self.items[k][a as usize..z as usize]);
                }
                let extra = &self.extra[k];
                let lo = extra.partition_point(|e| e.cell < c0 as u32);
                rows.extend(
                    extra[lo..]
                        .iter()
                        .take_while(|e| e.cell <= c1 as u32)
                        .map(|e| e.row),
                );
            }
            if !self.edits.is_empty() {
                self.through_edits(t, k, bounds, rows);
            }
            ascending(rows, seen);
            debug_assert!(
                rows.last()
                    .is_none_or(|&last| (last as usize) < row::len(t, TABLES[k])),
                "`t` is the version this index describes"
            );
        }
    }
}

// ---- private ----

impl PrimIndex {
    /// Calls `f` with each cell world box `b` touches, unless it touches more than
    /// [`SPAN_CELLS`]: then calls nothing and returns false.
    fn cells(&self, b: [i32; 4], mut f: impl FnMut(usize)) -> bool {
        let r = self.range(b);
        if (r[2] - r[0] + 1) * (r[3] - r[1] + 1) > SPAN_CELLS {
            return false;
        }
        for cy in r[1]..=r[3] {
            for cx in r[0]..=r[2] {
                f(cy * self.grid.cols + cx);
            }
        }
        true
    }

    /// Cells (inclusive `[cx0, cy0, cx1, cy1]`) an ordered world box touches, clamped to the
    /// grid.
    fn range(&self, b: [i32; 4]) -> [usize; 4] {
        let Grid {
            x0,
            y0,
            cell,
            cols,
            rows,
        } = self.grid;
        assert!(cell > 0, "a built index");
        debug_assert!(cols > 0, "a built grid has a column");
        debug_assert!(rows > 0, "a built grid has a row");
        debug_assert!(b[0] <= b[2], "box ordered in x");
        debug_assert!(b[1] <= b[3], "box ordered in y");
        let c = |v: i32, o: i64, n: usize| {
            ((i64::from(v) - o).div_euclid(cell)).clamp(0, n as i64 - 1) as usize
        };
        [
            c(b[0], x0, cols),
            c(b[1], y0, rows),
            c(b[2], x0, cols),
            c(b[3], y0, rows),
        ]
    }

    /// Carries kind `k`'s rows of the build through every edit since (dropping the removed),
    /// then adds the rows added since whose box meets `b`.
    fn through_edits(&self, t: &CanvasPrims, k: usize, b: [i32; 4], rows: &mut Vec<u32>) {
        assert!(!self.edits.is_empty(), "called only after an applied edit");
        rows.retain_mut(|listed| {
            let mut i = *listed;
            for edit in &self.edits {
                match remap(i, &edit[k].removed, &edit[k].added) {
                    Some(j) => i = j,
                    None => return false,
                }
            }
            *listed = i;
            true
        });
        let table = TABLES[k];
        let meets = |q: [i32; 4]| q[0] <= b[2] && q[2] >= b[0] && q[1] <= b[3] && q[3] >= b[1];
        rows.extend(
            self.added[k]
                .iter()
                .filter(|&&i| meets(row::read(t, table, i as usize).bounds)),
        );
        debug_assert!(rows.iter().all(|&i| (i as usize) < row::len(t, table)));
    }
}

/// Square cells, about `n / 16` of them (at most [`CELLS_MAX`]) over the extent
/// `[x0, y0, x1, y1]` (inclusive).
fn grid(extent: [i64; 4], n: usize) -> Grid {
    let (w, h) = (extent[2] - extent[0] + 1, extent[3] - extent[1] + 1);
    assert!(w > 0, "a non-empty extent in x");
    assert!(h > 0, "a non-empty extent in y");
    let cells = (n / 16).clamp(1, CELLS_MAX) as f64;
    let mut cell = (((w as f64) * (h as f64) / cells).sqrt().ceil() as i64).max(1);
    let fit = |cell: i64| {
        (
            ((w + cell - 1) / cell).max(1) as usize,
            ((h + cell - 1) / cell).max(1) as usize,
        )
    };
    // Rounding up both sides may overshoot the cell budget: widen the cells until it holds.
    let mut widenings = 0;
    while fit(cell).0 * fit(cell).1 > CELLS_MAX {
        cell += cell / 8 + 1;
        widenings += 1;
        assert!(widenings <= WIDEN_MAX, "cells widened {widenings} times");
    }
    let (cols, rows) = fit(cell);
    assert!(cols * rows <= CELLS_MAX);
    Grid {
        x0: extent[0],
        y0: extent[1],
        cell,
        cols,
        rows,
    }
}

/// Where kept row `i` (not in `removed`) lands after an edit script: its rank among the kept
/// rows, pushed past each added row at or before it (`removed`, `added` ascending).
fn remap(i: u32, removed: &[u32], added: &[u32]) -> Option<u32> {
    debug_assert!(removed.is_sorted(), "removed rows ascend");
    debug_assert!(added.is_sorted(), "added rows ascend");
    if removed.binary_search(&i).is_ok() {
        return None;
    }
    let mut j = i - removed.partition_point(|&g| g < i) as u32;
    for &p in added {
        if p > j {
            break;
        }
        j += 1;
    }
    debug_assert!(
        added.binary_search(&j).is_err(),
        "kept row {i} landed on added row {j}"
    );
    Some(j)
}

/// Sorts `rows` ascending without duplicates: through the bitmap `seen` (all zero, left so)
/// over their span when they are many next to it (a far zoom's tile lists ~10^5 rows, twice or
/// more each), else a sort.
fn ascending(rows: &mut Vec<u32>, seen: &mut Vec<u64>) {
    debug_assert!(
        seen.iter().all(|&w| w == 0),
        "the bitmap is zero between queries"
    );
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
        let mut m = mem::take(word);
        while m != 0 {
            rows.push(lo + (w * 64) as u32 + m.trailing_zeros());
            m &= m - 1;
        }
    }
    assert!(rows.len() <= k);
    assert!(rows.windows(2).all(|w| w[0] < w[1]));
    debug_assert!(seen.iter().all(|&w| w == 0), "the bitmap is left zero");
}

/// Merges the sorted `new` into the sorted `into` in one backwards pass (no reallocation past
/// one growth).
fn merge(into: &mut Vec<CellRow>, new: &[CellRow]) {
    debug_assert!(into.is_sorted(), "merged into a sorted list");
    debug_assert!(new.is_sorted(), "merged a sorted list");
    let (n, m) = (into.len(), new.len());
    into.resize(n + m, CellRow::default());
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
    // Once `new` is drained, the rest of `into` already sits in place.
    debug_assert_eq!(w, i, "the unmoved prefix is in place");
    debug_assert!(into.is_sorted());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which rows [`assert_covers`] requires a query to list.
    #[derive(Clone, Copy)]
    enum Shown {
        /// Every row, dead or alive.
        All,
        /// Only rows not killed ([`CanvasPrims::kill`]).
        Live,
    }

    /// Whether [`scatter`] draws arcs among its rows.
    #[derive(Clone, Copy)]
    enum WithArcs {
        Yes,
        No,
    }

    /// Oracle: a brute-force box test of every row. Asserts that every `shown` row of `t`
    /// whose box meets `b` is listed, and that each list ascends without duplicates and stays
    /// inside its table.
    fn assert_covers(t: &CanvasPrims, b: [i32; 4], got: &Rows, shown: Shown, at: &str) {
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
            assert!(rows.windows(2).all(|w| w[0] < w[1]), "{at}: ascending");
            assert!(
                rows.iter().all(|&i| (i as usize) < row::len(t, table)),
                "{at}"
            );
            for i in 0..row::len(t, table) {
                let drawn = match shown {
                    Shown::Live => row::live(t, table, i),
                    Shown::All => Some(row::read(t, table, i).bounds),
                };
                if drawn.is_some_and(meets) {
                    let listed = rows.binary_search(&(i as u32)).is_ok();
                    assert!(listed, "{at}: kind {k} row {i}");
                }
            }
        }
    }

    /// A random query box around the `w` × `h` area at the origin, reaching `pad` past it.
    fn query_box(next: &mut impl FnMut(u64) -> u64, w: u64, h: u64, pad: i32) -> [i32; 4] {
        let x = next(w + 2 * pad as u64) as i32 - pad;
        let y = next(h + 2 * pad as u64) as i32 - pad;
        [x, y, x + next(200_000) as i32, y + next(200_000) as i32]
    }

    /// 2000–3000 random lines, rects, arcs and labels over 10^6 × 6·10^5 milli-units.
    fn scatter(next: &mut impl FnMut(u64) -> u64, n: usize, arcs: WithArcs) -> CanvasPrims {
        let mut t = CanvasPrims::default();
        let mut r = |m: u64| next(m) as i32;
        for _ in 0..n {
            let (x, y) = (r(1_000_000), r(600_000));
            let kinds = match arcs {
                WithArcs::Yes => 4,
                WithArcs::No => 3,
            };
            match r(kinds) {
                0 => t.line(x, y, x + r(50_000), y + r(9_000), 1),
                1 => t.rect(x, y, x + r(20_000), y + r(20_000), 4),
                3 => t.arc(x, y, r(10_000), 0, 360_000, 4),
                _ => t.text(x, y, 5_000, 3, "abc"),
            }
        }
        t
    }

    /// Oracle: a query lists every row whose box meets the query box (no row drawn there is
    /// missed), ascending and without duplicates.
    #[test]
    fn queries_cover_every_row_they_should() {
        let mut next = crate::tests::rng(0x9e37_79b9_7f4a_7c15);
        let mut t = scatter(&mut next, 3000, WithArcs::Yes);
        t.line(-5_000_000, 0, 5_000_000, 0, 1); // spans every cell
        let index = PrimIndex::build(&t);
        let mut got = Rows::default();
        for q in 0..200 {
            let b = query_box(&mut next, 1_000_000, 600_000, 50_000);
            index.query(&t, b, &mut got);
            assert_covers(&t, b, &got, Shown::All, &format!("query {q}"));
        }
        let wide = t.lines.x0.len() as u32 - 1;
        assert!(got.lines.contains(&wide), "a wide row is listed");
    }

    /// Oracle: after random edit scripts (rows removed, added and moved, found by the canvas
    /// diff), a query still lists every row of the current table whose box meets the query
    /// box, ascending, and nothing past the table's end.
    #[test]
    fn queries_follow_applied_edits() {
        use crate::{CanvasDiff, Marks, TableChange};
        let mut next = crate::tests::rng(0x9e37_79b9_7f4a_7c15);
        let mut t = scatter(&mut next, 2000, WithArcs::No);
        let mut index = PrimIndex::build(&t);
        let (mut diff, marks) = (CanvasDiff::default(), Marks::default());
        assert!(!diff.update(&[(&t, TableChange::Any)], (&t, &marks, true)));
        let mut got = Rows::default();
        let mut followed = 0;
        for step in 0..40 {
            // Remove a few lines (swap-remove, which moves rows) and add some rows.
            for _ in 0..next(4) {
                let i = next(t.lines.x0.len() as u64) as usize;
                let l = &mut t.lines;
                for c in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
                    c.swap_remove(i);
                }
                l.layer.swap_remove(i);
                l.style.swap_remove(i);
            }
            for _ in 0..next(5) {
                let (x, y) = (next(1_000_000) as i32, next(600_000) as i32);
                if next(2) == 0 {
                    t.line(x, y, x + next(30_000) as i32, y, 1);
                } else {
                    t.rect(x, y, x + next(20_000) as i32, y + next(20_000) as i32, 4);
                }
            }
            let tables = [(&t, TableChange::Any)];
            assert!(diff.update(&tables, (&t, &marks, false)), "step {step}");
            if index.apply(diff.edits_of(0)) {
                followed += 1;
            } else {
                index = PrimIndex::build(&t);
            }
            for q in 0..20 {
                let b = query_box(&mut next, 1_000_000, 600_000, 50_000);
                index.query(&t, b, &mut got);
                assert_covers(&t, b, &got, Shown::All, &format!("step {step} query {q}"));
            }
        }
        assert!(followed >= 20, "{followed} edits followed");
    }

    /// Oracle: an index that follows a table edited by kills and appends (as the editor's
    /// scene edits it) lists, after every edit, every live row whose box meets the query box,
    /// ascending and within the table, without a rebuild; rows outside the built extent
    /// included. Negative space: a table that shrank is refused, and so is an edit script.
    #[test]
    fn queries_follow_killed_and_appended_rows() {
        let mut next = crate::tests::rng(0x2545_f491_4f6c_dd1d);
        let mut t = scatter(&mut next, 2000, WithArcs::No);
        let mut index = PrimIndex::build(&t);
        let mut got = Rows::default();
        for step in 0..60 {
            for _ in 0..next(4) {
                let kind = [0u8, 1, 5][next(3) as usize];
                let n = row::len(&t, TABLES[usize::from(kind)]);
                t.kill(kind, next(n as u64) as usize);
            }
            for _ in 0..next(6) {
                // Some land outside the extent the index was built over.
                let x = next(1_400_000) as i32 - 200_000;
                let y = next(900_000) as i32 - 150_000;
                match next(3) {
                    0 => t.line(x, y, x + next(30_000) as i32, y, 1),
                    1 => t.rect(x, y, x + next(20_000) as i32, y + next(20_000) as i32, 4),
                    _ => t.text(x, y, 5_000, 3, "abcd"),
                }
            }
            assert!(index.follow(&t), "step {step}");
            for q in 0..20 {
                let b = query_box(&mut next, 1_000_000, 600_000, 250_000);
                index.query(&t, b, &mut got);
                assert_covers(&t, b, &got, Shown::Live, &format!("step {step} query {q}"));
            }
        }
        let none: [(&[u32], &[u32]); 6] = Default::default();
        assert!(
            !index.clone().apply(none),
            "a followed index takes no edit script"
        );
        let mut shrunk = t.clone();
        shrunk.lines.x0.pop();
        shrunk.lines.y0.pop();
        shrunk.lines.x1.pop();
        shrunk.lines.y1.pop();
        shrunk.lines.layer.pop();
        shrunk.lines.style.pop();
        let refused = !index.follow(&shrunk);
        assert!(refused, "a shrunk table is not a kill-and-append edit");
    }

    /// Regression (HANDOFF latent bug 4): a killed far-away row neither widens the grid nor
    /// is listed; the grid equals the one built without it.
    #[test]
    fn dead_rows_do_not_widen_the_grid() {
        let mut t = CanvasPrims::default();
        for i in 0..100 {
            t.line(i * 1_000, 0, i * 1_000 + 500, 500, 1);
        }
        let want = PrimIndex::build(&t);
        let far = 2_000_000_000;
        t.line(-far, -far, far, far, 1);
        t.kill(0, 100);
        let index = PrimIndex::build(&t);
        assert_eq!(index.grid, want.grid);
        assert!(index.wide[0].is_empty());
        assert!(!index.items[0].contains(&100));
        let mut all_dead = CanvasPrims::default();
        all_dead.line(0, 0, 10, 10, 1);
        all_dead.kill(0, 0);
        assert_eq!(
            PrimIndex::build(&all_dead).grid.cols,
            1,
            "no live row: one cell"
        );
    }

    /// Oracle: both routes of `ascending` (bitmap for many rows, sort for few) equal a sort
    /// and dedup.
    #[test]
    fn ascending_equals_sort_and_dedup() {
        let mut next = crate::tests::rng(0x2545_f491_4f6c_dd1d);
        for (n, span) in [
            (10, 1000),
            (100, 50),
            (5000, 3000),
            (5000, 1 << 30),
            (70, 64),
        ] {
            let mut rows: Vec<u32> = (0..n).map(|_| 7 + next(span) as u32).collect();
            let mut want = rows.clone();
            want.sort_unstable();
            want.dedup();
            let mut seen = Vec::new();
            ascending(&mut rows, &mut seen);
            assert_eq!(rows, want, "{n} rows over {span}");
            assert!(seen.iter().all(|&w| w == 0), "the bitmap is left zero");
        }
    }

    /// Oracle: `merge` equals concatenating and sorting.
    #[test]
    fn merge_equals_sort() {
        let mut next = crate::tests::rng(0x1234_5678_9abc_def1);
        for _ in 0..50 {
            let mut a: Vec<CellRow> = (0..next(40))
                .map(|_| CellRow {
                    cell: next(9) as u32,
                    row: 0,
                })
                .collect();
            let mut b: Vec<CellRow> = (0..next(40))
                .map(|_| CellRow {
                    cell: next(9) as u32,
                    row: 1,
                })
                .collect();
            a.sort_unstable();
            b.sort_unstable();
            let mut want = [a.clone(), b.clone()].concat();
            want.sort_unstable();
            merge(&mut a, &b);
            assert_eq!(a, want);
        }
    }
}
