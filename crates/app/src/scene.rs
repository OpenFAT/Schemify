//! `scene`: the active document drawn as canvas primitive tables (`render::canvas`), plus
//! the derived tables the editor reads back — which object owns each primitive, every
//! object's geometry for hit-testing and box selection, connection points, junction dots.
//!
//! Data design:
//! 1. In: one `doc::Document` (rank 3 for a schematic, ranks 1–2 for a symbol) and the
//!    symbol primitives of every cell it instantiates ([`Symbols`]). Out: [`Scene`].
//! 2. How many: up to 2^22 objects per cell (ARCHITECTURE), 10^5 typical at the D39 budget.
//! 3. Widths: coordinates `i32` milli-units; object index `u32`; layer `u8`.
//! 4. Access: render walks each primitive table linearly per frame; hit tests and box
//!    selection walk `objs` linearly per click; marks walk the owner columns once per
//!    selection change. Owner columns are parallel to the render tables (SoA).
//! 5. Lifetime: rebuilt whenever the document's edit epoch, the view or the symbol cache
//!    changes; pan and zoom reuse it untouched.
//! 6. Parallel: instances could be stamped in chunks; at 10^5 objects the serial build is
//!    measured in `tests/perf.rs`.
//!
//! Layers follow xschem (0 background, 1 wire, 2 grid/selection, 3 text, 4 symbol, 5 pin);
//! the app adds [`LAYER_JUNCTION`], [`LAYER_BUS`], [`LAYER_MISSING`], [`LAYER_SEL`].

use std::collections::HashMap;

use schemify_doc::{DocHandle, Document, Project, Strings as DocStrings, Sym, TableId};
use schemify_expr::{Scratch, Template, Value};
use schemify_format::{Orient, RecordKind};
use schemify_render::canvas::{CanvasPrims, Marks, PrimKind};

use crate::geometry::{Pt, Xform, place};
use crate::session::View;

/// Junction dots (filled, wire color).
pub const LAYER_JUNCTION: u8 = 22;
/// Bus wires (wire color, thick).
pub const LAYER_BUS: u8 = 23;
/// Placeholder of an instance whose cell does not resolve.
pub const LAYER_MISSING: u8 = 24;
/// Overlay outlines in the selection color (box select, move preview, text selection).
pub const LAYER_SEL: u8 = 25;

/// One xschem unit in milli-units.
pub const UNIT: i32 = 1000;
/// Canvas text height per unit of a `size` attribute (xschem's size 0.4 ≈ 20 units).
const TEXT_PER_SIZE: f64 = 50_000.0;
/// Half side of a pin square (xschem pin boxes are 5 units wide).
const PIN_HALF: i32 = 2_500;
/// Junction dot radius (xschem `cadhalfdotsize` at snap 10).
const DOT_R: i32 = 3_700;

/// Identifies a selectable object across edits: the table plus the object id for rank-3
/// kinds, or the row for symbol pins and graphics (rows shift only when a pin or graphic
/// is inserted or deleted; the editor clears such selections then).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjKey {
    /// Table of the record.
    pub table: TableId,
    /// `ObjId.0` or row.
    pub key: u32,
}

/// Selectable objects of the scene (SoA): hit and box-select read these, never the doc.
#[derive(Clone, Debug, Default)]
pub struct Objects {
    /// Selection key.
    pub key: Vec<ObjKey>,
    /// Segment endpoints `[x1, y1, x2, y2]` when `seg`, else the bounding box.
    pub geom: Vec<[i32; 4]>,
    /// True for wires and symbol lines (hit by distance to the segment).
    pub seg: Vec<bool>,
}

impl Objects {
    fn push(&mut self, key: ObjKey, geom: [i32; 4], seg: bool) -> u32 {
        assert!(
            self.key.len() < CanvasPrims::CAPACITY,
            "scene object capacity"
        );
        self.key.push(key);
        self.geom.push(geom);
        self.seg.push(seg);
        (self.key.len() - 1) as u32
    }

    /// Number of objects.
    pub fn len(&self) -> usize {
        self.key.len()
    }

    /// True when the scene has no objects.
    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// Normalized bounding box of object `i`.
    pub fn bbox(&self, i: usize) -> [i32; 4] {
        let g = self.geom[i];
        [
            g[0].min(g[2]),
            g[1].min(g[3]),
            g[0].max(g[2]),
            g[1].max(g[3]),
        ]
    }
}

/// Object index of every primitive, parallel to the `CanvasPrims` tables.
#[derive(Clone, Debug, Default)]
pub struct Owners {
    /// Per line.
    pub lines: Vec<u32>,
    /// Per rect.
    pub rects: Vec<u32>,
    /// Per arc.
    pub arcs: Vec<u32>,
    /// Per open polyline.
    pub polylines: Vec<u32>,
    /// Per closed polygon.
    pub polygons: Vec<u32>,
    /// Per text.
    pub texts: Vec<u32>,
}

/// The drawn document and its derived lookup tables.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// Primitives for `render::canvas`.
    pub prims: CanvasPrims,
    /// Owner object of each primitive.
    pub owner: Owners,
    /// Selectable objects.
    pub objs: Objects,
    /// `(key, object index)` sorted by key, for selection lookups.
    pub index: Vec<(ObjKey, u32)>,
    /// Connection points (instance pins, wire ends) and their owner object.
    pub conn: Vec<(Pt, u32)>,
    /// World bounds of everything drawn (`[0; 4]` when empty).
    pub extent: [i32; 4],
    /// What was built: (document, view, document epoch, symbol cache epoch).
    pub built: Option<(DocHandle, View, u32, u32)>,
}

impl Scene {
    /// Object index of `key`, if drawn.
    pub fn find(&self, key: ObjKey) -> Option<u32> {
        self.index
            .binary_search_by(|e| e.0.cmp(&key))
            .ok()
            .map(|i| self.index[i].1)
    }
}

/// One cell's symbol in symbol-local coordinates: what every instance of it stamps.
#[derive(Clone, Debug, Default)]
pub struct SymPrims {
    lines: Vec<([i32; 4], u8)>,
    rects: Vec<([i32; 4], u8)>,
    arcs: Vec<([i32; 5], u8)>,
    polys: Vec<(Vec<Pt>, bool, u8)>,
    texts: Vec<SymText>,
    pins: Vec<Pt>,
    bbox: [i32; 4],
}

#[derive(Clone, Debug)]
struct SymText {
    at: Pt,
    size: i32,
    layer: u8,
    hcenter: bool,
    /// Parsed template; `None` for plain text or a template that does not parse (drawn raw).
    template: Option<Template>,
    raw: String,
}

/// Symbol primitives per indexed cell row, rebuilt when the cell's document changes.
#[derive(Debug, Default)]
pub struct Symbols {
    rows: Vec<Option<(DocHandle, u32, SymPrims)>>,
    /// Advances whenever an entry changes; part of the scene's built key.
    pub epoch: u32,
}

impl Symbols {
    // ponytail: libraries are indexed once per session; when re-indexing lands (file
    // watching), clear this cache with it, since `index_library` invalidates cell rows.

    /// Makes the entry of cell `row` current for document `h` at edit epoch `epoch`.
    pub fn refresh(&mut self, row: usize, h: DocHandle, epoch: u32, doc: &Document) {
        if self.rows.len() <= row {
            self.rows.resize_with(row + 1, || None);
        }
        let fresh = matches!(&self.rows[row], Some((d, e, _)) if *d == h && *e == epoch);
        if !fresh {
            self.rows[row] = Some((h, epoch, sym_prims(doc)));
            self.epoch = self.epoch.wrapping_add(1);
        }
        assert!(self.rows[row].is_some());
    }

    fn get(&self, row: usize) -> Option<&SymPrims> {
        self.rows.get(row)?.as_ref().map(|e| &e.2)
    }
}

fn attr<'d>(doc: &'d Document, span: schemify_doc::Span, key: &str) -> Option<&'d str> {
    let v = doc.pool().find_attr(doc.strings(), span, key)?;
    Some(v.first().map_or("", |s| doc.strings().get(*s)))
}

fn layer_attr(doc: &Document, span: schemify_doc::Span, default: u8) -> u8 {
    attr(doc, span, "layer")
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|&l| l < 22)
        .unwrap_or(default)
}

fn size_of(text: Option<&str>, default: f64) -> i32 {
    let s = text.and_then(|t| t.parse::<f64>().ok()).unwrap_or(default);
    (s.clamp(0.01, 100.0) * TEXT_PER_SIZE) as i32
}

fn degrees_milli(s: &DocStrings, sym: Sym) -> i32 {
    (s.get(sym).parse::<f64>().unwrap_or(0.0) * 1000.0) as i32
}

/// Builds the symbol primitives of a cell document (its pins and rank-2 graphics).
pub fn sym_prims(doc: &Document) -> SymPrims {
    let g = doc.graphics();
    let s = doc.strings();
    let coords = &doc.pool().coords;
    let mut p = SymPrims::default();
    let grow = |b: &mut [i32; 4], x: i32, y: i32| {
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    };
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for i in 0..g.shape.len() {
        let c = &coords[g.coords[i].range()];
        let layer = layer_attr(doc, g.attrs[i], 4);
        match g.shape[i] {
            RecordKind::Line => {
                p.lines.push(([c[0], c[1], c[2], c[3]], layer));
                grow(&mut bbox, c[0], c[1]);
                grow(&mut bbox, c[2], c[3]);
            }
            RecordKind::Rect => {
                p.rects.push(([c[0], c[1], c[2], c[3]], layer));
                grow(&mut bbox, c[0], c[1]);
                grow(&mut bbox, c[2], c[3]);
            }
            RecordKind::Arc | RecordKind::Circle => {
                let (start, sweep) = if g.shape[i] == RecordKind::Arc {
                    (
                        degrees_milli(s, g.angles[i][0]),
                        degrees_milli(s, g.angles[i][1]),
                    )
                } else {
                    (0, 360_000)
                };
                p.arcs
                    .push(([c[0], c[1], c[2].max(0), start, sweep], layer));
                grow(&mut bbox, c[0] - c[2], c[1] - c[2]);
                grow(&mut bbox, c[0] + c[2], c[1] + c[2]);
            }
            RecordKind::Poly => {
                let pts: Vec<Pt> = c.chunks_exact(2).map(|q| (q[0], q[1])).collect();
                for q in &pts {
                    grow(&mut bbox, q.0, q.1);
                }
                let closed = attr(doc, g.attrs[i], "closed") == Some("true");
                if pts.len() >= 2 {
                    p.polys.push((pts, closed, layer));
                }
            }
            RecordKind::Stext => {
                let raw = s.get(g.text[i]).to_owned();
                let template = raw
                    .contains('{')
                    .then(|| Template::parse(&raw).ok())
                    .flatten();
                p.texts.push(SymText {
                    at: (c[0], c[1]),
                    size: size_of(attr(doc, g.attrs[i], "size"), 0.3),
                    layer: layer_attr(doc, g.attrs[i], 3),
                    hcenter: attr(doc, g.attrs[i], "hcenter").is_some(),
                    template,
                    raw,
                });
                grow(&mut bbox, c[0], c[1]);
            }
            k => unreachable!("{k:?} is not a graphic"),
        }
    }
    let pins = doc.pins();
    for i in 0..pins.x.len() {
        p.pins.push((pins.x[i], pins.y[i]));
        grow(&mut bbox, pins.x[i] - PIN_HALF, pins.y[i] - PIN_HALF);
        grow(&mut bbox, pins.x[i] + PIN_HALF, pins.y[i] + PIN_HALF);
    }
    if bbox[0] > bbox[2] {
        bbox = [-5 * UNIT, -5 * UNIT, 5 * UNIT, 5 * UNIT];
    }
    p.bbox = bbox;
    p
}

/// Approximate text box: 0.6 em per character (the renderer shapes the real glyphs).
fn text_size(size: i32, text: &str) -> (i32, i32) {
    let n = text.chars().count().max(1) as f64;
    ((f64::from(size) * 0.6 * n) as i32, size)
}

/// Scene builder state: the scene being filled plus the extent accumulator.
struct Build<'a> {
    sc: &'a mut Scene,
    ext: [i32; 4],
}

impl Build<'_> {
    fn grow(&mut self, x: i32, y: i32) {
        self.ext[0] = self.ext[0].min(x);
        self.ext[1] = self.ext[1].min(y);
        self.ext[2] = self.ext[2].max(x);
        self.ext[3] = self.ext[3].max(y);
    }
    fn line(&mut self, a: Pt, b: Pt, layer: u8, o: u32) {
        self.sc.prims.line(a.0, a.1, b.0, b.1, layer);
        self.sc.owner.lines.push(o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    fn rect(&mut self, a: Pt, b: Pt, layer: u8, o: u32) {
        self.sc.prims.rect(a.0, a.1, b.0, b.1, layer);
        self.sc.owner.rects.push(o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    fn arc(&mut self, c: Pt, r: i32, start: i32, sweep: i32, layer: u8, o: u32) {
        self.sc.prims.arc(c.0, c.1, r, start, sweep, layer);
        self.sc.owner.arcs.push(o);
        self.grow(c.0 - r, c.1 - r);
        self.grow(c.0 + r, c.1 + r);
    }
    fn poly(&mut self, pts: &[Pt], closed: bool, layer: u8, o: u32) {
        self.sc.prims.poly(pts, closed, layer);
        if closed {
            self.sc.owner.polygons.push(o);
        } else {
            self.sc.owner.polylines.push(o);
        }
        for p in pts {
            self.grow(p.0, p.1);
        }
    }
    /// Text with its top-left at `at`; returns its box.
    fn text(&mut self, at: Pt, size: i32, layer: u8, s: &str, o: u32) -> [i32; 4] {
        if s.is_empty() || size <= 0 {
            return [at.0, at.1, at.0, at.1];
        }
        self.sc.prims.text(at.0, at.1, size, layer, s);
        self.sc.owner.texts.push(o);
        let (w, h) = text_size(size, s);
        self.grow(at.0, at.1);
        self.grow(at.0 + w, at.1 + h);
        [at.0, at.1, at.0 + w, at.1 + h]
    }
}

/// Rebuilds `sc` for document `h` shown as `view`. `symbols` must hold every cell the
/// document instantiates that resolves (see `Editor::load_symbols`); unresolved cells draw
/// as a red placeholder with the cell reference.
///
/// Cost: O(objects + stamped symbol primitives); reuses `sc`'s allocations.
pub fn build(
    sc: &mut Scene,
    project: &Project,
    h: DocHandle,
    view: View,
    symbols: &Symbols,
    epoch: u32,
) {
    let Some(doc) = project.doc(h) else {
        *sc = Scene::default();
        return;
    };
    sc.prims = CanvasPrims::default();
    sc.owner = Owners::default();
    sc.objs = Objects::default();
    sc.index.clear();
    sc.conn.clear();
    let mut b = Build {
        sc,
        ext: [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
    };
    match view {
        View::Schematic => schematic(&mut b, project, h, doc, symbols),
        View::Symbol => symbol(&mut b, doc),
    }
    let ext = if b.ext[0] > b.ext[2] { [0; 4] } else { b.ext };
    sc.extent = ext;
    fit_boxes(sc);
    sc.index = sc
        .objs
        .key
        .iter()
        .enumerate()
        .map(|(i, &k)| (k, i as u32))
        .collect();
    sc.index.sort_unstable();
    sc.built = Some((h, view, epoch, symbols.epoch));
    let p = &sc.prims;
    assert_eq!(p.lines.x0.len(), sc.owner.lines.len());
    assert_eq!(p.texts.x.len(), sc.owner.texts.len());
    assert_eq!(p.polygons.end.len(), sc.owner.polygons.len());
}

/// Grows every box object's geometry to cover all of its primitives, so whatever is drawn
/// for an object hits and box-selects as that object.
fn fit_boxes(sc: &mut Scene) {
    let p = &sc.prims;
    let o = &sc.owner;
    let objs = &mut sc.objs;
    let mut grow = |owner: u32, x0: i32, y0: i32, x1: i32, y1: i32| {
        if owner == u32::MAX || objs.seg[owner as usize] {
            return;
        }
        let g = &mut objs.geom[owner as usize];
        *g = [
            g[0].min(x0.min(x1)),
            g[1].min(y0.min(y1)),
            g[2].max(x0.max(x1)),
            g[3].max(y0.max(y1)),
        ];
    };
    for i in 0..p.lines.x0.len() {
        let l = &p.lines;
        grow(o.lines[i], l.x0[i], l.y0[i], l.x1[i], l.y1[i]);
    }
    for i in 0..p.rects.x0.len() {
        let r = &p.rects;
        grow(o.rects[i], r.x0[i], r.y0[i], r.x1[i], r.y1[i]);
    }
    for i in 0..p.arcs.cx.len() {
        let a = &p.arcs;
        grow(
            o.arcs[i],
            a.cx[i] - a.r[i],
            a.cy[i] - a.r[i],
            a.cx[i] + a.r[i],
            a.cy[i] + a.r[i],
        );
    }
    for (polys, owners) in [(&p.polylines, &o.polylines), (&p.polygons, &o.polygons)] {
        let mut start = 0;
        for (k, &end) in polys.end.iter().enumerate() {
            for j in start..end as usize {
                grow(owners[k], polys.x[j], polys.y[j], polys.x[j], polys.y[j]);
            }
            start = end as usize;
        }
    }
    let t = &p.texts;
    let mut start = 0;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        let (w, h) = text_size(t.size[i], &t.text[start..end]);
        grow(o.texts[i], t.x[i], t.y[i], t.x[i] + w, t.y[i] + h);
        start = end;
    }
}

fn schematic(b: &mut Build<'_>, project: &Project, h: DocHandle, doc: &Document, sym: &Symbols) {
    let s = doc.strings();
    // Instances: stamp the resolved cell's symbol through the placement transform.
    let inst = doc.instances();
    let mut inputs: Vec<Value<'_>> = Vec::new();
    let mut scratch = Scratch::default();
    let mut text = String::new();
    let mut resolved: HashMap<Sym, Option<usize>> = HashMap::new();
    for i in 0..inst.id.len() {
        let key = ObjKey {
            table: TableId::Instances,
            key: inst.id[i].0,
        };
        let (o, at) = (inst.orient[i], (inst.x[i], inst.y[i]));
        let cellref = s.get(inst.cell[i]);
        let row = *resolved
            .entry(inst.cell[i])
            .or_insert_with(|| project.resolve(h, cellref).ok());
        let prims = row.and_then(|r| sym.get(r));
        let Some(p) = prims else {
            let obj = b.sc.objs.push(
                key,
                [
                    at.0 - 10 * UNIT,
                    at.1 - 10 * UNIT,
                    at.0 + 10 * UNIT,
                    at.1 + 10 * UNIT,
                ],
                false,
            );
            b.rect(
                (at.0 - 10 * UNIT, at.1 - 10 * UNIT),
                (at.0 + 10 * UNIT, at.1 + 10 * UNIT),
                LAYER_MISSING,
                obj,
            );
            b.text(
                (at.0 - 10 * UNIT, at.1 + 11 * UNIT),
                8 * UNIT,
                LAYER_MISSING,
                cellref,
                obj,
            );
            continue;
        };
        let x = |q: Pt| place(o, at, q);
        let (c0, c1) = (x((p.bbox[0], p.bbox[1])), x((p.bbox[2], p.bbox[3])));
        let obj = b.sc.objs.push(key, [c0.0, c0.1, c1.0, c1.1], false);
        for (l, layer) in &p.lines {
            b.line(x((l[0], l[1])), x((l[2], l[3])), *layer, obj);
        }
        for (r, layer) in &p.rects {
            b.rect(x((r[0], r[1])), x((r[2], r[3])), *layer, obj);
        }
        for (a, layer) in &p.arcs {
            let (start, sweep) = arc_placed(o, a[3], a[4]);
            b.arc(x((a[0], a[1])), a[2], start, sweep, *layer, obj);
        }
        for (pts, closed, layer) in &p.polys {
            let q: Vec<Pt> = pts.iter().map(|&q| x(q)).collect();
            b.poly(&q, *closed, *layer, obj);
        }
        for &pin in &p.pins {
            let q = x(pin);
            b.rect(
                (q.0 - PIN_HALF, q.1 - PIN_HALF),
                (q.0 + PIN_HALF, q.1 + PIN_HALF),
                5,
                obj,
            );
            b.sc.conn.push((q, obj));
        }
        let iface = row.and_then(|r| project.interface(r));
        for t in &p.texts {
            text.clear();
            let rendered = t.template.as_ref().is_some_and(|tpl| {
                let ok = tpl
                    .program()
                    .bind(&mut inputs, |name| bind_ident(name, doc, i, iface, cellref))
                    .is_ok();
                ok && tpl.render(&inputs, &mut scratch, &mut text).is_ok()
            });
            if !rendered {
                text.clear();
                text.push_str(&t.raw);
            }
            let q = x(t.at);
            let (w, _) = text_size(t.size, &text);
            let left = if t.hcenter { q.0 - w / 2 } else { q.0 };
            b.text((left, q.1), t.size, t.layer, &text, obj);
        }
    }
    // Wires.
    let w = doc.wires();
    for i in 0..w.id.len() {
        let (a, c) = ((w.x1[i], w.y1[i]), (w.x2[i], w.y2[i]));
        let key = ObjKey {
            table: TableId::Wires,
            key: w.id[i].0,
        };
        let bus = attr(doc, w.attrs[i], "bus").is_some_and(|v| v == "true");
        let obj = b.sc.objs.push(key, [a.0, a.1, c.0, c.1], true);
        b.line(a, c, if bus { LAYER_BUS } else { 1 }, obj);
        b.sc.conn.push((a, obj));
        b.sc.conn.push((c, obj));
    }
    junctions(b, w);
    // Labels: a pin square at the anchor and the net name beside it, in wire color.
    let l = doc.labels();
    for i in 0..l.id.len() {
        let key = ObjKey {
            table: TableId::Labels,
            key: l.id[i].0,
        };
        placed(
            b,
            key,
            (l.x[i], l.y[i]),
            l.orient[i],
            s.get(l.text[i]),
            false,
        );
    }
    let p = doc.ports();
    for i in 0..p.id.len() {
        let key = ObjKey {
            table: TableId::Ports,
            key: p.id[i].0,
        };
        placed(
            b,
            key,
            (p.x[i], p.y[i]),
            p.orient[i],
            s.get(p.text[i]),
            true,
        );
    }
    let n = doc.noconns();
    for i in 0..n.id.len() {
        let key = ObjKey {
            table: TableId::Noconns,
            key: n.id[i].0,
        };
        let (x, y) = (n.x[i], n.y[i]);
        let r = 4 * UNIT;
        let obj = b.sc.objs.push(key, [x - r, y - r, x + r, y + r], false);
        b.line((x - r, y - r), (x + r, y + r), 4, obj);
        b.line((x - r, y + r), (x + r, y - r), 4, obj);
        b.sc.conn.push(((x, y), obj));
    }
    let t = doc.texts();
    for i in 0..t.id.len() {
        let key = ObjKey {
            table: TableId::Texts,
            key: t.id[i].0,
        };
        let size = size_of(attr(doc, t.attrs[i], "size"), 0.4);
        let layer = layer_attr(doc, t.attrs[i], 3);
        let at = (t.x[i], t.y[i]);
        let obj = b.sc.objs.push(key, [at.0, at.1, at.0, at.1], false);
        let bx = b.text(at, size, layer, s.get(t.text[i]), obj);
        b.sc.objs.geom[obj as usize] = bx;
    }
}

/// Binds a template identifier for instance row `i`: `name`, `cell`, then the instance's
/// override of a parameter, then the cell's default (numbers as numbers, the rest as text).
fn bind_ident<'d>(
    name: &str,
    doc: &'d Document,
    i: usize,
    iface: Option<&'d Document>,
    cellref: &'d str,
) -> Option<Value<'d>> {
    let s = doc.strings();
    let inst = doc.instances();
    let text = match name {
        "name" => s.get(inst.name[i]),
        "cell" => cellref.rsplit([':', '/']).next().unwrap_or(cellref),
        _ => {
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

/// Maps a symbol-local arc through an instance orientation (millidegrees).
fn arc_placed(o: Orient, start: i32, sweep: i32) -> (i32, i32) {
    let mut a = start;
    if o.mirrored() {
        a = 180_000 - a - sweep;
    }
    a += 90_000 * i32::from(o.quarter_turns());
    (a.rem_euclid(360_000), sweep)
}

/// A label (`port = false`) or port: pin square or arrow at the anchor and the name beside
/// it, on the side the orientation points to.
fn placed(b: &mut Build<'_>, key: ObjKey, at: Pt, o: Orient, name: &str, port: bool) {
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
        b.poly(&arrow, true, 5, obj);
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
    b.sc.conn.push((at, obj));
}

/// xschem's connection cues: per wire end +1 for every other wire end at the same point
/// and +2 for every wire whose interior it touches; a dot where the count exceeds 1.
// ponytail: axis-aligned interior touches use per-coordinate buckets, diagonal wires are
// scanned linearly; a spatial grid if diagonal-heavy schematics show up in profiles.
fn junctions(b: &mut Build<'_>, w: &schemify_doc::Wires) {
    let n = w.id.len();
    if n < 2 {
        return;
    }
    let mut ends: Vec<Pt> = Vec::with_capacity(2 * n);
    for i in 0..n {
        ends.push((w.x1[i], w.y1[i]));
        ends.push((w.x2[i], w.y2[i]));
    }
    ends.sort_unstable();
    let mut horiz: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
    let mut vert: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
    let mut diag: Vec<usize> = Vec::new();
    for i in 0..n {
        if w.y1[i] == w.y2[i] {
            horiz.entry(w.y1[i]).or_default().push((w.x1[i], w.x2[i]));
        } else if w.x1[i] == w.x2[i] {
            vert.entry(w.x1[i]).or_default().push((w.y1[i], w.y2[i]));
        } else {
            diag.push(i);
        }
    }
    let interior = |p: Pt| -> bool {
        let h = horiz
            .get(&p.1)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.0 && p.0 < c));
        let v = vert
            .get(&p.0)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.1 && p.1 < c));
        let d = diag.iter().any(|&i| {
            let (ax, ay, bx, by) = (
                i64::from(w.x1[i]),
                i64::from(w.y1[i]),
                i64::from(w.x2[i]),
                i64::from(w.y2[i]),
            );
            let (px, py) = (i64::from(p.0), i64::from(p.1));
            let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            let dot = (px - ax) * (bx - ax) + (py - ay) * (by - ay);
            let len2 = (bx - ax).pow(2) + (by - ay).pow(2);
            cross == 0 && dot > 0 && dot < len2
        });
        h || v || d
    };
    let mut i = 0;
    while i < ends.len() {
        let mut j = i;
        while j < ends.len() && ends[j] == ends[i] {
            j += 1;
        }
        let p = ends[i];
        if j - i >= 3 || interior(p) {
            // Dots belong to no object: they are cues, not selectable records.
            b.sc.prims.arc(p.0, p.1, DOT_R, 0, 360_000, LAYER_JUNCTION);
            b.sc.owner.arcs.push(u32::MAX);
        }
        i = j;
    }
}

fn symbol(b: &mut Build<'_>, doc: &Document) {
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
                let (l, layer) = p.lines[li];
                li += 1;
                let obj = b.sc.objs.push(key, l, true);
                b.line((l[0], l[1]), (l[2], l[3]), layer, obj);
            }
            RecordKind::Rect => {
                let (r, layer) = p.rects[ri];
                ri += 1;
                let obj = b.sc.objs.push(key, r, false);
                b.rect((r[0], r[1]), (r[2], r[3]), layer, obj);
            }
            RecordKind::Arc | RecordKind::Circle => {
                let (a, layer) = p.arcs[ai];
                ai += 1;
                let obj = b.sc.objs.push(
                    key,
                    [a[0] - a[2], a[1] - a[2], a[0] + a[2], a[1] + a[2]],
                    false,
                );
                b.arc((a[0], a[1]), a[2], a[3], a[4], layer, obj);
            }
            RecordKind::Poly => {
                if pi >= p.polys.len() {
                    continue;
                }
                let (pts, closed, layer) = &p.polys[pi];
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
                b.poly(pts, *closed, *layer, obj);
            }
            RecordKind::Stext => {
                let t = &p.texts[ti];
                ti += 1;
                let obj = b.sc.objs.push(key, [t.at.0, t.at.1, t.at.0, t.at.1], false);
                let (w, _) = text_size(t.size, &t.raw);
                let left = if t.hcenter { t.at.0 - w / 2 } else { t.at.0 };
                let bx = b.text((left, t.at.1), t.size, t.layer, &t.raw, obj);
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
        b.sc.conn.push((q, obj));
    }
}

/// Returns the object under world point `p` within `tol` milli-units: the nearest segment
/// object within `tol`, else the smallest box containing `p` (inflated by `tol`), so wires
/// win over the instance they cross and small symbols over big ones.
///
/// Cost: O(objects).
pub fn hit(objs: &Objects, p: (f64, f64), tol: f64) -> Option<u32> {
    assert!(tol >= 0.0 && tol.is_finite());
    let mut best_seg: Option<(f64, u32)> = None;
    let mut best_box: Option<(f64, u32)> = None;
    for i in 0..objs.len() {
        let g = objs.geom[i];
        if objs.seg[i] {
            let d = seg_dist(p, g);
            if d <= tol && best_seg.is_none_or(|b| d < b.0) {
                best_seg = Some((d, i as u32));
            }
        } else {
            let b = objs.bbox(i);
            let inside = p.0 >= f64::from(b[0]) - tol
                && p.0 <= f64::from(b[2]) + tol
                && p.1 >= f64::from(b[1]) - tol
                && p.1 <= f64::from(b[3]) + tol;
            let area = (f64::from(b[2]) - f64::from(b[0])) * (f64::from(b[3]) - f64::from(b[1]));
            if inside && best_box.is_none_or(|bb| area < bb.0) {
                best_box = Some((area, i as u32));
            }
        }
    }
    let r = best_seg.or(best_box).map(|b| b.1);
    assert!(r.is_none_or(|i| (i as usize) < objs.len()));
    r
}

fn seg_dist(p: (f64, f64), g: [i32; 4]) -> f64 {
    let (ax, ay, bx, by) = (
        f64::from(g[0]),
        f64::from(g[1]),
        f64::from(g[2]),
        f64::from(g[3]),
    );
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - ax) * dx + (p.1 - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (ax + t * dx - p.0, ay + t * dy - p.1);
    (qx * qx + qy * qy).sqrt()
}

/// Objects selected by a box `[x0, y0, x1, y1]` (any corner order): fully inside it, or,
/// with `touch` (xschem's right-to-left drag), any object whose box meets it.
pub fn box_select(objs: &Objects, r: [f64; 4], touch: bool) -> Vec<u32> {
    let (x0, y0, x1, y1) = (
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    );
    let mut out = Vec::new();
    for i in 0..objs.len() {
        let b = objs.bbox(i).map(f64::from);
        let ok = if touch {
            b[0] <= x1 && b[2] >= x0 && b[1] <= y1 && b[3] >= y0
        } else {
            b[0] >= x0 && b[2] <= x1 && b[1] >= y0 && b[3] <= y1
        };
        if ok {
            out.push(i as u32);
        }
    }
    assert!(out.windows(2).all(|w| w[0] < w[1]));
    out
}

/// Recolors every primitive of a selected object (`selected[obj]`) in `color`.
pub fn marks(sc: &Scene, selected: &[bool], color: u32, out: &mut Marks) {
    assert_eq!(selected.len(), sc.objs.len());
    *out = Marks::default();
    let on = |o: u32| o != u32::MAX && selected[o as usize];
    let tables = [
        (PrimKind::Line, &sc.owner.lines),
        (PrimKind::Rect, &sc.owner.rects),
        (PrimKind::Arc, &sc.owner.arcs),
        (PrimKind::Polyline, &sc.owner.polylines),
        (PrimKind::Polygon, &sc.owner.polygons),
    ];
    for (kind, owners) in tables {
        for (i, &o) in owners.iter().enumerate() {
            if on(o) {
                out.push(kind, i as u32, color);
            }
        }
    }
}

/// Copies the primitives of selected objects through `x` into `out` on [`LAYER_SEL`]
/// (move and copy previews; texts included, so a moving label shows its name).
pub fn push_moved(sc: &Scene, selected: &[bool], x: Xform, out: &mut CanvasPrims) {
    assert_eq!(selected.len(), sc.objs.len());
    let on = |o: u32| o != u32::MAX && selected[o as usize];
    let p = &sc.prims;
    for i in 0..p.lines.x0.len() {
        if on(sc.owner.lines[i]) {
            let (a, c) = (
                x.point((p.lines.x0[i], p.lines.y0[i])),
                x.point((p.lines.x1[i], p.lines.y1[i])),
            );
            out.line(a.0, a.1, c.0, c.1, LAYER_SEL);
        }
    }
    for i in 0..p.rects.x0.len() {
        if on(sc.owner.rects[i]) {
            let (a, c) = (
                x.point((p.rects.x0[i], p.rects.y0[i])),
                x.point((p.rects.x1[i], p.rects.y1[i])),
            );
            out.rect(a.0, a.1, c.0, c.1, LAYER_SEL);
        }
    }
    for i in 0..p.arcs.cx.len() {
        if on(sc.owner.arcs[i]) {
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
    }
    for (polys, owners, closed) in [
        (&p.polylines, &sc.owner.polylines, false),
        (&p.polygons, &sc.owner.polygons, true),
    ] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            if on(owners[k]) {
                let pts: Vec<Pt> = (start..end)
                    .map(|j| x.point((polys.x[j], polys.y[j])))
                    .collect();
                out.poly(&pts, closed, LAYER_SEL);
            }
            start = end;
        }
    }
    let t = &p.texts;
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if on(sc.owner.texts[i]) {
            let a = x.point((t.x[i], t.y[i]));
            out.text(a.0, a.1, t.size[i], LAYER_SEL, &t.text[start..end]);
        }
        start = end;
    }
}

/// Stamps cell `row`'s symbol at `at` with orientation `o` into `out` on [`LAYER_SEL`]
/// (placement preview; texts drawn raw). Returns false when the cell has no cached symbol.
pub fn stamp(symbols: &Symbols, row: usize, o: Orient, at: Pt, out: &mut CanvasPrims) -> bool {
    let Some(p) = symbols.get(row) else {
        return false;
    };
    let x = |q: Pt| place(o, at, q);
    for (l, _) in &p.lines {
        let (a, b) = (x((l[0], l[1])), x((l[2], l[3])));
        out.line(a.0, a.1, b.0, b.1, LAYER_SEL);
    }
    for (r, _) in &p.rects {
        let (a, b) = (x((r[0], r[1])), x((r[2], r[3])));
        out.rect(a.0, a.1, b.0, b.1, LAYER_SEL);
    }
    for (a, _) in &p.arcs {
        let c = x((a[0], a[1]));
        let (s, w) = arc_placed(o, a[3], a[4]);
        out.arc(c.0, c.1, a[2], s, w, LAYER_SEL);
    }
    for (pts, closed, _) in &p.polys {
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
            RecordKind::Wire => out.line(c(1), c(2), c(3), c(4), LAYER_SEL),
            RecordKind::Line => out.line(c(0), c(1), c(2), c(3), LAYER_SEL),
            RecordKind::Rect => out.rect(c(0), c(1), c(2), c(3), LAYER_SEL),
            _ => {
                let Some(a) = crate::geometry::anchor(&r) else {
                    continue;
                };
                let u = 5 * UNIT;
                out.rect(a.0 - u, a.1 - u, a.0 + u, a.1 + u, LAYER_SEL);
                let text = match r.kind {
                    RecordKind::Inst => r.find_attr("name").and_then(|v| v.values.first()).cloned(),
                    RecordKind::Label | RecordKind::Port | RecordKind::Text => {
                        r.fields.get(4).cloned()
                    }
                    _ => None,
                };
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    out.text(a.0 + u, a.1 - 3 * u, 12 * UNIT, LAYER_SEL, &t);
                }
            }
        }
    }
}
