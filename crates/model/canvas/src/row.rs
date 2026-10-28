//! One row of a [`CanvasPrims`] table seen uniformly: the row's hash (everything drawn about
//! it) and its world box. What [`CanvasDiff`](crate::CanvasDiff) diffs and
//! [`PrimIndex`](crate::PrimIndex) grids. Crate-private; no spec clause (the hash is ours and
//! never leaves the process).
//!
//! Contents:
//! - `pub(crate) enum Table`, `pub(crate) const TABLES: [Table; 6]`
//! - `pub(crate) struct Row { hash, bounds }`
//! - `pub(crate) fn len(t, table) -> usize`, `layers(t, table) -> &[u8]`,
//!   `layers_mut(t, table) -> &mut [u8]`, `read(t, table, i) -> Row`,
//!   `live(t, table, i) -> Option<[i32; 4]>`, `mix(h, v) -> u64`

use crate::{CanvasPrims, LAYER_DEAD, LineStyle};

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

/// One row as the diff and the index see it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Row {
    /// Everything drawn about the row (kind, coordinates, layer, style, label bytes).
    pub(crate) hash: u64,
    /// World box `[x0, y0, x1, y1]`, `x0 <= x1` and `y0 <= y1`.
    pub(crate) bounds: [i32; 4],
}

/// Rows of `table`, dead included.
pub(crate) fn len(t: &CanvasPrims, table: Table) -> usize {
    let rows = match table {
        Table::Lines => t.lines.x0.len(),
        Table::Rects => t.rects.x0.len(),
        Table::Arcs => t.arcs.cx.len(),
        Table::Polylines => t.polylines.end.len(),
        Table::Polygons => t.polygons.end.len(),
        Table::Texts => t.texts.x.len(),
    };
    debug_assert_eq!(layers(t, table).len(), rows, "layer column in step");
    rows
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
pub(crate) fn layers_mut(t: &mut CanvasPrims, table: Table) -> &mut [u8] {
    match table {
        Table::Lines => &mut t.lines.layer,
        Table::Rects => &mut t.rects.layer,
        Table::Arcs => &mut t.arcs.layer,
        Table::Polylines => &mut t.polylines.layer,
        Table::Polygons => &mut t.polygons.layer,
        Table::Texts => &mut t.texts.layer,
    }
}

/// Row `i` of `table`: its hash and its world box.
///
/// # Panics
///
/// If `i` is past the table, or a chain has fewer than 2 points.
#[inline]
pub(crate) fn read(t: &CanvasPrims, table: Table, i: usize) -> Row {
    let span =
        |x0: i32, y0: i32, x1: i32, y1: i32| [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    let row = match table {
        Table::Lines | Table::Rects => {
            let (seed, l) = match table {
                Table::Lines => (1, &t.lines),
                _ => (2, &t.rects),
            };
            let fields = [
                l.x0[i],
                l.y0[i],
                l.x1[i],
                l.y1[i],
                pen(l.layer[i], l.style[i]),
            ];
            Row {
                hash: fold(seed, &fields),
                bounds: span(fields[0], fields[1], fields[2], fields[3]),
            }
        }
        Table::Arcs => {
            let a = &t.arcs;
            let (cx, cy, r) = (a.cx[i], a.cy[i], a.r[i]);
            // A negative radius would invert the box below.
            debug_assert!(r >= 0, "arc {i} has a negative radius");
            let fields = [
                cx,
                cy,
                r,
                a.start[i],
                a.sweep[i],
                pen(a.layer[i], a.style[i]),
            ];
            Row {
                hash: fold(3, &fields),
                bounds: [
                    cx.saturating_sub(r),
                    cy.saturating_sub(r),
                    cx.saturating_add(r),
                    cy.saturating_add(r),
                ],
            }
        }
        Table::Polylines | Table::Polygons => {
            let (seed, p) = match table {
                Table::Polylines => (4, &t.polylines),
                _ => (5, &t.polygons),
            };
            let points = crate::chain(&p.end, i);
            let (xs, ys) = (&p.x[points.clone()], &p.y[points]);
            assert!(xs.len() >= 2, "chain {i} has fewer than 2 points");
            let hash = xs
                .iter()
                .zip(ys)
                .fold(fold(seed, &[pen(p.layer[i], p.style[i])]), |h, (&x, &y)| {
                    mix(mix(h, i64::from(x)), i64::from(y))
                });
            let (Some(&x0), Some(&y0), Some(&x1), Some(&y1)) = (
                xs.iter().min(),
                ys.iter().min(),
                xs.iter().max(),
                ys.iter().max(),
            ) else {
                unreachable!("a chain of 2 or more points has extremes");
            };
            Row {
                hash,
                bounds: [x0, y0, x1, y1],
            }
        }
        Table::Texts => text_row(t, i),
    };
    debug_assert!(row.bounds[0] <= row.bounds[2], "row box ordered in x");
    debug_assert!(row.bounds[1] <= row.bounds[3], "row box ordered in y");
    row
}

/// Row `i`'s world box, or `None` when the row was killed ([`CanvasPrims::kill`]).
///
/// # Panics
///
/// If `i` is past the table.
pub(crate) fn live(t: &CanvasPrims, table: Table, i: usize) -> Option<[i32; 4]> {
    let layer = layers(t, table);
    assert!(i < layer.len(), "row {i} of {table:?} past the table");
    (layer[i] != LAYER_DEAD).then(|| read(t, table, i).bounds)
}

/// Folds one field into a row hash (deterministic on every target).
pub(crate) fn mix(h: u64, v: i64) -> u64 {
    (h.rotate_left(5) ^ v.cast_unsigned()).wrapping_mul(0x517c_c1b7_2722_0a95)
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
fn text_row(t: &CanvasPrims, i: usize) -> Row {
    let texts = &t.texts;
    let bytes = &texts.text.as_bytes()[crate::chain(&texts.end, i)];
    // ponytail: a table filled column by column may lack `rot`; it reads as unturned.
    let rot = texts.rot.get(i).copied().unwrap_or(0);
    let fields = [
        texts.x[i],
        texts.y[i],
        texts.size[i],
        i32::from(texts.layer[i]),
        i32::from(rot),
    ];
    let hash = bytes
        .iter()
        .fold(fold(6, &fields), |h, &b| mix(h, i64::from(b)));
    // Labels hang right and down from their anchor: at most an em per byte across and two
    // lines down (what `draw_texts` treats as visible), padded by an em; a turned label reaches
    // as far in any direction.
    let (x, y, em) = (
        i64::from(fields[0]),
        i64::from(fields[1]),
        i64::from(fields[2]),
    );
    // A non-positive em would invert the box below.
    debug_assert!(em > 0, "label {i} has a non-positive font size");
    let reach = em * (bytes.len() as i64 + 1).max(3);
    // Lossless: clamped into `i32` first.
    let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    let bounds = if rot == 0 {
        [
            clamp(x - em),
            clamp(y - em),
            clamp(x + reach),
            clamp(y + 3 * em),
        ]
    } else {
        [
            clamp(x - reach),
            clamp(y - reach),
            clamp(x + reach),
            clamp(y + reach),
        ]
    };
    Row { hash, bounds }
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
        assert_eq!(read(&t, Table::Lines, 0).bounds, [-5, 3, 10, 20]);
        assert_eq!(
            read(&t, Table::Lines, 0).hash,
            read(&t, Table::Lines, 1).hash
        );
        assert_ne!(
            read(&t, Table::Lines, 0).hash,
            read(&t, Table::Lines, 2).hash,
            "layer"
        );
        assert_ne!(
            read(&t, Table::Lines, 0).hash,
            read(&t, Table::Rects, 0).hash,
            "kind"
        );
        assert_eq!(read(&t, Table::Arcs, 0).bounds, [70, 70, 130, 130]);
        assert_eq!(read(&t, Table::Polygons, 0).bounds, [0, -2, 7, 9]);
        assert_eq!(read(&t, Table::Texts, 0).bounds, [-10, -10, 30, 30]);
        assert_eq!(
            read(&t, Table::Texts, 1).bounds,
            [-30, -30, 30, 30],
            "turned"
        );
        assert_ne!(
            read(&t, Table::Texts, 0).hash,
            read(&t, Table::Texts, 1).hash,
            "rot"
        );
        t.kill(5, 1);
        assert!(live(&t, Table::Texts, 0).is_some());
        assert!(live(&t, Table::Texts, 1).is_none());
    }
}
