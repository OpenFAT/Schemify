//! One row of a [`CanvasPrims`] table seen uniformly: the row's hash (everything drawn about
//! it) and its world box. What [`CanvasDiff`](crate::CanvasDiff) diffs and
//! [`PrimIndex`](crate::PrimIndex) grids. Crate-private; no spec clause (the hash is ours and
//! never leaves the process).
//!
//! Contents:
//! - `pub(crate) enum Table`, `pub(crate) const TABLES: [Table; 6]`
//! - `pub(crate) fn len(t, table) -> usize`, `layers(t, table) -> &[u8]`,
//!   `layers_mut(t, table) -> &mut Vec<u8>`, `row(t, table, i) -> (u64, [i32; 4])`,
//!   `live(t, table, i) -> Option<[i32; 4]>`, `mix(h, v) -> u64`

use crate::{CanvasPrims, LAYER_DEAD, LineStyle, chain};

/// The six tables of [`CanvasPrims`], in its field order (the `kind` of
/// [`CanvasPrims::kill`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Table {
    Lines,
    Rects,
    Arcs,
    Polylines,
    Polygons,
    Texts,
}

/// Every table, indexed by kind.
pub(crate) const TABLES: [Table; 6] = [
    Table::Lines,
    Table::Rects,
    Table::Arcs,
    Table::Polylines,
    Table::Polygons,
    Table::Texts,
];

/// Rows of `table`, dead included.
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

/// The layer column of `table`.
pub(crate) fn layers(t: &CanvasPrims, table: Table) -> &[u8] {
    match table {
        Table::Lines => &t.lines.layer,
        Table::Rects => &t.rects.layer,
        Table::Arcs => &t.arcs.layer,
        Table::Polylines => &t.polylines.layer,
        Table::Polygons => &t.polygons.layer,
        Table::Texts => &t.texts.layer,
    }
}

/// The layer column of `table`, to kill rows in.
pub(crate) fn layers_mut(t: &mut CanvasPrims, table: Table) -> &mut Vec<u8> {
    match table {
        Table::Lines => &mut t.lines.layer,
        Table::Rects => &mut t.rects.layer,
        Table::Arcs => &mut t.arcs.layer,
        Table::Polylines => &mut t.polylines.layer,
        Table::Polygons => &mut t.polygons.layer,
        Table::Texts => &mut t.texts.layer,
    }
}

/// Row `i` of `table`: its hash (everything drawn about it) and its world box
/// `[x0, y0, x1, y1]`, `x0 <= x1` and `y0 <= y1`.
#[inline]
pub(crate) fn row(t: &CanvasPrims, table: Table, i: usize) -> (u64, [i32; 4]) {
    let span =
        |x0: i32, y0: i32, x1: i32, y1: i32| [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    match table {
        Table::Lines | Table::Rects => {
            let (seed, l) = match table {
                Table::Lines => (1, &t.lines),
                _ => (2, &t.rects),
            };
            let f = [
                l.x0[i],
                l.y0[i],
                l.x1[i],
                l.y1[i],
                pen(l.layer[i], l.style[i]),
            ];
            (fold(seed, &f), span(f[0], f[1], f[2], f[3]))
        }
        Table::Arcs => {
            let a = &t.arcs;
            let (cx, cy, r) = (a.cx[i], a.cy[i], a.r[i]);
            let f = [
                cx,
                cy,
                r,
                a.start[i],
                a.sweep[i],
                pen(a.layer[i], a.style[i]),
            ];
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
            let pts = chain(&p.end, i);
            let (xs, ys) = (&p.x[pts.clone()], &p.y[pts]);
            assert!(xs.len() >= 2, "a chain has 2 points");
            let h = xs
                .iter()
                .zip(ys)
                .fold(fold(seed, &[pen(p.layer[i], p.style[i])]), |h, (&x, &y)| {
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
        Table::Texts => text_row(t, i),
    }
}

/// Row `i`'s world box, or `None` when the row was killed ([`CanvasPrims::kill`]).
pub(crate) fn live(t: &CanvasPrims, table: Table, i: usize) -> Option<[i32; 4]> {
    (layers(t, table)[i] != LAYER_DEAD).then(|| row(t, table, i).1)
}

/// Folds one field into a row hash (deterministic on every target).
pub(crate) fn mix(h: u64, v: i64) -> u64 {
    (h.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95)
}

// ---- private ----

/// A row's layer and stroke style as one hashed field.
#[inline]
fn pen(layer: u8, style: LineStyle) -> i32 {
    i32::from(layer) | (i32::from(style.0) << 8)
}

fn fold(seed: i64, fields: &[i32]) -> u64 {
    fields
        .iter()
        .fold(mix(0, seed), |h, &v| mix(h, i64::from(v)))
}

/// Label `i`: hashed with its bytes; boxed by how far it can reach, as the renderer never
/// measures glyphs here.
#[inline]
fn text_row(t: &CanvasPrims, i: usize) -> (u64, [i32; 4]) {
    let tx = &t.texts;
    let bytes = &tx.text.as_bytes()[chain(&tx.end, i)];
    // ponytail: a table filled column by column may lack `rot`; it reads as unturned.
    let rot = tx.rot.get(i).copied().unwrap_or(0);
    let f = [
        tx.x[i],
        tx.y[i],
        tx.size[i],
        i32::from(tx.layer[i]),
        i32::from(rot),
    ];
    let h = bytes.iter().fold(fold(6, &f), |h, &b| mix(h, i64::from(b)));
    // Labels hang right and down from their anchor: at most an em per byte across and two
    // lines down (what `draw_texts` treats as visible), padded by an em; a turned label reaches
    // as far in any direction.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: hand-computed boxes per kind; a row's hash changes with anything drawn about it
    /// (negative space: an identical row hashes the same).
    #[test]
    fn rows_box_and_hash_what_is_drawn() {
        let mut t = CanvasPrims::default();
        t.line(10, 20, -5, 3, 1);
        t.line(10, 20, -5, 3, 1);
        t.line(10, 20, -5, 3, 2);
        t.rect(10, 20, -5, 3, 1);
        t.arc(100, 100, 30, 0, 90_000, 4);
        t.poly(&[(0, 5), (7, -2), (3, 9)], true, 4);
        t.text(0, 0, 10, 3, "ab");
        t.text_rot(0, 0, 10, 3, "ab", 1);
        assert_eq!(row(&t, Table::Lines, 0).1, [-5, 3, 10, 20]);
        assert_eq!(row(&t, Table::Lines, 0).0, row(&t, Table::Lines, 1).0);
        assert_ne!(
            row(&t, Table::Lines, 0).0,
            row(&t, Table::Lines, 2).0,
            "layer"
        );
        assert_ne!(
            row(&t, Table::Lines, 0).0,
            row(&t, Table::Rects, 0).0,
            "kind"
        );
        assert_eq!(row(&t, Table::Arcs, 0).1, [70, 70, 130, 130]);
        assert_eq!(row(&t, Table::Polygons, 0).1, [0, -2, 7, 9]);
        assert_eq!(row(&t, Table::Texts, 0).1, [-10, -10, 30, 30]);
        assert_eq!(row(&t, Table::Texts, 1).1, [-30, -30, 30, 30], "turned");
        assert_ne!(
            row(&t, Table::Texts, 0).0,
            row(&t, Table::Texts, 1).0,
            "rot"
        );
        t.kill(5, 1);
        assert!(live(&t, Table::Texts, 0).is_some());
        assert!(live(&t, Table::Texts, 1).is_none());
    }
}
