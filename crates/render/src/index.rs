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
//! 5. Lives until the table changes (the owner rebuilds it then).
//! 6. Kinds are independent (parallelizable; not needed at ~10 ms per 10^6 rows).

use crate::canvas::CanvasPrims;
use crate::damage::{TABLES, len, row};

/// Rows a row may touch before it is kept in the cell-free list (drawn whenever queried).
const SPAN_CELLS: u64 = 16;
/// Most cells of one grid.
const CELLS_MAX: usize = 1 << 16;

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
/// version only (rows added or moved later are not in it).
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
        if n == 0 {
            return PrimIndex {
                cell: 1,
                cols: 1,
                rows: 1,
                start: Default::default(),
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
            ..PrimIndex::default()
        };
        for (k, boxes) in boxes.iter().enumerate() {
            let mut count = vec![0u32; cols * rows + 1];
            for (i, b) in boxes.iter().enumerate() {
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

    /// Replaces `out` with the rows whose cells touch world box `b` (a superset of the rows
    /// drawn inside it), ascending per kind.
    ///
    /// Cost: O(k + n/64) for the k rows listed in those cells out of n (a bitmap pass), or
    /// O(k log k) when k is small next to n.
    pub fn query(&self, b: [i32; 4], out: &mut Rows) {
        assert!(b[0] <= b[2] && b[1] <= b[3], "inverted box");
        let r = self.range(b);
        let (kinds, seen) = out.kinds();
        for (k, rows) in kinds.into_iter().enumerate() {
            rows.clear();
            rows.extend_from_slice(&self.wide[k]);
            if self.start[k].is_empty() {
                continue;
            }
            for cy in r[1]..=r[3] {
                let c0 = cy * self.cols + r[0];
                let c1 = cy * self.cols + r[2] + 1;
                let (a, z) = (self.start[k][c0] as usize, self.start[k][c1] as usize);
                rows.extend_from_slice(&self.items[k][a..z]);
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
            ix.query(b, &mut got);
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
