//! Previews drawn over the scene: a selection moved by a transform, a symbol stamped at the
//! cursor, records being placed. In: the scene or the symbol cache. Out: overlay primitives.

use super::draw::arc_placed;
use super::symbols::Symbols;
use super::{LAYER_SEL, PIN_HALF, Scene, UNIT};
use crate::geometry::{Pt, Xform, place};
use schemify_canvas::CanvasPrims;
use schemify_format::{Orient, RecordKind};

/// Copies the primitives of selected objects through `x` into `out` on [`LAYER_SEL`]
/// (move and copy previews; texts included, so a moving label shows its name), in table then
/// row order, from the selection's rows ([`selected_prims`] of `sc`). Cost: O(selected
/// primitives).
pub fn push_moved(sc: &Scene, rows: &[(u8, u32)], x: Xform, out: &mut CanvasPrims) {
    let p = &sc.prims;
    let before = out.len();
    let mut pts: Vec<Pt> = Vec::new();
    for &(kind, i) in rows {
        let i = i as usize;
        match kind {
            0 => {
                let (a, c) = (
                    x.point((p.lines.x0[i], p.lines.y0[i])),
                    x.point((p.lines.x1[i], p.lines.y1[i])),
                );
                out.line(a.0, a.1, c.0, c.1, LAYER_SEL);
            }
            1 => {
                let (a, c) = (
                    x.point((p.rects.x0[i], p.rects.y0[i])),
                    x.point((p.rects.x1[i], p.rects.y1[i])),
                );
                out.rect(a.0, a.1, c.0, c.1, LAYER_SEL);
            }
            2 => {
                let c = x.point((p.arcs.cx[i], p.arcs.cy[i]));
                let (s, w) = x.arc(
                    f64::from(p.arcs.start[i]) / 1000.0,
                    f64::from(p.arcs.sweep[i]) / 1000.0,
                );
                out.arc(
                    c.0,
                    c.1,
                    p.arcs.r[i],
                    (s * 1000.0) as i32,
                    (w * 1000.0) as i32,
                    LAYER_SEL,
                );
            }
            3 | 4 => {
                let polys = if kind == 3 { &p.polylines } else { &p.polygons };
                let start = if i == 0 { 0 } else { polys.end[i - 1] as usize };
                pts.clear();
                pts.extend(
                    (start..polys.end[i] as usize).map(|j| x.point((polys.x[j], polys.y[j]))),
                );
                out.poly(&pts, kind == 4, LAYER_SEL);
            }
            _ => {
                let t = &p.texts;
                let start = if i == 0 { 0 } else { t.end[i - 1] as usize };
                let a = x.point((t.x[i], t.y[i]));
                out.text(
                    a.0,
                    a.1,
                    t.size[i],
                    LAYER_SEL,
                    &t.text[start..t.end[i] as usize],
                );
            }
        }
    }
    assert!(out.len() >= before);
}

/// Stamps cell `row`'s symbol at `at` with orientation `o` into `out` on [`LAYER_SEL`]
/// (placement preview; texts drawn raw). Returns false when the cell has no cached symbol.
pub fn stamp(symbols: &Symbols, row: usize, o: Orient, at: Pt, out: &mut CanvasPrims) -> bool {
    let Some(p) = symbols.get(row) else {
        return false;
    };
    let x = |q: Pt| place(o, at, q);
    for (l, ..) in &p.lines {
        let (a, b) = (x((l[0], l[1])), x((l[2], l[3])));
        out.line(a.0, a.1, b.0, b.1, LAYER_SEL);
    }
    for (r, ..) in &p.rects {
        let (a, b) = (x((r[0], r[1])), x((r[2], r[3])));
        out.rect(a.0, a.1, b.0, b.1, LAYER_SEL);
    }
    for (a, ..) in &p.arcs {
        let c = x((a[0], a[1]));
        let (s, w) = arc_placed(o, a[3], a[4]);
        out.arc(c.0, c.1, a[2], s, w, LAYER_SEL);
    }
    for (pts, closed, ..) in &p.polys {
        let q: Vec<Pt> = pts.iter().map(|&q| x(q)).collect();
        out.poly(&q, *closed, LAYER_SEL);
    }
    for &pin in &p.pins {
        let q = x(pin);
        out.rect(
            q.0 - PIN_HALF,
            q.1 - PIN_HALF,
            q.0 + PIN_HALF,
            q.1 + PIN_HALF,
            LAYER_SEL,
        );
    }
    true
}

/// Outlines clipboard records through `x` (paste preview): wires and graphics as drawn,
/// everything else as a small box at its anchor plus its text.
pub fn preview_records(recs: &[schemify_doc::RecordBuf], x: Xform, out: &mut CanvasPrims) {
    preview_records_on(recs, x, LAYER_SEL, out);
}

/// [`preview_records`] on `layer` (the staged-edit diff draws inserted records on its own).
pub fn preview_records_on(
    recs: &[schemify_doc::RecordBuf],
    x: Xform,
    layer: u8,
    out: &mut CanvasPrims,
) {
    for r in recs {
        let mut r = r.clone();
        if crate::geometry::transform_record(&mut r, x).is_none() {
            continue;
        }
        let c = |i: usize| {
            r.fields
                .get(i)
                .and_then(|v| schemify_format::parse_coord(v).ok())
                .unwrap_or(0)
        };
        match r.kind {
            RecordKind::Wire => out.line(c(1), c(2), c(3), c(4), layer),
            RecordKind::Line => out.line(c(0), c(1), c(2), c(3), layer),
            RecordKind::Rect => out.rect(c(0), c(1), c(2), c(3), layer),
            _ => {
                let Some(a) = crate::geometry::anchor(&r) else {
                    continue;
                };
                let u = 5 * UNIT;
                out.rect(a.0 - u, a.1 - u, a.0 + u, a.1 + u, layer);
                let text = match r.kind {
                    RecordKind::Inst => r.find_attr("name").and_then(|v| v.values.first()).cloned(),
                    RecordKind::Label | RecordKind::Port | RecordKind::Text => {
                        r.fields.get(4).cloned()
                    }
                    _ => None,
                };
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    out.text(a.0 + u, a.1 - 3 * u, 12 * UNIT, layer, &t);
                }
            }
        }
    }
}
