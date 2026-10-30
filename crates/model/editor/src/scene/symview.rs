//! A symbol document drawn as its own view (rank 1–2): pins with their names and
//! directions, graphics, texts, each its own object; plus the junction dots of a schematic
//! view ([`redot`]) after its wires changed.
//!
//! Contents:
//! - `pub(super) fn redot`
//! - `pub(super) fn symbol`

use schemify_canvas::LineStyle;
use schemify_doc::{Document, TableId};
use schemify_format::RecordKind;

use crate::geometry::Pt;

use super::refresh::Build;
use super::{DOT_R, Hit, LAYER_JUNCTION, ObjKey, PIN_HALF, UNIT, symbols};

/// Makes the junction dots at `points` (sorted, unique) match xschem's rule for the current
/// wiring ([`Wiring::dot_at`]): adds missing dots, kills stale ones. Dots belong to no
/// object (owner `u32::MAX`): they are cues, not selectable records.
pub(super) fn redot(b: &mut Build<'_>, points: &[Pt]) {
    debug_assert!(points.windows(2).all(|w| w[0] < w[1]));
    for &p in points {
        let now = b.sc.wiring.dot_at(p);
        match (b.sc.wiring.dots.get(&p).copied(), now) {
            (None, true) => {
                let row = b.sc.prims.arcs.cx.len() as u32;
                b.sc.wiring.dots.insert(p, row);
                b.sc.prims.arc(p.0, p.1, DOT_R, 0, 360_000, LAYER_JUNCTION);
                b.owned(2, u32::MAX);
            }
            (Some(row), false) => {
                b.sc.wiring.dots.remove(&p);
                let r = row as usize;
                assert_eq!(
                    (b.sc.prims.arcs.cx[r], b.sc.prims.arcs.cy[r]),
                    p,
                    "the dot's row"
                );
                b.sc.prims.kill(2, r);
                if let Some(k) = b.sc.killed.as_mut() {
                    k[2].push(row);
                }
                b.sc.dead += 1;
            }
            _ => {}
        }
    }
}

pub(super) fn symbol(b: &mut Build<'_>, doc: &Document) {
    let p = symbols::sym_prims(doc);
    let g = doc.graphics();
    // One object per graphic row, in draw order; `sym_prims` pushed per kind, so walk the
    // table again to pair rows with primitives.
    let (mut li, mut ri, mut ai, mut pi, mut ti) = (0, 0, 0, 0, 0);
    for row in 0..g.shape.len() {
        let key = ObjKey {
            table: TableId::Graphics,
            key: row as u32,
        };
        match g.shape[row] {
            RecordKind::Line => {
                let (l, layer, st) = p.lines[li];
                li += 1;
                let obj = b.sc.objs.push(key, l, Hit::Segment);
                b.line((l[0], l[1]), (l[2], l[3]), layer, st, obj);
            }
            RecordKind::Rect => {
                let (r, layer, st) = p.rects[ri];
                ri += 1;
                let obj = b.sc.objs.push(key, r, Hit::Box);
                b.rect((r[0], r[1]), (r[2], r[3]), layer, st, obj);
            }
            RecordKind::Arc | RecordKind::Circle => {
                let (a, layer, st) = p.arcs[ai];
                ai += 1;
                let obj = b.sc.objs.push(
                    key,
                    [a[0] - a[2], a[1] - a[2], a[0] + a[2], a[1] + a[2]],
                    Hit::Box,
                );
                b.arc((a[0], a[1]), a[2], a[3], a[4], layer, st, obj);
            }
            RecordKind::Poly => {
                let (pts, closed, layer, st) = &p.polys[pi];
                pi += 1;
                let mut bx = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
                for q in pts {
                    bx = [
                        bx[0].min(q.0),
                        bx[1].min(q.1),
                        bx[2].max(q.0),
                        bx[3].max(q.1),
                    ];
                }
                let obj = b.sc.objs.push(key, bx, Hit::Box);
                b.poly(pts, *closed, *layer, *st, obj);
            }
            RecordKind::Stext => {
                let t = &p.texts[ti];
                ti += 1;
                let obj =
                    b.sc.objs
                        .push(key, [t.at.0, t.at.1, t.at.0, t.at.1], Hit::Box);
                let rot = t.orient.quarter_turns();
                let at = symbols::sym_text_at(t.at, t, &t.raw, rot, t.orient.mirrored());
                let bx = b.text_rot(at, t.size, t.layer, &t.raw, rot, obj);
                b.sc.objs.geom[obj as usize] = bx;
            }
            _ => {}
        }
    }
    assert!(
        li == p.lines.len() && ri == p.rects.len() && ai == p.arcs.len(),
        "every line, rect and arc primitive paired with its graphic row"
    );
    assert!(
        pi == p.polys.len() && ti == p.texts.len(),
        "every polygon and text primitive paired with its graphic row"
    );
    let pins = doc.pins();
    let s = doc.strings();
    for i in 0..pins.x.len() {
        let key = ObjKey {
            table: TableId::Pins,
            key: i as u32,
        };
        let q = (pins.x[i], pins.y[i]);
        let obj = b.sc.objs.push(
            key,
            [
                q.0 - PIN_HALF,
                q.1 - PIN_HALF,
                q.0 + PIN_HALF,
                q.1 + PIN_HALF,
            ],
            Hit::Box,
        );
        b.rect(
            (q.0 - PIN_HALF, q.1 - PIN_HALF),
            (q.0 + PIN_HALF, q.1 + PIN_HALF),
            5,
            LineStyle::PLAIN,
            obj,
        );
        let tb = b.text(
            (q.0 + 3 * UNIT, q.1 - 9 * UNIT),
            8 * UNIT,
            5,
            s.get(pins.name[i]),
            obj,
        );
        let g = &mut b.sc.objs.geom[obj as usize];
        *g = [
            g[0].min(tb[0]),
            g[1].min(tb[1]),
            g[2].max(tb[2]),
            g[3].max(tb[3]),
        ];
        b.conn_at(q, obj);
    }
}
