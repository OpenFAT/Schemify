//! The marks' rows: the selection marks over the scene in, the [`MARKS`] source's rows out.
//! Each live marked row is copied in its mark colour, 2 px wide and unfilled
//! (`canvas::draw_marks`); each run of marks of one colour is a layer range of its own, drawn
//! in mark order, as the CPU strokes a run as one path after the one before.
//!
//! Contents:
//! - `InstanceRows::pack_marks`

use schemify_canvas::{CanvasPrims, LAYER_DEAD, LAYERS, Marks, PrimKind};

use super::pens;
use super::rows::{self, InstanceRows, Slack};
use super::{ARC, EDGE, KINDS, LINE, MARKS, RECT, STRIDE, TRI};

/// A mark's stroke: 2 logical px (`canvas::draw_marks`), in 1/16 px.
const MARK_WIDTH_Q16: u32 = 32;

impl InstanceRows {
    /// Packs the marks source whole when `changed` (the marks or `scene` since the last call)
    /// or never packed: every live marked row of `scene` again.
    ///
    /// Cost: O(marks); nothing when unchanged.
    ///
    /// # Panics
    ///
    /// Panics on a mark past its table's end.
    pub fn pack_marks(&mut self, scene: &CanvasPrims, marks: &Marks, changed: bool) {
        if !changed && self.packed[MARKS] {
            return;
        }
        let n = marks.kind.len();
        assert_eq!(n, marks.index.len(), "mark columns in step");
        assert_eq!(n, marks.color.len(), "mark columns in step");
        let live = |i: usize| {
            let k = marks.index[i] as usize;
            let layer = match marks.kind[i] {
                PrimKind::Line => scene.lines.layer.get(k),
                PrimKind::Rect => scene.rects.layer.get(k),
                PrimKind::Arc => scene.arcs.layer.get(k),
                PrimKind::Polyline => scene.polylines.layer.get(k),
                PrimKind::Polygon => scene.polygons.layer.get(k),
            };
            *layer.expect("a mark names a row of its table") != LAYER_DEAD
        };
        // The colour run of each mark, the range it lands in.
        // ponytail: runs past the 32nd share the last range (drawn kind by kind there); a
        // selection is one colour, the visual diff a few.
        let mut runs = Vec::with_capacity(n);
        for i in 0..n {
            let next = i > 0 && marks.color[i] != marks.color[i - 1];
            let run: usize = runs.last().map_or(0, |&r: &usize| r + usize::from(next));
            runs.push(run);
        }
        debug_assert_eq!(runs.len(), n, "a run per mark");
        let range = |i: usize| runs[i].min(LAYERS - 1);
        let mut count = [[0u32; LAYERS]; KINDS];
        let live_marks = (0..n).filter(|&i| live(i));
        for i in live_marks.clone() {
            let k = marks.index[i] as usize;
            let (kind, edges) = match marks.kind[i] {
                PrimKind::Line => (LINE, 1),
                PrimKind::Rect => (RECT, 1),
                PrimKind::Arc => (ARC, 1),
                PrimKind::Polyline => (
                    EDGE,
                    rows::edges_of(&scene.polylines, k, /* closed */ false) as u32,
                ),
                PrimKind::Polygon => (
                    EDGE,
                    rows::edges_of(&scene.polygons, k, /* closed */ true) as u32,
                ),
            };
            count[kind][range(i)] += edges;
        }
        let tables = &mut self.tables[MARKS];
        for (kind, t) in tables.iter_mut().enumerate() {
            t.layout(kind, &count[kind], Slack::Tight);
        }
        let style = pens::style_word(MARK_WIDTH_Q16, 0, 0);
        let mut put = |kind: usize, run: usize, fields: &[u32]| {
            let t = &mut tables[kind];
            let s = t.take(run).expect("laid out") as usize;
            rows::row(&mut t.bytes, STRIDE[kind], s, &fields[..STRIDE[kind] / 4]);
        };
        for i in live_marks {
            let k = marks.index[i] as usize;
            let color = marks.color[i].swap_bytes();
            match marks.kind[i] {
                PrimKind::Line => put(
                    LINE,
                    range(i),
                    &rows::seg_fields(&scene.lines, k, color, style),
                ),
                PrimKind::Rect => put(
                    RECT,
                    range(i),
                    &rows::seg_fields(&scene.rects, k, color, style),
                ),
                PrimKind::Arc => put(
                    ARC,
                    range(i),
                    &rows::arc_fields(&scene.arcs, k, color, style),
                ),
                PrimKind::Polyline | PrimKind::Polygon => {
                    let closed = marks.kind[i] == PrimKind::Polygon;
                    let p = if closed {
                        &scene.polygons
                    } else {
                        &scene.polylines
                    };
                    for j in 0..rows::edges_of(p, k, closed) {
                        put(
                            EDGE,
                            range(i),
                            &rows::edge_fields(p, k, j, closed, color, style),
                        );
                    }
                }
            }
        }
        assert!(
            tables.iter().zip(&count).all(|(t, c)| t.used == *c),
            "every mark placed"
        );
        assert_eq!(self.count(MARKS, TRI), 0, "a mark never fills");
        self.packed[MARKS] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks_exact(4)
            .map(|c| u32::from_ne_bytes(c.try_into().expect("4 bytes")))
            .collect()
    }

    /// Marks copy the marked rows' geometry in the mark colour, 2 px wide, unfilled, skipping
    /// dead rows; polygon marks are edges; unchanged marks write nothing.
    #[test]
    fn marks_copy_the_row_two_px_wide() {
        let mut t = CanvasPrims::default();
        t.line(0, 0, 10, 10, 1);
        t.line(0, 0, 10, 10, 1);
        t.rect(0, 0, 5, 5, 5);
        t.poly(&[(0, 0), (10, 0), (10, 10)], true, 4);
        t.kill(0, 1);
        let mut m = Marks::default();
        m.push(PrimKind::Line, 0, 0xffff_00ff);
        m.push(PrimKind::Line, 1, 0xffff_00ff);
        m.push(PrimKind::Rect, 0, 0x00ff_00ff);
        m.push(PrimKind::Polygon, 0, 0xffff_00ff);
        let mut r = InstanceRows::default();
        r.pack_marks(&t, &m, false);
        assert_eq!(
            (r.count(MARKS, LINE), r.count(MARKS, EDGE)),
            (1, 3),
            "the dead line skipped"
        );
        let s = words(r.bytes(MARKS, LINE));
        assert_eq!(s[4].to_ne_bytes(), [0xff, 0xff, 0x00, 0xff]);
        assert_eq!(s[5], 32, "2 px");
        assert_eq!(words(r.bytes(MARKS, RECT))[5], 32, "a mark never fills");
        assert_eq!(r.count(MARKS, TRI), 0);
        // Three colour runs (yellow, green, yellow), drawn in that order.
        assert_eq!(r.layer_slots(MARKS, LINE, 0).len(), 1);
        assert_eq!(r.layer_slots(MARKS, RECT, 1).len(), 1);
        assert_eq!(r.layer_slots(MARKS, EDGE, 2).len(), 3);
        assert_eq!(r.dirty_bytes(), 24 + 24 + 3 * 24);
        r.clear_dirty();
        r.pack_marks(&t, &m, false);
        assert_eq!(r.dirty_bytes(), 0, "unchanged marks write nothing");
    }
}
