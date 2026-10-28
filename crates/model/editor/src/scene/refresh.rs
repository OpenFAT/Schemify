//! Building and updating the scene: [`build`] draws a document anew, [`update`] follows its
//! change log in place (dead rows kept, new rows appended).
//!
//! Contents:
//! - `pub(super) struct Build`, `Build::owned`, `Build::conn_at`, `Build::line`, `Build::rect`,
//!   `Build::arc`, `Build::poly`, `Build::text`, `Build::text_rot`
//! - `pub fn build`
//! - `pub(super) fn drawn`
//! - `pub fn update`
//! - `pub(super) fn counts`
//! - `pub(super) fn stamped`

use schemify_canvas::{CanvasPrims, LineStyle};
use schemify_doc::{DocHandle, ObjId, Project, TableId, Write};
use schemify_elab::CellState;

use crate::geometry::Pt;
use crate::session::View;

use super::dots::Wiring;
use super::draw;
use super::draw::Stamp;
use super::symbols;
use super::symbols::Symbols;
use super::symview;
use super::{ObjKey, Objects, Owners, Scene};

/// Scene builder state: the scene being filled plus the extent accumulator.
pub(super) struct Build<'a> {
    pub(super) sc: &'a mut Scene,
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
    pub(super) fn owned(&mut self, kind: usize, o: u32) {
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
    pub(super) fn conn_at(&mut self, p: Pt, o: u32) {
        self.sc.conn.push((p, o));
        self.sc.conn_stamp.push(self.sc.objs.stamp[o as usize]);
    }
    pub(super) fn line(&mut self, a: Pt, b: Pt, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.line_styled(a.0, a.1, b.0, b.1, layer, style);
        self.owned(0, o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    pub(super) fn rect(&mut self, a: Pt, b: Pt, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.rect_styled(a.0, a.1, b.0, b.1, layer, style);
        self.owned(1, o);
        self.grow(a.0, a.1);
        self.grow(b.0, b.1);
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn arc(
        &mut self,
        c: Pt,
        r: i32,
        start: i32,
        sweep: i32,
        layer: u8,
        style: LineStyle,
        o: u32,
    ) {
        self.sc
            .prims
            .arc_styled(c.0, c.1, r, start, sweep, layer, style);
        self.owned(2, o);
        self.grow(c.0 - r, c.1 - r);
        self.grow(c.0 + r, c.1 + r);
    }
    pub(super) fn poly(&mut self, pts: &[Pt], closed: bool, layer: u8, style: LineStyle, o: u32) {
        self.sc.prims.poly_styled(pts, closed, layer, style);
        self.owned(if closed { 4 } else { 3 }, o);
        for p in pts {
            self.grow(p.0, p.1);
        }
    }
    /// Text with its top-left at `at`; returns its box.
    pub(super) fn text(&mut self, at: Pt, size: i32, layer: u8, s: &str, o: u32) -> [i32; 4] {
        self.text_rot(at, size, layer, s, 0, o)
    }
    /// Text turned `rot` quarter turns clockwise about `at`; returns its box.
    /// Draws `s` turned `rot` clockwise quarter turns about `at`; returns its box. Text is
    /// never drawn upside down (format spec §3): a half turn draws the glyphs upright and a
    /// three-quarter turn as a quarter turn, from the far corner of the turned box, so the
    /// text reads normally and covers the box the turn gives it (right- and bottom-aligned at
    /// the anchor). xschem's `rot=2` labels land where xschem shows them.
    pub(super) fn text_rot(
        &mut self,
        at: Pt,
        size: i32,
        layer: u8,
        s: &str,
        rot: u8,
        o: u32,
    ) -> [i32; 4] {
        if s.is_empty() || size <= 0 {
            return [at.0, at.1, at.0, at.1];
        }
        let (from, drawn) = symbols::text_start(at, size, s, rot).expect("a drawn text");
        let (dx, dy) = symbols::rotated_extent(symbols::text_size(size, s), rot % 4);
        let (q0, q1) = ((at.0, at.1), (at.0 + dx, at.1 + dy));
        self.sc
            .prims
            .text_rot(from.0, from.1, size, layer, s, drawn);
        self.owned(5, o);
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
                draw::instance(&mut b, project, h, doc, symbols, values, i, &mut st);
            }
            for i in 0..doc.wires().id.len() {
                draw::wire(&mut b, doc, i);
            }
            b.sc.wiring = Wiring::of(doc.wires());
            let mut ends = b.sc.wiring.xy.clone();
            ends.dedup();
            symview::redot(&mut b, &ends);
            draw::other_objects(&mut b, doc, 0..usize::MAX);
        }
        View::Symbol => symview::symbol(&mut b, doc),
    }
    let ext = if b.ext[0] > b.ext[2] { [0; 4] } else { b.ext };
    sc.extent = ext;
    fit_boxes(sc, [0; 6]);
    sc.index.reserve(sc.objs.len());
    sc.index.extend(
        sc.objs
            .key
            .iter()
            .enumerate()
            .map(|(i, &k)| (super::pack(k), i as u32)),
    );
    sc.built = Some((h, view, epoch, symbols.epoch));
    sc.generation = generation;
    check_tables(sc);
}

/// Tables a schematic scene draws objects of.
pub(super) fn drawn(t: TableId) -> bool {
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
/// unchanged except on the objects touched and on `revalued` (the instances whose values
/// changed, elab's value delta; keys the document no longer holds are skipped): when values
/// changed elsewhere, call [`build`]. A revalued instance no write touched keeps its rows and
/// has its texts rewritten in place (reported through [`Scene::edits`] as killed rows that
/// are live) when each text keeps its length and box; otherwise it is re-stamped.
///
/// Cost: O((touched + revalued objects) · (log rows + their primitives)), plus the index
/// inserts.
///
/// # Panics
///
/// If a key of `revalued` is not an instance, or two objects share a key.
pub fn update(
    sc: &mut Scene,
    project: &Project,
    h: DocHandle,
    symbols: &Symbols,
    epoch: u32,
    values: Option<&CellState>,
    revalued: &[ObjKey],
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
    let mut edited: Vec<ObjKey> = changes
        .iter()
        .filter(|c| c.what != Write::Moved && drawn(c.table) && c.id != ObjId::NONE)
        .map(|c| ObjKey {
            table: c.table,
            key: c.id.0,
        })
        .collect();
    assert!(
        revalued.iter().all(|k| k.table == TableId::Instances),
        "values change on instances only"
    );
    edited.sort_unstable();
    edited.dedup();
    let mut keys: Vec<ObjKey> = edited.iter().chain(revalued).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    let total = sc.objs.len().max(1);
    if keys.len() > total / 8 + 64 || sc.dead * 4 > sc.prims.len() + (1 << 14) {
        // ponytail: most of the document changed (a paste, an import), or a quarter of the
        // rows are dead: one full build is cheaper than this many removals, and compacts
        // (~30 ms at 10^5 objects, once per tens of thousands of removed rows; compacting on
        // an idle tick would take it off the edit frame).
        build(sc, project, h, View::Schematic, symbols, epoch, values);
        return false;
    }
    // An instance whose only change is its values keeps its rows: its texts are rewritten
    // in place when each keeps its length (a revalued `{W}` text); the rest are re-stamped.
    let mut st = Stamp::default();
    let restamp: Vec<ObjKey> = keys
        .iter()
        .copied()
        .filter(|&key| {
            let row = doc.find_id(ObjId(key.key)).filter(|l| l.table == key.table);
            let (Some(o), Some(loc)) = (sc.find(key), row) else {
                return true;
            };
            edited.binary_search(&key).is_ok()
                || !draw::revalue_texts(
                    sc,
                    project,
                    h,
                    doc,
                    symbols,
                    values,
                    (loc.row as usize, o),
                    &mut st,
                )
        })
        .collect();
    // Junction points the changed wires can affect, while the old wires are still indexed;
    // then the old objects go.
    let mut points: Vec<Pt> = Vec::new();
    for &key in &restamp {
        let Some(o) = sc.find(key) else { continue };
        if key.table == TableId::Wires {
            let w = sc.objs.geom[o as usize];
            sc.wiring.touched(w, &mut points);
            sc.wiring.remove(w);
        }
        remove_object(sc, key);
    }
    // Stamp what the document holds now, then index it.
    let (objs0, prims0) = (sc.objs.len(), counts(sc));
    let ext = match sc.extent {
        [0, 0, 0, 0] => [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
        e => e,
    };
    let mut b = Build { ext, sc };
    let mut wires: Vec<[i32; 4]> = Vec::new();
    for &key in &restamp {
        let Some(loc) = doc.find_id(ObjId(key.key)).filter(|l| l.table == key.table) else {
            continue; // deleted
        };
        let i = loc.row as usize;
        match key.table {
            TableId::Instances => {
                draw::instance(&mut b, project, h, doc, symbols, values, i, &mut st)
            }
            TableId::Wires => {
                draw::wire(&mut b, doc, i);
                let w = doc.wires();
                let p = [w.x1[i], w.y1[i], w.x2[i], w.y2[i]];
                b.sc.wiring.add(p);
                wires.push(p);
            }
            t => draw::one_other(&mut b, doc, t, i),
        }
    }
    for &w in &wires {
        b.sc.wiring.touched(w, &mut points);
    }
    points.sort_unstable();
    points.dedup();
    symview::redot(&mut b, &points);
    let ext = b.ext;
    sc.extent = if ext[0] > ext[2] { [0; 4] } else { ext };
    fit_boxes(sc, prims0);
    for o in objs0..sc.objs.len() {
        let prev = sc.index.insert(super::pack(sc.objs.key[o]), o as u32);
        assert!(prev.is_none(), "one object per key");
    }
    sc.built = Some((h, View::Schematic, epoch, symbols.epoch));
    sc.cursor = Some(doc.cursor());
    sc.touched = Some(restamp);
    sc.generation = sc.generation.wrapping_add(1);
    check_tables(sc);
    true
}

/// Primitive rows per kind ([`CanvasPrims::kill`] order): where an update's new rows start.
pub(super) fn counts(sc: &Scene) -> [usize; 6] {
    sc.owner.cols().map(Vec::len)
}

/// The rows (or connection points) carrying stamp `s` in a non-decreasing stamp column.
pub(super) fn stamped(col: &[u32], s: u32) -> std::ops::Range<usize> {
    let lo = col.partition_point(|&x| x < s);
    let hi = lo + col[lo..].partition_point(|&x| x == s);
    assert!(hi <= col.len());
    lo..hi
}

// ---- private ----

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

/// Removes object `key` (when drawn): its primitive rows die in place (reported through
/// [`Scene::edits`]), its connection points lose their owner, its index entry goes; the last
/// object takes its index (swap-remove), so that object's rows and points are renumbered.
///
/// Cost: O(log rows) per kind plus the rows of the two objects involved.
fn remove_object(sc: &mut Scene, key: ObjKey) {
    let Some(o) = sc.index.remove(&super::pack(key)) else {
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
        sc.index.insert(super::pack(moved), o);
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
fn fit_boxes(sc: &mut Scene, from: [usize; 6]) {
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
    for i in from[0]..p.lines.x0.len() {
        let l = &p.lines;
        grow(o.lines[i], l.x0[i], l.y0[i], l.x1[i], l.y1[i]);
    }
    for i in from[1]..p.rects.x0.len() {
        let r = &p.rects;
        grow(o.rects[i], r.x0[i], r.y0[i], r.x1[i], r.y1[i]);
    }
    for i in from[2]..p.arcs.cx.len() {
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
        (&p.polylines, &o.polylines, from[3]),
        (&p.polygons, &o.polygons, from[4]),
    ] {
        let tail = polys.end.iter().zip(owners).enumerate().skip(k0);
        for (k, (&end, &owner)) in tail {
            let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
            for j in start..end as usize {
                grow(owner, polys.x[j], polys.y[j], polys.x[j], polys.y[j]);
            }
        }
    }
    let t = &p.texts;
    for i in from[5]..t.x.len() {
        let start = if i == 0 { 0 } else { t.end[i - 1] as usize };
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let (w, h) = symbols::rotated_extent(
            symbols::text_size(t.size[i], &t.text[start..t.end[i] as usize]),
            rot,
        );
        grow(o.texts[i], t.x[i], t.y[i], t.x[i] + w, t.y[i] + h);
    }
}
