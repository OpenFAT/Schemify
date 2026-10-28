//! Drawing one document row as primitives: instances (with their symbol and texts), wires,
//! shapes, texts, symbol pins and graphics. In: a row of the document. Out: rows appended to
//! the scene's tables through `Build`.

use super::refresh::{Build, stamped};
use super::symbols::{
    SymText, Symbols, attr, layer_attr, size_of, style_attr, sym_prims, sym_text_at, text_size,
    text_start,
};
use super::{DOT_R, LAYER_BUS, LAYER_JUNCTION, LAYER_MISSING, ObjKey, PIN_HALF, Scene, UNIT};
use crate::geometry::{Pt, place};
use schemify_canvas::LineStyle;
use schemify_doc::{DocHandle, Document, Project, Sym, TableId};
use schemify_elab::CellState;
use schemify_expr::{Scratch, Value};
use schemify_format::{Orient, RecordKind};
use std::collections::HashMap;

/// Reusable buffers of instance stamping (resolved cells, template inputs, text).
#[derive(Default)]
pub(super) struct Stamp<'d> {
    resolved: HashMap<Sym, Option<usize>>,
    inputs: Vec<Value<'d>>,
    scratch: Scratch,
    pub(super) text: String,
}

/// Stamps instance row `i`: the resolved cell's symbol through the placement transform, or
/// a red placeholder with the cell reference when it does not resolve.
#[allow(clippy::too_many_arguments)]
pub(super) fn instance<'d>(
    b: &mut Build<'_>,
    project: &'d Project,
    h: DocHandle,
    doc: &'d Document,
    sym: &Symbols,
    values: Option<&'d CellState>,
    i: usize,
    st: &mut Stamp<'d>,
) {
    let s = doc.strings();
    let inst = doc.instances();
    let key = ObjKey {
        table: TableId::Instances,
        key: inst.id[i].0,
    };
    let (o, at) = (inst.orient[i], (inst.x[i], inst.y[i]));
    let cellref = s.get(inst.cell[i]);
    let row = *st
        .resolved
        .entry(inst.cell[i])
        .or_insert_with(|| project.resolve(h, cellref).ok());
    let Some(p) = row.and_then(|r| sym.get(r)) else {
        let u = 10 * UNIT;
        let obj =
            b.sc.objs
                .push(key, [at.0 - u, at.1 - u, at.0 + u, at.1 + u], false);
        b.rect(
            (at.0 - u, at.1 - u),
            (at.0 + u, at.1 + u),
            LAYER_MISSING,
            LineStyle::PLAIN,
            obj,
        );
        b.text(
            (at.0 - u, at.1 + 11 * UNIT),
            8 * UNIT,
            LAYER_MISSING,
            cellref,
            obj,
        );
        return;
    };
    let x = |q: Pt| place(o, at, q);
    let (c0, c1) = (x((p.bbox[0], p.bbox[1])), x((p.bbox[2], p.bbox[3])));
    let obj = b.sc.objs.push(key, [c0.0, c0.1, c1.0, c1.1], false);
    for &(l, layer, st) in &p.lines {
        b.line(x((l[0], l[1])), x((l[2], l[3])), layer, st, obj);
    }
    for &(r, layer, st) in &p.rects {
        b.rect(x((r[0], r[1])), x((r[2], r[3])), layer, st, obj);
    }
    for &(a, layer, st) in &p.arcs {
        let (start, sweep) = arc_placed(o, a[3], a[4]);
        b.arc(x((a[0], a[1])), a[2], start, sweep, layer, st, obj);
    }
    for (pts, closed, layer, st) in &p.polys {
        let q: Vec<Pt> = pts.iter().map(|&q| x(q)).collect();
        b.poly(&q, *closed, *layer, *st, obj);
    }
    for &pin in &p.pins {
        let q = x(pin);
        b.rect(
            (q.0 - PIN_HALF, q.1 - PIN_HALF),
            (q.0 + PIN_HALF, q.1 + PIN_HALF),
            5,
            LineStyle::PLAIN,
            obj,
        );
        b.conn_at(q, obj);
    }
    let iface = row.and_then(|r| project.interface(r));
    for t in &p.texts {
        let (q, rot) = inst_text(t, (o, at), (doc, i, iface, cellref), values, st);
        b.text_rot(q, t.size, t.layer, &st.text, rot, obj);
    }
}

/// Renders symbol text `t` of instance row `i` (placed at `at` turned `o`) into `st.text`;
/// returns its anchor and quarter turns, as [`Build::text_rot`] takes them.
pub(super) fn inst_text<'d>(
    t: &SymText,
    (o, at): (Orient, Pt),
    (doc, i, iface, cellref): (&'d Document, usize, Option<&'d Document>, &'d str),
    values: Option<&'d CellState>,
    st: &mut Stamp<'d>,
) -> (Pt, u8) {
    let Stamp {
        inputs,
        scratch,
        text,
        ..
    } = st;
    text.clear();
    let rendered = t.template.as_ref().is_some_and(|tpl| {
        let ok = tpl
            .program()
            .bind(inputs, |name| {
                bind_ident(name, doc, i, iface, cellref, values)
            })
            .is_ok();
        ok && tpl.render(inputs, scratch, text).is_ok()
    });
    if !rendered {
        text.clear();
        text.push_str(&t.raw);
    }
    let turned = crate::geometry::compose(o, t.orient);
    let rot = turned.quarter_turns();
    (
        sym_text_at(place(o, at, t.at), t, text, rot, turned.mirrored()),
        rot,
    )
}

/// Re-renders the texts of instance row `i`, drawn as object `o` whose only change is its
/// values, into its own text rows in place, reporting the rewritten rows through
/// [`Scene::edits`]. False (rows possibly half rewritten, to be removed by the caller) when
/// a text's row would move, resize or appear or vanish (a value of another length): then the
/// instance is re-stamped whole.
///
/// Cost: O(the instance's texts), no row added or killed.
#[allow(clippy::too_many_arguments)]
pub(super) fn revalue_texts<'d>(
    sc: &mut Scene,
    project: &'d Project,
    h: DocHandle,
    doc: &'d Document,
    sym: &Symbols,
    values: Option<&'d CellState>,
    (i, o): (usize, u32),
    st: &mut Stamp<'d>,
) -> bool {
    let inst = doc.instances();
    let (orient, at) = (inst.orient[i], (inst.x[i], inst.y[i]));
    let cellref = doc.strings().get(inst.cell[i]);
    let row = *st
        .resolved
        .entry(inst.cell[i])
        .or_insert_with(|| project.resolve(h, cellref).ok());
    let Some(p) = row.and_then(|r| sym.get(r)) else {
        return false; // the placeholder has no value texts
    };
    let iface = row.and_then(|r| project.interface(r));
    let rows = stamped(&sc.stamps.texts, sc.objs.stamp[o as usize]);
    let mut next = rows.start;
    for t in &p.texts {
        let (q, rot) = inst_text(t, (orient, at), (doc, i, iface, cellref), values, st);
        let Some((from, drawn)) = text_start(q, t.size, &st.text, rot) else {
            continue; // drew no row before either, or the count check below fails
        };
        if next == rows.end {
            return false;
        }
        let (r, tx) = (next, &mut sc.prims.texts);
        next += 1;
        assert_eq!(sc.owner.texts[r], o, "the object's own rows");
        let start = if r == 0 { 0 } else { tx.end[r - 1] as usize };
        let old = &tx.text[start..tx.end[r] as usize];
        let same_row = (tx.x[r], tx.y[r], tx.size[r], tx.layer[r], tx.rot[r])
            == (from.0, from.1, t.size, t.layer, drawn);
        if !same_row || old.len() != st.text.len() || old.chars().count() != st.text.chars().count()
        {
            return false;
        }
        if old != st.text {
            tx.text.replace_range(start..tx.end[r] as usize, &st.text);
            if let Some(k) = sc.killed.as_mut() {
                k[5].push(r as u32);
            }
        }
    }
    next == rows.end
}

/// Draws wire row `i` (its connection points included; dots are [`redot`]'s).
pub(super) fn wire(b: &mut Build<'_>, doc: &Document, i: usize) {
    let w = doc.wires();
    let (a, c) = ((w.x1[i], w.y1[i]), (w.x2[i], w.y2[i]));
    let key = ObjKey {
        table: TableId::Wires,
        key: w.id[i].0,
    };
    let bus = attr(doc, w.attrs[i], "bus").is_some_and(|v| v == "true");
    let obj = b.sc.objs.push(key, [a.0, a.1, c.0, c.1], true);
    b.line(a, c, if bus { LAYER_BUS } else { 1 }, LineStyle::PLAIN, obj);
    b.conn_at(a, obj);
    b.conn_at(c, obj);
}

/// Labels, ports, noconns, texts and shapes of rows `rows` (clamped to each table) in that
/// order.
pub(super) fn other_objects(b: &mut Build<'_>, doc: &Document, rows: std::ops::Range<usize>) {
    for table in [
        TableId::Labels,
        TableId::Ports,
        TableId::Noconns,
        TableId::Texts,
        TableId::Shapes,
    ] {
        let len = match table {
            TableId::Labels => doc.labels().id.len(),
            TableId::Ports => doc.ports().id.len(),
            TableId::Noconns => doc.noconns().id.len(),
            TableId::Texts => doc.texts().id.len(),
            _ => doc.shapes().id.len(),
        };
        for i in rows.start.min(len)..rows.end.min(len) {
            one_other(b, doc, table, i);
        }
    }
}

/// Draws row `i` of a label, port, noconn, text or shape table.
pub(super) fn one_other(b: &mut Build<'_>, doc: &Document, table: TableId, i: usize) {
    let s = doc.strings();
    match table {
        TableId::Labels | TableId::Ports => {
            // Labels: a pin square at the anchor and the net name beside it, in wire color;
            // ports: an arrow.
            let t = if table == TableId::Labels {
                doc.labels()
            } else {
                doc.ports()
            };
            let key = ObjKey {
                table,
                key: t.id[i].0,
            };
            let port = table == TableId::Ports;
            placed(
                b,
                key,
                (t.x[i], t.y[i]),
                t.orient[i],
                s.get(t.text[i]),
                port,
            );
        }
        TableId::Noconns => {
            let n = doc.noconns();
            let key = ObjKey {
                table,
                key: n.id[i].0,
            };
            let (x, y) = (n.x[i], n.y[i]);
            let r = 4 * UNIT;
            let obj = b.sc.objs.push(key, [x - r, y - r, x + r, y + r], false);
            b.line((x - r, y - r), (x + r, y + r), 4, LineStyle::PLAIN, obj);
            b.line((x - r, y + r), (x + r, y - r), 4, LineStyle::PLAIN, obj);
            b.conn_at((x, y), obj);
        }
        TableId::Texts => {
            let t = doc.texts();
            let key = ObjKey {
                table,
                key: t.id[i].0,
            };
            let size = size_of(attr(doc, t.attrs[i], "size"), 0.4);
            let layer = layer_attr(doc, t.attrs[i], 3);
            let at = (t.x[i], t.y[i]);
            let obj = b.sc.objs.push(key, [at.0, at.1, at.0, at.1], false);
            // Rotated texts (format spec §3 orientation): turned about the anchor; a mirror
            // keeps the text readable.
            let rot = t.orient[i].quarter_turns();
            let bx = b.text_rot(at, size, layer, s.get(t.text[i]), rot, obj);
            b.sc.objs.geom[obj as usize] = bx;
        }
        TableId::Shapes => shape(b, doc, i),
        _ => unreachable!("{table:?} has no canvas objects"),
    }
}

/// Draws row `i` of the schematic graphics (`shape` records, format spec §4 rank 3): one
/// object each, on its `layer` (default 4, the symbol layer, like xschem's default drawing
/// layer); a malformed row draws nothing.
pub(super) fn shape(b: &mut Build<'_>, doc: &Document, i: usize) {
    let sh = doc.shapes();
    let key = ObjKey {
        table: TableId::Shapes,
        key: sh.id[i].0,
    };
    let c = &doc.pool().coords[sh.coords[i].range()];
    let layer = layer_attr(doc, sh.attrs[i], 4);
    let st = style_attr(doc, sh.attrs[i]);
    match sh.form[i] {
        RecordKind::Line if c.len() == 4 => {
            let obj = b.sc.objs.push(key, [c[0], c[1], c[2], c[3]], true);
            b.line((c[0], c[1]), (c[2], c[3]), layer, st, obj);
        }
        RecordKind::Rect if c.len() == 4 => {
            let obj = b.sc.objs.push(key, [c[0], c[1], c[2], c[3]], false);
            b.rect((c[0], c[1]), (c[2], c[3]), layer, st, obj);
        }
        RecordKind::Arc | RecordKind::Circle if c.len() >= 3 => {
            let (start, sweep) = match c {
                [_, _, _, a, s] => (*a, *s),
                _ => (0, 360_000),
            };
            let r = c[2].max(0);
            let obj =
                b.sc.objs
                    .push(key, [c[0] - r, c[1] - r, c[0] + r, c[1] + r], false);
            b.arc((c[0], c[1]), r, start, sweep, layer, st, obj);
        }
        RecordKind::Poly if c.len() >= 4 => {
            let pts: Vec<Pt> = c.chunks_exact(2).map(|q| (q[0], q[1])).collect();
            let closed = attr(doc, sh.attrs[i], "closed") == Some("true");
            let obj =
                b.sc.objs
                    .push(key, [pts[0].0, pts[0].1, pts[0].0, pts[0].1], false);
            b.poly(&pts, closed, layer, st, obj);
        }
        _ => {}
    }
}

/// Binds a template identifier for instance row `i`: `name`, `cell`, then the param's
/// evaluated value from `values` (elab), then the instance's override text, then the cell's
/// default (numbers as numbers, the rest as text).
pub(super) fn bind_ident<'d>(
    name: &str,
    doc: &'d Document,
    i: usize,
    iface: Option<&'d Document>,
    cellref: &'d str,
    values: Option<&'d CellState>,
) -> Option<Value<'d>> {
    let s = doc.strings();
    let inst = doc.instances();
    let text = match name {
        "name" => s.get(inst.name[i]),
        "cell" => cellref.rsplit([':', '/']).next().unwrap_or(cellref),
        _ => {
            if let Some(v) = values.and_then(|st| param_value(st, i, name)) {
                return Some(v);
            }
            let over = doc
                .pool()
                .find_attr(s, inst.attrs[i], name)
                .and_then(|v| v.first())
                .map(|v| s.get(*v));
            match over {
                Some(v) => v,
                None => {
                    let iface = iface?;
                    let is = iface.strings();
                    let p = iface.params();
                    let row = (0..p.fields.len()).find(|&r| is.get(p.fields[r][0]) == name)?;
                    is.get(p.fields[row][1])
                }
            }
        }
    };
    Some(match schemify_expr::parse_num(text) {
        Ok(v) if name != "name" => Value::Num(v),
        _ => Value::Str(text),
    })
}

/// The evaluated value of param `name` of instance row `i` (`None`: unresolved cell, no
/// such param, or an evaluation error, which the docs page and checks report).
pub(super) fn param_value<'c>(st: &'c CellState, i: usize, name: &str) -> Option<Value<'c>> {
    let iface = st.ifaces.get(*st.inst_iface.get(i)? as usize)?;
    let n = st.names.find(name)?;
    let k = iface.param_name.iter().position(|&p| p == n)?;
    let node = st.graph.inst_param(i, k)?;
    st.graph.vals.get(node as usize)?.value()
}

/// Maps a symbol-local arc through an instance orientation (millidegrees).
pub(super) fn arc_placed(o: Orient, start: i32, sweep: i32) -> (i32, i32) {
    let mut a = start;
    if o.mirrored() {
        a = 180_000 - a - sweep;
    }
    a += 90_000 * i32::from(o.quarter_turns());
    (a.rem_euclid(360_000), sweep)
}

/// A label (`port = false`) or port: pin square or arrow at the anchor and the name beside
/// it, on the side the orientation points to.
pub(super) fn placed(b: &mut Build<'_>, key: ObjKey, at: Pt, o: Orient, name: &str, port: bool) {
    let obj = b.sc.objs.push(key, [at.0, at.1, at.0, at.1], false);
    let dir = place(o, (0, 0), (1, 0));
    let size = 16 * UNIT;
    let (w, h) = text_size(size, name);
    let mut bx = [
        at.0 - PIN_HALF,
        at.1 - PIN_HALF,
        at.0 + PIN_HALF,
        at.1 + PIN_HALF,
    ];
    if port {
        // ipin-like arrow pointing at the anchor from the far side.
        let (dx, dy) = (-dir.0, -dir.1);
        let p = |u: i32, v: i32| (at.0 + dx * u - dy * v, at.1 + dy * u + dx * v);
        let arrow = [
            p(0, 0),
            p(5 * UNIT, -5 * UNIT),
            p(25 * UNIT, -5 * UNIT),
            p(25 * UNIT, 5 * UNIT),
            p(5 * UNIT, 5 * UNIT),
        ];
        b.poly(&arrow, true, 5, LineStyle::PLAIN, obj);
        for q in arrow {
            bx = [
                bx[0].min(q.0),
                bx[1].min(q.1),
                bx[2].max(q.0),
                bx[3].max(q.1),
            ];
        }
    } else {
        b.rect(
            (at.0 - PIN_HALF, at.1 - PIN_HALF),
            (at.0 + PIN_HALF, at.1 + PIN_HALF),
            5,
            LineStyle::PLAIN,
            obj,
        );
    }
    let off = if port { 28 * UNIT } else { 4 * UNIT };
    let tl = match dir {
        (1, 0) if port => (at.0 - off - w, at.1 - h / 2),
        (-1, 0) if port => (at.0 + off, at.1 - h / 2),
        (1, 0) => (at.0 + off, at.1 - h - UNIT),
        (-1, 0) => (at.0 - off - w, at.1 - h - UNIT),
        (_, d) if d > 0 => (at.0 + UNIT, at.1 + off),
        _ => (at.0 + UNIT, at.1 - off - h),
    };
    let tb = b.text(tl, size, 1, name, obj);
    bx = [
        bx[0].min(tb[0]),
        bx[1].min(tb[1]),
        bx[2].max(tb[2]),
        bx[3].max(tb[3]),
    ];
    b.sc.objs.geom[obj as usize] = bx;
    b.conn_at(at, obj);
}

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
    let p = sym_prims(doc);
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
                let obj = b.sc.objs.push(key, l, true);
                b.line((l[0], l[1]), (l[2], l[3]), layer, st, obj);
            }
            RecordKind::Rect => {
                let (r, layer, st) = p.rects[ri];
                ri += 1;
                let obj = b.sc.objs.push(key, r, false);
                b.rect((r[0], r[1]), (r[2], r[3]), layer, st, obj);
            }
            RecordKind::Arc | RecordKind::Circle => {
                let (a, layer, st) = p.arcs[ai];
                ai += 1;
                let obj = b.sc.objs.push(
                    key,
                    [a[0] - a[2], a[1] - a[2], a[0] + a[2], a[1] + a[2]],
                    false,
                );
                b.arc((a[0], a[1]), a[2], a[3], a[4], layer, st, obj);
            }
            RecordKind::Poly => {
                if pi >= p.polys.len() {
                    continue;
                }
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
                let obj = b.sc.objs.push(key, bx, false);
                b.poly(pts, *closed, *layer, *st, obj);
            }
            RecordKind::Stext => {
                let t = &p.texts[ti];
                ti += 1;
                let obj = b.sc.objs.push(key, [t.at.0, t.at.1, t.at.0, t.at.1], false);
                let rot = t.orient.quarter_turns();
                let at = sym_text_at(t.at, t, &t.raw, rot, t.orient.mirrored());
                let bx = b.text_rot(at, t.size, t.layer, &t.raw, rot, obj);
                b.sc.objs.geom[obj as usize] = bx;
            }
            _ => {}
        }
    }
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
            false,
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
