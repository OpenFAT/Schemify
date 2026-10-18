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

use std::collections::{BTreeMap, HashMap};

use schemify_doc::{
    Cursor, DocHandle, Document, ObjId, Project, Strings as DocStrings, Sym, TableId, Write,
};
use schemify_elab::CellState;
use schemify_expr::{Scratch, Template, Value};
use schemify_format::{Orient, RecordKind};
use schemify_render::canvas::{CanvasPrims, LineStyle, Marks, PrimKind};

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
/// Every object has a stamp, unique for the scene's life (its primitive rows carry it), so an
/// update finds an object's rows by binary search however objects were renumbered.
#[derive(Clone, Debug, Default)]
pub struct Objects {
    /// Selection key.
    pub key: Vec<ObjKey>,
    /// Segment endpoints `[x1, y1, x2, y2]` when `seg`, else the bounding box.
    pub geom: Vec<[i32; 4]>,
    /// True for wires and symbol lines (hit by distance to the segment).
    pub seg: Vec<bool>,
    /// The stamp of each object (ascending in creation order, not in object order).
    stamp: Vec<u32>,
    /// The next stamp to give.
    next_stamp: u32,
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
        let s = self.fresh_stamp();
        self.stamp.push(s);
        (self.key.len() - 1) as u32
    }

    /// A stamp no row of the scene carries yet (each object's, and each junction dot's).
    fn fresh_stamp(&mut self) -> u32 {
        let s = self.next_stamp;
        self.next_stamp = s.checked_add(1).expect("stamps outlast 2^32 stampings");
        s
    }

    /// Removes object `o`; the last object takes its index (returned when there was one).
    fn swap_remove(&mut self, o: usize) -> Option<usize> {
        let last = self.key.len() - 1;
        self.key.swap_remove(o);
        self.geom.swap_remove(o);
        self.seg.swap_remove(o);
        self.stamp.swap_remove(o);
        assert_eq!(self.key.len(), self.stamp.len());
        (o != last).then_some(o)
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

/// Object index of every primitive, parallel to the `CanvasPrims` tables (`u32::MAX`: a
/// junction dot, which belongs to no object, or a dead row).
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

impl Owners {
    /// The six columns in [`CanvasPrims::kill`]'s kind order.
    fn cols(&self) -> [&Vec<u32>; 6] {
        [
            &self.lines,
            &self.rects,
            &self.arcs,
            &self.polylines,
            &self.polygons,
            &self.texts,
        ]
    }

    fn cols_mut(&mut self) -> [&mut Vec<u32>; 6] {
        [
            &mut self.lines,
            &mut self.rects,
            &mut self.arcs,
            &mut self.polylines,
            &mut self.polygons,
            &mut self.texts,
        ]
    }
}

/// The drawn document and its derived lookup tables.
///
/// An update ([`update`]) edits the tables in place: a removed object's primitive rows stay
/// as dead rows ([`schemify_render::canvas::LAYER_DEAD`], owner `u32::MAX`) so every other row
/// keeps its index, its connection points stay with owner `u32::MAX`, and new rows append;
/// readers skip both. A [`build`] compacts.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// Primitives for `render::canvas` (dead rows included, see above).
    pub prims: CanvasPrims,
    /// Owner object of each primitive.
    pub owner: Owners,
    /// Selectable objects.
    pub objs: Objects,
    /// Connection points (instance pins, wire ends) and their owner object (`u32::MAX`: a
    /// removed object's, to skip).
    pub conn: Vec<(Pt, u32)>,
    /// World bounds of everything drawn (`[0; 4]` when empty). [`update`] only grows it: after
    /// deletions it may cover more than is drawn until the next [`build`].
    pub extent: [i32; 4],
    /// What was built: (document, view, document epoch, symbol cache epoch).
    pub built: Option<(DocHandle, View, u32, u32)>,
    /// Changes of every [`build`] and [`update`] since the counter was read.
    pub generation: u64,
    /// `(key, object index)`, for selection lookups. Never iterated.
    index: HashMap<ObjKey, u32>,
    /// The stamp of each primitive row (non-decreasing per kind), parallel to `owner`.
    stamps: Owners,
    /// The stamp of each connection point, parallel to `conn`.
    conn_stamp: Vec<u32>,
    /// Where the document's change log stood when the scene was last brought up to date.
    cursor: Option<Cursor>,
    /// Rows killed since [`Scene::clear_edits`] (per kind), or `None` after a build.
    killed: Option<[Vec<u32>; 6]>,
    /// The objects the last [`update`] removed or stamped; `None` after a build.
    touched: Option<Vec<ObjKey>>,
    /// Dead primitive rows (for compaction).
    dead: usize,
    /// Wire ends and runs, for junction dots.
    wiring: Wiring,
}

/// Wire ends and runs (xschem's connection cues): what decides where junction dots go, kept
/// up to date per changed wire.
#[derive(Clone, Debug, Default)]
struct Wiring {
    /// Wire ends by (x, y), with how many ends are there.
    xy: BTreeMap<Pt, u32>,
    /// The same ends as (y, x).
    yx: BTreeMap<Pt, u32>,
    /// Horizontal runs: y → (x min, x max) of each wire on it.
    horiz: HashMap<i32, Vec<(i32, i32)>>,
    /// Vertical runs: x → (y min, y max).
    vert: HashMap<i32, Vec<(i32, i32)>>,
    /// Diagonal wires `[x1, y1, x2, y2]`.
    diag: Vec<[i32; 4]>,
    /// Center of each dot drawn → its arc row.
    dots: BTreeMap<Pt, u32>,
}

impl Wiring {
    /// The wiring of every wire row of `w`.
    fn of(w: &schemify_doc::Wires) -> Wiring {
        let mut out = Wiring::default();
        for i in 0..w.id.len() {
            let p = [w.x1[i], w.y1[i], w.x2[i], w.y2[i]];
            out.ends(p, true);
            out.runs(p, true);
        }
        out
    }

    /// Adds (`add`) or removes the two ends of wire `w`.
    fn ends(&mut self, w: [i32; 4], add: bool) {
        for p in [(w[0], w[1]), (w[2], w[3])] {
            for (map, k) in [(&mut self.xy, p), (&mut self.yx, (p.1, p.0))] {
                if add {
                    *map.entry(k).or_insert(0) += 1;
                    continue;
                }
                let n = map.get_mut(&k).expect("a drawn wire end");
                *n -= 1;
                if *n == 0 {
                    map.remove(&k);
                }
            }
        }
        debug_assert_eq!(self.xy.len(), self.yx.len());
    }

    /// Adds (`add`) or removes the run of wire `w` from the per-row/column maps.
    fn runs(&mut self, w: [i32; 4], add: bool) {
        let (map, key, run) = if w[1] == w[3] {
            (&mut self.horiz, w[1], (w[0].min(w[2]), w[0].max(w[2])))
        } else if w[0] == w[2] {
            (&mut self.vert, w[0], (w[1].min(w[3]), w[1].max(w[3])))
        } else {
            if add {
                self.diag.push(w);
            } else {
                let i = self
                    .diag
                    .iter()
                    .position(|d| *d == w)
                    .expect("a drawn wire");
                self.diag.swap_remove(i);
            }
            return;
        };
        let runs = map.entry(key).or_default();
        if add {
            runs.push(run);
        } else {
            let i = runs.iter().position(|r| *r == run).expect("a drawn run");
            runs.swap_remove(i);
        }
    }

    fn add(&mut self, w: [i32; 4]) {
        self.ends(w, true);
        self.runs(w, true);
    }

    fn remove(&mut self, w: [i32; 4]) {
        self.ends(w, false);
        self.runs(w, false);
    }

    /// Wire ends at `p`.
    fn ends_at(&self, p: Pt) -> usize {
        self.xy.get(&p).map_or(0, |&n| n as usize)
    }

    /// Whether `p` lies strictly inside some wire.
    fn interior(&self, p: Pt) -> bool {
        let h = self
            .horiz
            .get(&p.1)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.0 && p.0 < c));
        let v = self
            .vert
            .get(&p.0)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.1 && p.1 < c));
        let d = self.diag.iter().any(|w| {
            let (ax, ay, bx, by) = (
                i64::from(w[0]),
                i64::from(w[1]),
                i64::from(w[2]),
                i64::from(w[3]),
            );
            let (px, py) = (i64::from(p.0), i64::from(p.1));
            let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            let dot = (px - ax) * (bx - ax) + (py - ay) * (by - ay);
            let len2 = (bx - ax).pow(2) + (by - ay).pow(2);
            cross == 0 && dot > 0 && dot < len2
        });
        h || v || d
    }

    /// xschem's rule: a dot at a wire end where three or more ends meet, or one touches
    /// another wire's interior.
    fn dot_at(&self, p: Pt) -> bool {
        let n = self.ends_at(p);
        n >= 3 || (n >= 1 && self.interior(p))
    }

    /// Wire ends lying strictly inside wire `w` (where `w` decides their dots), plus `w`'s
    /// own ends: every point whose dot `w` can change.
    fn touched(&self, w: [i32; 4], out: &mut Vec<Pt>) {
        use std::ops::Bound::Excluded;
        out.extend([(w[0], w[1]), (w[2], w[3])]);
        if w[1] == w[3] {
            let (a, c) = (w[0].min(w[2]), w[0].max(w[2]));
            let run = self.yx.range((Excluded((w[1], a)), Excluded((w[1], c))));
            out.extend(run.map(|(q, _)| (q.1, q.0)));
        } else if w[0] == w[2] {
            let (a, c) = (w[1].min(w[3]), w[1].max(w[3]));
            let run = self.xy.range((Excluded((w[0], a)), Excluded((w[0], c))));
            out.extend(run.map(|(q, _)| *q));
        } else {
            // ponytail: a diagonal wire checks every end, O(ends); a 2-D index if
            // diagonal-heavy schematics show up in profiles.
            let mut probe = Wiring::default();
            probe.diag.push(w);
            out.extend(self.xy.keys().filter(|q| probe.interior(**q)));
        }
    }
}

impl Scene {
    /// Object index of `key`, if drawn.
    pub fn find(&self, key: ObjKey) -> Option<u32> {
        let o = self.index.get(&key).copied();
        debug_assert!(o.is_none_or(|o| self.objs.key[o as usize] == key));
        o
    }

    /// The rows [`update`]s killed since the last [`Scene::clear_edits`], per kind in
    /// [`CanvasPrims::kill`] order: with the rows appended since, every change of `prims` in
    /// that time (a canvas diffs them as `TableChange::Rows`). `None` when a [`build`] made
    /// the tables anew in that time.
    pub fn edits(&self) -> Option<&[Vec<u32>; 6]> {
        self.killed.as_ref()
    }

    /// Starts the next [`Scene::edits`] window (the canvas has taken the edits so far).
    pub fn clear_edits(&mut self) {
        let k = self.killed.get_or_insert_with(Default::default);
        k.iter_mut().for_each(Vec::clear);
        assert!(self.edits().is_some_and(|k| k.iter().all(Vec::is_empty)));
    }

    /// The objects the last [`update`] removed or stamped (sorted, a key may no longer be
    /// drawn), or `None` when the last change was a [`build`]: what a derived table keyed by
    /// object follows when [`Scene::generation`] moved by exactly one.
    pub fn touched(&self) -> Option<&[ObjKey]> {
        self.touched.as_deref()
    }

    /// The connection points of drawn objects.
    pub fn conn_points(&self) -> impl Iterator<Item = (Pt, u32)> + '_ {
        self.conn.iter().copied().filter(|c| c.1 != u32::MAX)
    }
}

/// One cell's symbol in symbol-local coordinates: what every instance of it stamps.
#[derive(Clone, Debug, Default)]
pub struct SymPrims {
    lines: Vec<([i32; 4], u8, LineStyle)>,
    rects: Vec<([i32; 4], u8, LineStyle)>,
    arcs: Vec<([i32; 5], u8, LineStyle)>,
    polys: Vec<(Vec<Pt>, bool, u8, LineStyle)>,
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
    /// The record's orientation: the text turns like a `text` record's (format spec §4).
    orient: Orient,
    /// Parsed template; `None` for plain text or a template that does not parse (drawn raw).
    template: Option<Template>,
    raw: String,
}

/// Symbol primitives per indexed cell row, rebuilt when the cell's document changes.
#[derive(Debug, Default)]
pub struct Symbols {
    /// Per row: the open document drawn (`None`: the indexed symbol) and its edit epoch.
    rows: Vec<Option<(Option<DocHandle>, u32, SymPrims)>>,
    /// Advances whenever an entry changes; part of the scene's built key.
    pub epoch: u32,
}

impl Symbols {
    // ponytail: libraries are indexed once per session; when re-indexing lands (file
    // watching), clear this cache with it, since `index_library` invalidates cell rows.

    /// Makes the entry of cell `row` current for `doc`: open document `h` at edit epoch
    /// `epoch`, or the indexed symbol (`h == None`, epoch 0). Rebuilds the primitives and
    /// advances [`Symbols::epoch`] only when that source changed.
    pub fn refresh(&mut self, row: usize, h: Option<DocHandle>, epoch: u32, doc: &Document) {
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

/// The `width`, `dash` and `fill` attributes of a graphic (format spec §4, stroke attributes):
/// a value that is not a number reads as absent; the rest rounds and clamps in
/// [`LineStyle::new`]. `fill true` fills a closed shape; absent or `false` leaves it to the
/// layer.
fn style_attr(doc: &Document, span: schemify_doc::Span) -> LineStyle {
    let n = |key: &str, absent: u8| {
        attr(doc, span, key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map_or(absent, |v| v.round().clamp(0.0, 255.0) as u8)
    };
    let st = LineStyle::new(n("width", 1), n("dash", 0));
    if attr(doc, span, "fill") == Some("true") {
        st.filled()
    } else {
        st
    }
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
        let style = style_attr(doc, g.attrs[i]);
        match g.shape[i] {
            RecordKind::Line => {
                p.lines.push(([c[0], c[1], c[2], c[3]], layer, style));
                grow(&mut bbox, c[0], c[1]);
                grow(&mut bbox, c[2], c[3]);
            }
            RecordKind::Rect => {
                p.rects.push(([c[0], c[1], c[2], c[3]], layer, style));
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
                    .push(([c[0], c[1], c[2].max(0), start, sweep], layer, style));
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
                    p.polys.push((pts, closed, layer, style));
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
                    orient: g.orient[i],
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

/// The far corner of a `(w, h)` text box from its anchor after `rot` clockwise quarter turns.
fn rotated_extent((w, h): (i32, i32), rot: u8) -> (i32, i32) {
    match rot % 4 {
        0 => (w, h),
        1 => (-h, w),
        2 => (-w, -h),
        _ => (h, -w),
    }
}

/// Where a symbol text turned `rot` quarter turns with its anchor at `q` starts: the anchor;
/// for `hcenter` half its width back along its direction; under a mirror (`mirrored`, the
/// composed orientation's) its whole width back, so the text ends at the anchor and a label
/// column right of the body stays clear of the mirrored body (format spec §4, `stext`).
fn sym_text_at(q: Pt, t: &SymText, text: &str, rot: u8, mirrored: bool) -> Pt {
    if !t.hcenter && !mirrored {
        return q;
    }
    let (w, _) = text_size(t.size, text);
    // ponytail: right alignment uses the 0.6 em/char estimate, so a mirrored column is
    // ragged by the estimate's error; shape in the scene if that shows.
    let back = if t.hcenter { w / 2 } else { w };
    let (dx, dy) = rotated_extent((-back, 0), rot);
    assert!(back >= 0);
    (q.0 + dx, q.1 + dy)
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
    /// Records the owner and stamp of a row just pushed to kind `kind` (`o == u32::MAX`: a
    /// junction dot, which takes a stamp of its own).
    fn owned(&mut self, kind: usize, o: u32) {
        let s = match o {
            u32::MAX => self.sc.objs.fresh_stamp(),
            o => self.sc.objs.stamp[o as usize],
        };
        let stamps = self.sc.stamps.cols_mut();
        debug_assert!(stamps[kind].last().is_none_or(|&l| l <= s), "stamps ascend");
        stamps[kind].push(s);
        self.sc.owner.cols_mut()[kind].push(o);
    }
    /// A connection point of object `o`.
    fn conn_at(&mut self, p: Pt, o: u32) {
        self.sc.conn.push((p, o));
        self.sc.conn_stamp.push(self.sc.objs.stamp[o as usize]);
    }
    fn line(&mut self, a: Pt, b: Pt, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.line_styled(a.0, a.1, b.0, b.1, layer, style);
        self.owned(0, o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    fn rect(&mut self, a: Pt, b: Pt, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.rect_styled(a.0, a.1, b.0, b.1, layer, style);
        self.owned(1, o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    #[allow(clippy::too_many_arguments)]
    fn arc(&mut self, c: Pt, r: i32, start: i32, sweep: i32, layer: u8, style: LineStyle, o: u32) {
        self.sc
            .prims
            .arc_styled(c.0, c.1, r, start, sweep, layer, style);
        self.owned(2, o);
        self.grow(c.0 - r, c.1 - r);
        self.grow(c.0 + r, c.1 + r);
    }
    fn poly(&mut self, pts: &[Pt], closed: bool, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.poly_styled(pts, closed, layer, style);
        self.owned(if closed { 4 } else { 3 }, o);
        for p in pts {
            self.grow(p.0, p.1);
        }
    }
    /// Text with its top-left at `at`; returns its box.
    fn text(&mut self, at: Pt, size: i32, layer: u8, s: &str, o: u32) -> [i32; 4] {
        self.text_rot(at, size, layer, s, 0, o)
    }
    /// Text turned `rot` quarter turns clockwise about `at`; returns its box.
    fn text_rot(&mut self, at: Pt, size: i32, layer: u8, s: &str, rot: u8, o: u32) -> [i32; 4] {
        if s.is_empty() || size <= 0 {
            return [at.0, at.1, at.0, at.1];
        }
        self.sc.prims.text_rot(at.0, at.1, size, layer, s, rot);
        self.owned(5, o);
        let (dx, dy) = rotated_extent(text_size(size, s), rot);
        let (q0, q1) = ((at.0, at.1), (at.0 + dx, at.1 + dy));
        self.grow(q0.0, q0.1);
        self.grow(q1.0, q1.1);
        [
            q0.0.min(q1.0),
            q0.1.min(q1.1),
            q0.0.max(q1.0),
            q0.1.max(q1.1),
        ]
    }
}

/// Rebuilds `sc` for document `h` shown as `view`. `symbols` must hold every cell the
/// document instantiates that resolves (see `Editor::load_symbols`); unresolved cells draw
/// as a red placeholder with the cell reference. `values`, when given, must be the elab
/// state of `h` at this epoch: symbol texts (`{W}`) then show evaluated param values (an
/// override `W = W_in` reads the var's value, D19) instead of the override text. The result
/// has no dead rows; [`Scene::edits`] and [`Scene::touched`] are `None` until the next
/// [`update`].
///
/// Cost: O(objects + stamped symbol primitives); reuses `sc`'s allocations.
pub fn build(
    sc: &mut Scene,
    project: &Project,
    h: DocHandle,
    view: View,
    symbols: &Symbols,
    epoch: u32,
    values: Option<&CellState>,
) {
    let generation = sc.generation.wrapping_add(1);
    let Some(doc) = project.doc(h) else {
        *sc = Scene::default();
        sc.generation = generation;
        return;
    };
    sc.prims = CanvasPrims::default();
    sc.owner = Owners::default();
    sc.stamps = Owners::default();
    sc.objs = Objects::default();
    sc.index.clear();
    sc.conn.clear();
    sc.conn_stamp.clear();
    sc.wiring = Wiring::default();
    (sc.killed, sc.touched, sc.dead) = (None, None, 0);
    sc.cursor = (view == View::Schematic).then(|| doc.cursor());
    let mut b = Build {
        sc,
        ext: [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
    };
    let values = values.filter(|v| v.inst_iface.len() == doc.instances().id.len());
    match view {
        View::Schematic => {
            let mut st = Stamp::default();
            for i in 0..doc.instances().id.len() {
                instance(&mut b, project, h, doc, symbols, values, i, &mut st);
            }
            for i in 0..doc.wires().id.len() {
                wire(&mut b, doc, i);
            }
            b.sc.wiring = Wiring::of(doc.wires());
            let ends: Vec<Pt> = b.sc.wiring.xy.keys().copied().collect();
            redot(&mut b, &ends);
            other_objects(&mut b, doc, 0..usize::MAX);
        }
        View::Symbol => symbol(&mut b, doc),
    }
    let ext = if b.ext[0] > b.ext[2] { [0; 4] } else { b.ext };
    sc.extent = ext;
    fit_boxes(sc, Counts::default());
    sc.index = sc
        .objs
        .key
        .iter()
        .enumerate()
        .map(|(i, &k)| (k, i as u32))
        .collect();
    sc.built = Some((h, view, epoch, symbols.epoch));
    sc.generation = generation;
    check_tables(sc);
}

/// Tables a schematic scene draws objects of.
fn drawn(t: TableId) -> bool {
    matches!(
        t,
        TableId::Instances
            | TableId::Wires
            | TableId::Labels
            | TableId::Ports
            | TableId::Noconns
            | TableId::Texts
            | TableId::Shapes
    )
}

/// Brings a schematic scene built for document `h` up to date with the document's current
/// rows (`epoch`), from the document's change log: every object a logged write touched is
/// removed and, while the document holds it, stamped again; junction dots are recomputed only
/// where a changed wire can move them. Same contract as [`build`] (whose result it equals as a
/// set of live primitives per object, up to row order); falls back to it, returning false,
/// when the scene was built for another document, view or symbol cache, when the log no longer
/// reaches the scene's last update, when most of the document changed, or when dead rows
/// pass a quarter of the tables (the build compacts them). Values (`{W}` texts) are taken as
/// unchanged except on the objects touched: when they changed elsewhere, call [`build`].
///
/// Cost: O(touched objects · (log rows + their primitives)), plus the index inserts.
pub fn update(
    sc: &mut Scene,
    project: &Project,
    h: DocHandle,
    symbols: &Symbols,
    epoch: u32,
    values: Option<&CellState>,
) -> bool {
    let Some(doc) = project.doc(h) else {
        return false;
    };
    let current =
        matches!(sc.built, Some((d, View::Schematic, _, s)) if d == h && s == symbols.epoch);
    let changes = sc
        .cursor
        .filter(|_| current)
        .and_then(|c| doc.changes_since(c));
    let Some(changes) = changes else {
        build(sc, project, h, View::Schematic, symbols, epoch, values);
        return false;
    };
    let values = values.filter(|v| v.inst_iface.len() == doc.instances().id.len());
    // The objects the writes touched (a moved row's record did not change).
    let mut keys: Vec<ObjKey> = changes
        .iter()
        .filter(|c| c.what != Write::Moved && drawn(c.table) && c.id != ObjId::NONE)
        .map(|c| ObjKey {
            table: c.table,
            key: c.id.0,
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let total = sc.objs.len().max(1);
    if keys.len() > total / 8 + 64 || sc.dead * 4 > sc.prims.len() + (1 << 14) {
        // ponytail: most of the document changed (a paste, an import), or a quarter of the
        // rows are dead: one full build is cheaper than this many removals, and compacts.
        build(sc, project, h, View::Schematic, symbols, epoch, values);
        return false;
    }
    // Junction points the changed wires can affect, while the old wires are still indexed;
    // then the old objects go.
    let mut points: Vec<Pt> = Vec::new();
    for &key in &keys {
        let Some(o) = sc.find(key) else { continue };
        if key.table == TableId::Wires {
            let w = sc.objs.geom[o as usize];
            sc.wiring.touched(w, &mut points);
            sc.wiring.remove(w);
        }
        remove_object(sc, key);
    }
    // Stamp what the document holds now, then index it.
    let (objs0, prims0) = (sc.objs.len(), Counts::of(sc));
    let ext = match sc.extent {
        [0, 0, 0, 0] => [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
        e => e,
    };
    let mut b = Build { ext, sc };
    let mut st = Stamp::default();
    let mut wires: Vec<[i32; 4]> = Vec::new();
    for &key in &keys {
        let Some(loc) = doc.find_id(ObjId(key.key)).filter(|l| l.table == key.table) else {
            continue; // deleted
        };
        let i = loc.row as usize;
        match key.table {
            TableId::Instances => instance(&mut b, project, h, doc, symbols, values, i, &mut st),
            TableId::Wires => {
                wire(&mut b, doc, i);
                let w = doc.wires();
                let p = [w.x1[i], w.y1[i], w.x2[i], w.y2[i]];
                b.sc.wiring.add(p);
                wires.push(p);
            }
            t => one_other(&mut b, doc, t, i),
        }
    }
    for &w in &wires {
        b.sc.wiring.touched(w, &mut points);
    }
    points.sort_unstable();
    points.dedup();
    redot(&mut b, &points);
    let ext = b.ext;
    sc.extent = if ext[0] > ext[2] { [0; 4] } else { ext };
    fit_boxes(sc, prims0);
    for o in objs0..sc.objs.len() {
        let prev = sc.index.insert(sc.objs.key[o], o as u32);
        assert!(prev.is_none(), "one object per key");
    }
    sc.built = Some((h, View::Schematic, epoch, symbols.epoch));
    sc.cursor = Some(doc.cursor());
    sc.touched = Some(keys);
    sc.generation = sc.generation.wrapping_add(1);
    check_tables(sc);
    true
}

/// Parallel-column invariants of a built scene.
fn check_tables(sc: &Scene) {
    let (p, o) = (&sc.prims, &sc.owner);
    assert_eq!(p.lines.x0.len(), o.lines.len());
    assert_eq!(p.rects.x0.len(), o.rects.len());
    assert_eq!(p.arcs.cx.len(), o.arcs.len());
    assert_eq!(p.polylines.end.len(), o.polylines.len());
    assert_eq!(p.polygons.end.len(), o.polygons.len());
    assert_eq!(p.texts.x.len(), o.texts.len());
    assert_eq!(
        p.texts.rot.len(),
        p.texts.x.len(),
        "rotation column in step"
    );
    for (s, w) in sc.stamps.cols().into_iter().zip(o.cols()) {
        assert_eq!(s.len(), w.len(), "a stamp per row");
    }
    assert_eq!(
        sc.conn.len(),
        sc.conn_stamp.len(),
        "a stamp per connection point"
    );
    assert_eq!(sc.index.len(), sc.objs.len(), "one index entry per object");
    assert!(sc.dead <= p.len());
}

/// Primitive counts per table (where an update's new primitives start).
#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    lines: usize,
    rects: usize,
    arcs: usize,
    polylines: usize,
    polygons: usize,
    texts: usize,
}

impl Counts {
    fn of(sc: &Scene) -> Counts {
        let o = &sc.owner;
        Counts {
            lines: o.lines.len(),
            rects: o.rects.len(),
            arcs: o.arcs.len(),
            polylines: o.polylines.len(),
            polygons: o.polygons.len(),
            texts: o.texts.len(),
        }
    }
}

/// The rows (or connection points) carrying stamp `s` in a non-decreasing stamp column.
fn stamped(col: &[u32], s: u32) -> std::ops::Range<usize> {
    let lo = col.partition_point(|&x| x < s);
    let hi = lo + col[lo..].partition_point(|&x| x == s);
    assert!(hi <= col.len());
    lo..hi
}

/// Removes object `key` (when drawn): its primitive rows die in place (reported through
/// [`Scene::edits`]), its connection points lose their owner, its index entry goes; the last
/// object takes its index (swap-remove), so that object's rows and points are renumbered.
///
/// Cost: O(log rows) per kind plus the rows of the two objects involved.
fn remove_object(sc: &mut Scene, key: ObjKey) {
    let Some(o) = sc.index.remove(&key) else {
        return;
    };
    let s = sc.objs.stamp[o as usize];
    for kind in 0..6 {
        let rows = stamped(sc.stamps.cols()[kind], s);
        for r in rows.clone() {
            assert_eq!(sc.owner.cols()[kind][r], o, "the object's own rows");
            sc.prims.kill(kind as u8, r);
            sc.owner.cols_mut()[kind][r] = u32::MAX;
        }
        // After a build nobody has taken yet (`None`), the edits are unknown anyway.
        if let Some(k) = sc.killed.as_mut() {
            k[kind].extend(rows.clone().map(|r| r as u32));
        }
        sc.dead += rows.len();
    }
    for c in stamped(&sc.conn_stamp, s) {
        sc.conn[c].1 = u32::MAX;
    }
    // The last object moves into `o`'s slot.
    if let Some(slot) = sc.objs.swap_remove(o as usize) {
        let (moved, ms) = (sc.objs.key[slot], sc.objs.stamp[slot]);
        sc.index.insert(moved, o);
        let last = sc.objs.len() as u32;
        for kind in 0..6 {
            for r in stamped(sc.stamps.cols()[kind], ms) {
                let w = &mut sc.owner.cols_mut()[kind][r];
                assert_eq!(*w, last, "the moved object's rows");
                *w = o;
            }
        }
        for c in stamped(&sc.conn_stamp, ms) {
            sc.conn[c].1 = o;
        }
    }
    assert_eq!(sc.index.len(), sc.objs.len());
}

/// Grows every box object's geometry to cover all of its primitives from `from` on, so
/// whatever is drawn for an object hits and box-selects as that object.
fn fit_boxes(sc: &mut Scene, from: Counts) {
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
    for i in from.lines..p.lines.x0.len() {
        let l = &p.lines;
        grow(o.lines[i], l.x0[i], l.y0[i], l.x1[i], l.y1[i]);
    }
    for i in from.rects..p.rects.x0.len() {
        let r = &p.rects;
        grow(o.rects[i], r.x0[i], r.y0[i], r.x1[i], r.y1[i]);
    }
    for i in from.arcs..p.arcs.cx.len() {
        let a = &p.arcs;
        grow(
            o.arcs[i],
            a.cx[i] - a.r[i],
            a.cy[i] - a.r[i],
            a.cx[i] + a.r[i],
            a.cy[i] + a.r[i],
        );
    }
    for (polys, owners, k0) in [
        (&p.polylines, &o.polylines, from.polylines),
        (&p.polygons, &o.polygons, from.polygons),
    ] {
        for (k, (&end, &owner)) in polys.end.iter().zip(owners).enumerate().skip(k0) {
            let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
            for j in start..end as usize {
                grow(owner, polys.x[j], polys.y[j], polys.x[j], polys.y[j]);
            }
        }
    }
    let t = &p.texts;
    for i in from.texts..t.x.len() {
        let start = if i == 0 { 0 } else { t.end[i - 1] as usize };
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let (w, h) = rotated_extent(text_size(t.size[i], &t.text[start..t.end[i] as usize]), rot);
        grow(o.texts[i], t.x[i], t.y[i], t.x[i] + w, t.y[i] + h);
    }
}

/// Reusable buffers of instance stamping (resolved cells, template inputs, text).
#[derive(Default)]
struct Stamp<'d> {
    resolved: HashMap<Sym, Option<usize>>,
    inputs: Vec<Value<'d>>,
    scratch: Scratch,
    text: String,
}

/// Stamps instance row `i`: the resolved cell's symbol through the placement transform, or
/// a red placeholder with the cell reference when it does not resolve.
#[allow(clippy::too_many_arguments)]
fn instance<'d>(
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
        let Stamp {
            inputs,
            scratch,
            text,
            ..
        } = &mut *st;
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
        let at = sym_text_at(x(t.at), t, text, rot, turned.mirrored());
        b.text_rot(at, t.size, t.layer, text, rot, obj);
    }
}

/// Draws wire row `i` (its connection points included; dots are [`redot`]'s).
fn wire(b: &mut Build<'_>, doc: &Document, i: usize) {
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
fn other_objects(b: &mut Build<'_>, doc: &Document, rows: std::ops::Range<usize>) {
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
fn one_other(b: &mut Build<'_>, doc: &Document, table: TableId, i: usize) {
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
fn shape(b: &mut Build<'_>, doc: &Document, i: usize) {
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
fn bind_ident<'d>(
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
fn param_value<'c>(st: &'c CellState, i: usize, name: &str) -> Option<Value<'c>> {
    let iface = st.ifaces.get(*st.inst_iface.get(i)? as usize)?;
    let n = st.names.find(name)?;
    let k = iface.param_name.iter().position(|&p| p == n)?;
    let node = st.graph.inst_param(i, k)?;
    st.graph.vals.get(node as usize)?.value()
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
fn redot(b: &mut Build<'_>, points: &[Pt]) {
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

/// Returns the object under world point `p` within `tol` milli-units: the nearest segment
/// object within `tol`, else the smallest box containing `p` (inflated by `tol`), so wires
/// win over the instance they cross and small symbols over big ones.
///
/// Cost: one integer pass over `objs.geom` (the box each object's hit area fits in, inflated
/// by `tol`), then the exact distance or box test on the few objects that pass it; candidates
/// are tested in ascending order, so ties resolve as a full scan resolves them.
pub fn hit(objs: &Objects, p: (f64, f64), tol: f64) -> Option<u32> {
    assert!(tol >= 0.0 && tol.is_finite());
    assert_eq!(objs.geom.len(), objs.seg.len());
    // Whole milli-units around `p`: an object whose box misses this box is farther than `tol`
    // (its coordinates are integers, so rounding outwards keeps every hit).
    // Clamped to i32, the comparisons below equal the i64 ones (every coordinate is an i32).
    let lim = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (x0, x1) = (lim((p.0 - tol).floor()), lim((p.0 + tol).ceil()));
    let (y0, y1) = (lim((p.1 - tol).floor()), lim((p.1 + tol).ceil()));
    let mut best_seg: Option<(f64, u32)> = None;
    let mut best_box: Option<(f64, u32)> = None;
    // ponytail: a predicted branch per object (≈ 1.6 ns at 10^5–10^6; a 64-bit mask per 64
    // objects measured slower without SSE4.1 min/max); a grid over the objects answers in
    // O(cell) but costs ~40 ns per object to build after every scene change.
    for (i, g) in objs.geom.iter().enumerate() {
        let near = g[0].min(g[2]) <= x1
            && g[0].max(g[2]) >= x0
            && g[1].min(g[3]) <= y1
            && g[1].max(g[3]) >= y0;
        if near {
            hit_one(objs, i, p, tol, &mut best_seg, &mut best_box);
        }
    }
    let r = best_seg.or(best_box).map(|b| b.1);
    assert!(r.is_none_or(|i| (i as usize) < objs.len()));
    r
}

/// The exact test of object `i` against `p` within `tol`, folded into the best segment (by
/// distance) and the best box (by area) so far; the first of equals wins.
fn hit_one(
    objs: &Objects,
    i: usize,
    p: (f64, f64),
    tol: f64,
    best_seg: &mut Option<(f64, u32)>,
    best_box: &mut Option<(f64, u32)>,
) {
    let g = objs.geom[i];
    if objs.seg[i] {
        let d = seg_dist(p, g);
        if d <= tol && best_seg.is_none_or(|b| d < b.0) {
            *best_seg = Some((d, i as u32));
        }
    } else {
        let b = objs.bbox(i);
        let inside = p.0 >= f64::from(b[0]) - tol
            && p.0 <= f64::from(b[2]) + tol
            && p.1 >= f64::from(b[1]) - tol
            && p.1 <= f64::from(b[3]) + tol;
        let area = (f64::from(b[2]) - f64::from(b[0])) * (f64::from(b[3]) - f64::from(b[1]));
        if inside && best_box.is_none_or(|bb| area < bb.0) {
            *best_box = Some((area, i as u32));
        }
    }
}

/// The primitive rows `(kind, row)` of the selected objects, by table (lines, rects, arcs,
/// polylines, polygons, texts: kinds 0–5) then row: what [`marks`] and [`push_moved`] read.
/// Computed once per selection or scene change, so a move preview frame costs O(selection).
///
/// Cost: O(selection · log rows) to find each object's rows by its stamp, plus a sort of the
/// rows found.
pub fn selected_prims(sc: &Scene, selected: &Selected, out: &mut Vec<(u8, u32)>) {
    assert_eq!(selected.universe(), sc.objs.len());
    out.clear();
    for kind in 0..6 {
        let at = out.len();
        let stamps = sc.stamps.cols()[kind];
        for &o in &selected.objs {
            let rows = stamped(stamps, sc.objs.stamp[o as usize]);
            debug_assert!(rows.clone().all(|r| sc.owner.cols()[kind][r] == o));
            out.extend(rows.map(|r| (kind as u8, r as u32)));
        }
        out[at..].sort_unstable();
    }
    assert!(out.len() <= sc.prims.len());
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

/// The selected objects of a scene: a bitset over object indices beside the sorted list of
/// the set ones.
///
/// Why a bitset: every reader asks about one object per primitive or connection point (the
/// marks, the move preview on every frame of a drag, the export subset, the wire-start hit),
/// so a lookup must be O(1); a sorted index list alone would make those O(log selection)
/// each. Why the list: a change clears only the bits set before, so it costs O(old + new
/// selection) plus the growth of the bitset, not O(objects) as a `vec![false; objects]` did.
/// Capacity: [`CanvasPrims::CAPACITY`] objects, so `objects / 64` words.
#[derive(Clone, Debug, Default)]
pub struct Selected {
    words: Vec<u64>,
    objs: Vec<u32>,
    universe: u32,
}

impl Selected {
    /// Selects exactly `objs` (any order, duplicates allowed) out of a scene of `universe`
    /// objects. Cost: O(previous + new selection), plus zeroing the words a larger scene adds.
    ///
    /// # Panics
    ///
    /// If `universe` exceeds the scene object capacity or an index is `>= universe`.
    pub fn set(&mut self, universe: usize, objs: impl IntoIterator<Item = u32>) {
        assert!(universe <= CanvasPrims::CAPACITY, "scene object capacity");
        for &o in &self.objs {
            self.words[o as usize / 64] &= !(1u64 << (o % 64));
        }
        self.objs.clear();
        self.objs.extend(objs);
        self.objs.sort_unstable();
        self.objs.dedup();
        self.words.resize(universe.div_ceil(64), 0);
        for &o in &self.objs {
            assert!((o as usize) < universe, "selected object out of the scene");
            self.words[o as usize / 64] |= 1u64 << (o % 64);
        }
        self.universe = universe as u32;
        debug_assert_eq!(
            self.words
                .iter()
                .map(|w| w.count_ones() as usize)
                .sum::<usize>(),
            self.objs.len(),
            "one bit per selected object"
        );
    }

    /// Whether object `o` is selected; false for any index outside the scene (`u32::MAX`, the
    /// owner of an unowned primitive, included).
    pub fn contains(&self, o: u32) -> bool {
        self.words
            .get(o as usize / 64)
            .is_some_and(|w| w >> (o % 64) & 1 != 0)
    }

    /// Number of scene objects the set was made for (the scene's `objs.len()` when current).
    pub fn universe(&self) -> usize {
        self.universe as usize
    }

    /// Number of selected objects.
    pub fn len(&self) -> usize {
        self.objs.len()
    }

    /// True when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.objs.is_empty()
    }
}

/// Recolors every primitive of a selected object in `color`, in table then row order, from
/// the selection's rows ([`selected_prims`] of `sc`). Cost: O(selected primitives).
pub fn marks(sc: &Scene, rows: &[(u8, u32)], color: u32, out: &mut Marks) {
    *out = Marks::default();
    const KINDS: [PrimKind; 5] = [
        PrimKind::Line,
        PrimKind::Rect,
        PrimKind::Arc,
        PrimKind::Polyline,
        PrimKind::Polygon,
    ];
    for &(kind, row) in rows {
        if let Some(&k) = KINDS.get(usize::from(kind)) {
            out.push(k, row, color);
        }
    }
    assert!(out.kind.len() <= sc.prims.len());
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the prefiltered hit test answers what testing every object answers, ties and
    /// misses included, over random segments and boxes (some degenerate, some huge).
    #[test]
    fn hit_equals_a_full_scan() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |m: i64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % (m as u64)) as i64 - m / 2
        };
        let mut objs = Objects::default();
        for k in 0..4000u32 {
            let (x, y) = (next(2_000_000) as i32, next(2_000_000) as i32);
            let (w, h) = match k % 401 {
                0 => (next(400_000) as i32, next(400_000) as i32),
                k if k % 7 == 0 => (0, 0),
                _ => (next(20_000) as i32, next(20_000) as i32),
            };
            let key = ObjKey {
                table: TableId::Wires,
                key: k,
            };
            objs.push(key, [x, y, x + w, y + h], k % 3 == 0);
        }
        let full = |p: (f64, f64), tol: f64| {
            let (mut s, mut b) = (None, None);
            for i in 0..objs.len() {
                hit_one(&objs, i, p, tol, &mut s, &mut b);
            }
            s.or(b).map(|b: (f64, u32)| b.1)
        };
        let mut hits = 0;
        for k in 0..3000 {
            let tol = [0.0, 0.5, 250.0, 3000.0][k % 4];
            // Half the probes land near an object's corner or edge (just in, on, just out).
            let p = if k % 2 == 0 {
                let g = objs.geom[(next(8000) + 4000) as usize % objs.len()];
                let d = [0.0, tol, tol + 0.5, -tol][k / 2 % 4] + next(3) as f64;
                (f64::from(g[0]) + d, f64::from(g[1] + g[3]) / 2.0)
            } else {
                (next(2_000_000) as f64 + 0.37, next(2_000_000) as f64 - 0.61)
            };
            let got = hit(&objs, p, tol);
            assert_eq!(got, full(p, tol), "at {p:?} within {tol}");
            hits += usize::from(got.is_some());
        }
        assert!(
            hits > 300 && hits < 2900,
            "both hits and misses tested: {hits}"
        );
    }
}
