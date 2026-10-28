//! The pack: canvas tables and how they changed in, retained instance rows out
//! ([`InstanceRows`]). Each 1:1 column (lines, rects, arcs) is laid out by palette layer with
//! free slots for appended rows and patched in place where a [`Change::Killed`] says; the
//! polyline and polygon edges and the polygon fills are repacked whole on any chain change.
//! The marks' source is packed in [`super::marks`].
//!
//! Contents:
//! - `pub struct InstanceRows`, `InstanceRows::count`, `InstanceRows::bytes`,
//!   `InstanceRows::layer_slots`, `InstanceRows::row`, `InstanceRows::clear_dirty`,
//!   `InstanceRows::dirty`, `InstanceRows::dirty_bytes`, `InstanceRows::pack`
//! - `pub(super) struct Table`, `Table::layout`, `Table::take`, `Table::put`; `pub(super) const BYTES_MAX`
//! - `pub(super) fn row(buf: &mut [u8], stride: usize, i: usize, fields: &[u32])`
//! - `pub(super) fn seg_fields`, `arc_fields`, `edge_fields`, `edges_of` (a row's words)

use std::ops::Range;

use schemify_canvas::{Arcs, CanvasPrims, LAYERS, LineStyle, Lines, Polys};

use super::ear;
use super::pens::{Pen, Pens};
use super::{
    ARC, CLOSED, Change, DIRTY_MAX, EDGE, FILL, FIRST, Frame, KINDS, LINE, MARKS, RECT, SOURCES,
    STRIDE, TRI,
};

/// The instance rows of one canvas: per source and kind the rows grouped by layer, the slot
/// of each table row, and the byte ranges written since the GPU last read them
/// ([`super::Pipelines::upload`]). Headless: plain byte tables, so the pack is tested without
/// a GPU. Owner: the window's canvas (or the CPU painter's); written by the painters' canvas
/// calls.
#[derive(Default)]
pub struct InstanceRows {
    pub(super) tables: [[Table; KINDS]; SOURCES],
    /// Per source: the slot of each lines, rects and arcs row ([`NO_SLOT`]: none).
    slots: [[Vec<u32>; 3]; SOURCES],
    /// Per source: ever packed, and the lengths of its lines, rects, arcs, polylines and
    /// polygons tables at the last pack.
    pub(super) packed: [bool; SOURCES],
    counts: [[usize; 5]; SOURCES],
    /// Scratch of [`InstanceRows::repack_polys`]: one polygon's triangles, and the live
    /// chains in drawing order.
    ears: Vec<[u32; 3]>,
    order: Vec<u32>,
    /// The palette's pens, resolved on first use and kept while the palette is the same.
    pens: Pens,
    /// Per row of the column being repacked: its palette layer, or [`NO_KEY`] when it draws
    /// nothing (the pack's one decision per row, made once).
    keys: Vec<u8>,
    /// The frame to draw, set by the painter's canvas call and taken by the window.
    pub frame: Option<Frame>,
}

/// One row array: rows grouped by palette layer, each layer a slot range with free slots at
/// its end, and what was written since the GPU last read it.
#[derive(Clone, Debug, Default)]
pub(super) struct Table {
    pub(super) bytes: Vec<u8>,
    /// Per layer: first slot, slots in use (drawn), slots reserved.
    pub(super) start: [u32; LAYERS],
    pub(super) used: [u32; LAYERS],
    cap: [u32; LAYERS],
    pub(super) dirty: Vec<Range<usize>>,
}

/// Most bytes one row array holds: wgpu's default buffer limit (256 MiB).
pub(super) const BYTES_MAX: usize = 1 << 28;

/// A table row with no slot (dead, hidden, or nothing to draw).
const NO_SLOT: u32 = u32::MAX;
/// A row that draws nothing (dead, or its layer hidden) in [`InstanceRows::repack`]'s key
/// column: above every palette layer.
const NO_KEY: u8 = u8::MAX;
/// The kind each 1:1 table column feeds: lines, rects, arcs.
const ONE_TO_ONE: [usize; 3] = [LINE, RECT, ARC];

const _: () = assert!(NO_KEY as usize >= LAYERS, "no key is no layer");
const _: () = {
    let mut k = 0;
    while k < KINDS {
        assert!(STRIDE[k].is_multiple_of(4), "rows are whole words");
        assert!(STRIDE[k] <= 7 * 4, "a row is at most `row`'s seven words");
        k += 1;
    }
};
const _: () = assert!(
    CanvasPrims::CAPACITY < 1 << 31,
    "a chain index leaves the closed bit"
);

/// A packed 1:1 row: its layer and its words (the kind's [`STRIDE`] / 4 of them used).
struct Packed {
    layer: usize,
    words: [u32; 7],
}

impl std::fmt::Debug for InstanceRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n: usize = (0..SOURCES)
            .flat_map(|s| (0..KINDS).map(move |k| (s, k)))
            .map(|(s, k)| self.count(s, k))
            .sum();
        f.debug_struct("InstanceRows").field("rows", &n).finish()
    }
}

impl InstanceRows {
    /// Slots of kind `kind` from source `src` in use (drawn; an emptied slot draws nothing).
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES` or `kind ≥ KINDS`.
    #[must_use]
    pub fn count(&self, src: usize, kind: usize) -> usize {
        assert!(src < SOURCES, "a row source");
        assert!(kind < KINDS, "a row kind");
        let t = &self.tables[src][kind];
        let n: usize = t.used.iter().map(|&u| u as usize).sum();
        debug_assert!(
            n * STRIDE[kind] <= t.bytes.len(),
            "the slots in use are rows"
        );
        n
    }

    /// The rows of kind `kind` from source `src`, `STRIDE[kind]` bytes per slot, every layer
    /// range in turn (free slots zero).
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES` or `kind ≥ KINDS`.
    #[must_use]
    pub fn bytes(&self, src: usize, kind: usize) -> &[u8] {
        assert!(src < SOURCES, "a row source");
        assert!(kind < KINDS, "a row kind");
        let b = &self.tables[src][kind].bytes;
        debug_assert_eq!(b.len() % STRIDE[kind], 0, "whole rows");
        b
    }

    /// The slots of layer `layer` of kind `kind` from source `src` that are drawn.
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES`, `kind ≥ KINDS` or `layer ≥ LAYERS`.
    #[must_use]
    pub fn layer_slots(&self, src: usize, kind: usize, layer: usize) -> Range<u32> {
        assert!(layer < LAYERS, "a palette layer");
        let t = &self.tables[src][kind];
        let slots = t.start[layer]..t.start[layer] + t.used[layer];
        debug_assert!(
            slots.end as usize * STRIDE[kind] <= t.bytes.len(),
            "a layer's slots are rows"
        );
        slots
    }

    /// Slot `slot` of kind `kind` from source `src` as its `STRIDE[kind] / 4` words, the rest
    /// zero (the canvas shaders' `In` fields: coordinates, then colour and style word; a fill
    /// triangle's colour last). An empty slot is all zero.
    ///
    /// # Panics
    ///
    /// Panics on a slot past the rows.
    #[inline]
    #[must_use]
    pub fn row(&self, src: usize, kind: usize, slot: u32) -> [u32; 7] {
        let b = &self.tables[src][kind].bytes[slot as usize * STRIDE[kind]..][..STRIDE[kind]];
        debug_assert_eq!(b.len() % 4, 0, "a row is whole words");
        let mut w = [0u32; 7];
        for (word, c) in w.iter_mut().zip(b.as_chunks::<4>().0) {
            *word = u32::from_ne_bytes(*c);
        }
        w
    }

    /// Forgets the dirty ranges, as an upload does: for a consumer that reads the rows in
    /// place (the CPU painter), and for tests that count the bytes one frame writes.
    pub fn clear_dirty(&mut self) {
        for t in self.tables.iter_mut().flatten() {
            t.dirty.clear();
        }
        debug_assert_eq!(self.dirty_bytes(), 0, "no range left");
    }

    /// The byte ranges of `(src, kind)` written since the GPU last read the rows.
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES` or `kind ≥ KINDS`.
    #[must_use]
    pub fn dirty(&self, src: usize, kind: usize) -> &[Range<usize>] {
        assert!(src < SOURCES, "a row source");
        assert!(kind < KINDS, "a row kind");
        let d = &self.tables[src][kind].dirty;
        debug_assert!(d.len() <= DIRTY_MAX, "dirty ranges bounded");
        d
    }

    /// Bytes the GPU has to read to be current: the dirty ranges, clipped to the rows.
    #[must_use]
    pub fn dirty_bytes(&self) -> usize {
        let mut n = 0;
        let mut total = 0;
        for t in self.tables.iter().flatten() {
            let len = t.bytes.len();
            total += len;
            debug_assert!(t.dirty.len() <= DIRTY_MAX, "dirty ranges bounded");
            n += t
                .dirty
                .iter()
                .map(|r| r.end.min(len).saturating_sub(r.start.min(len)))
                .sum::<usize>();
        }
        // Ranges of one array may overlap only before they merge (`push_dirty`), so at most
        // DIRTY_MAX copies of each byte.
        debug_assert!(n <= total * DIRTY_MAX, "dirty bytes are bytes of the rows");
        n
    }

    /// Packs source table `src` (one of the four canvas tables or the overlay, not
    /// [`MARKS`]) of `t` after `change`, resolving each row's colour and width from
    /// `palette`. A [`Change::Killed`] empties the killed rows' slots and puts appended rows
    /// into their layer's free slots, writing only those; a layer out of free slots, a table
    /// that shrank, an edit script, [`Change::All`] or a source never packed repack it whole.
    /// Polyline and polygon edges and fills are repacked whole when either chain table
    /// changed.
    ///
    /// Cost: O(rows written); [`Change::Same`] writes none.
    ///
    /// # Panics
    ///
    /// Panics on `src` = [`MARKS`] or past [`SOURCES`].
    pub fn pack(
        &mut self,
        src: usize,
        t: &CanvasPrims,
        palette: &schemify_canvas::Palette,
        change: Change<'_>,
    ) {
        assert!(src < SOURCES, "a row source");
        assert!(src != MARKS, "the marks pack through `pack_marks`");
        let n = [
            t.lines.x0.len(),
            t.rects.x0.len(),
            t.arcs.cx.len(),
            t.polylines.end.len(),
            t.polygons.end.len(),
        ];
        assert!(
            n.iter().all(|&c| c <= CanvasPrims::CAPACITY),
            "tables in capacity"
        );
        self.pens.use_palette(palette);
        let was = self.counts[src];
        let whole = !self.packed[src] || matches!(change, Change::All | Change::Edits(_));
        let polys = match change {
            Change::Same => false,
            Change::Killed(k) => {
                n[3] != was[3] || n[4] != was[4] || !k[3].is_empty() || !k[4].is_empty()
            }
            Change::All | Change::Edits(_) => true,
        };
        for c in 0..3 {
            let patched = match change {
                Change::Killed(k) if !whole => self.patch(src, c, t, k),
                Change::Same if !whole => true,
                _ => false,
            };
            if !patched {
                self.repack(src, c, t);
            }
        }
        if whole || polys {
            self.repack_polys(src, t);
        }
        self.counts[src] = n;
        self.packed[src] = true;
        assert!(
            (0..3).all(|c| self.slots[src][c].len() == n[c]),
            "a slot per table row"
        );
    }
}

impl Table {
    /// Lays the table out for `count[l]` rows per layer, with free slots after each non-empty
    /// layer's rows when `slack`; every slot zero (empty), every layer unused, all dirty.
    ///
    /// # Panics
    ///
    /// Panics past [`BYTES_MAX`].
    pub(super) fn layout(&mut self, kind: usize, count: &[u32; LAYERS], slack: bool) {
        assert!(kind < KINDS, "a row kind");
        let mut at = 0u32;
        for (l, &n) in count.iter().enumerate() {
            let free = if slack && n > 0 { n / 8 + 16 } else { 0 };
            (self.start[l], self.used[l], self.cap[l]) = (at, 0, n + free);
            at = at.checked_add(n + free).expect("slots are u32");
        }
        let len = at as usize * STRIDE[kind];
        assert!(len <= BYTES_MAX, "rows fit wgpu's buffer limit");
        self.bytes.clear();
        self.bytes.resize(len, 0);
        self.dirty.clear();
        super::push_dirty(&mut self.dirty, 0..len);
        debug_assert_eq!(self.used, [0; LAYERS], "every layer unused");
        debug_assert_eq!(self.bytes.len(), len, "every slot laid out");
    }

    /// Takes the next free slot of layer `l`, or `None` when its range is full.
    pub(super) fn take(&mut self, l: usize) -> Option<u32> {
        assert!(l < LAYERS, "a palette layer");
        let slot = (self.used[l] < self.cap[l]).then(|| {
            self.used[l] += 1;
            self.start[l] + self.used[l] - 1
        });
        debug_assert!(self.used[l] <= self.cap[l], "a layer's slots are bounded");
        slot
    }

    /// Writes `fields` into slot `s` of this `kind`'s rows and notes it dirty.
    pub(super) fn put(&mut self, kind: usize, s: u32, fields: &[u32]) {
        let at = s as usize * STRIDE[kind];
        assert!(
            at + STRIDE[kind] <= self.bytes.len(),
            "slot {s} is a laid-out slot"
        );
        row(&mut self.bytes, STRIDE[kind], s as usize, fields);
        super::push_dirty(&mut self.dirty, at..at + STRIDE[kind]);
        debug_assert!(!self.dirty.is_empty(), "the slot is noted dirty");
    }
}

/// Writes `fields` as row `i` of a `stride`-byte row array (which holds it).
///
/// # Panics
///
/// Panics unless `fields` is one whole row, or past the array.
pub(super) fn row(buf: &mut [u8], stride: usize, i: usize, fields: &[u32]) {
    assert_eq!(fields.len() * 4, stride, "a whole row");
    let at = i * stride;
    assert!(at + stride <= buf.len(), "row {i} is inside the array");
    let out = &mut buf[at..at + stride];
    for (b, f) in out.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
        *b = f.to_ne_bytes();
    }
}

/// Line or rect row `i` of `l` in colour `c` and style word `s`: `[x0, y0, x1, y1, c, s, 0]`.
pub(super) fn seg_fields(l: &Lines, i: usize, c: u32, s: u32) -> [u32; 7] {
    debug_assert_eq!(l.x1.len(), l.x0.len(), "columns in step");
    debug_assert_eq!(l.y1.len(), l.x0.len(), "columns in step");
    let f = [l.x0[i], l.y0[i], l.x1[i], l.y1[i]].map(|v| v as u32);
    [f[0], f[1], f[2], f[3], c, s, 0]
}

/// Arc row `i` of `a` in colour `c` and style word `s`: `[cx, cy, r, start, sweep, c, s]`.
pub(super) fn arc_fields(a: &Arcs, i: usize, c: u32, s: u32) -> [u32; 7] {
    debug_assert_eq!(a.r.len(), a.cx.len(), "columns in step");
    debug_assert_eq!(a.sweep.len(), a.cx.len(), "columns in step");
    let f = [a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i]].map(|v| v as u32);
    [f[0], f[1], f[2], f[3], f[4], c, s]
}

/// Edges chain `k` of `p` draws (a polygon, `closed`, also its closing edge).
pub(super) fn edges_of(p: &Polys, k: usize, closed: bool) -> usize {
    let m = schemify_canvas::chain(&p.end, k).len();
    assert!(m >= 2, "malformed poly table");
    let edges = if closed { m } else { m - 1 };
    debug_assert!(edges >= 1, "a chain has an edge");
    edges
}

/// Edge `j` of chain `k` of `p` in colour `c` and style word `s`, its chain flags added:
/// `[xa, ya, xb, yb, c, s | flags]`.
pub(super) fn edge_fields(p: &Polys, k: usize, j: usize, closed: bool, c: u32, s: u32) -> [u32; 6] {
    let pts = schemify_canvas::chain(&p.end, k);
    let m = pts.len();
    debug_assert!(j < m, "an edge of the chain");
    debug_assert!(closed || j + 1 < m, "an open chain has no closing edge");
    let (a, b) = (pts.start + j, pts.start + (j + 1) % m);
    let f = [p.x[a], p.y[a], p.x[b], p.y[b]].map(|v| v as u32);
    let first = if j == 0 { FIRST } else { 0 };
    let close = if closed { CLOSED } else { 0 };
    [f[0], f[1], f[2], f[3], c, s | ((first | close) << 20)]
}

// ---- private ----

impl InstanceRows {
    /// Repacks 1:1 column `c` (0 lines, 1 rects, 2 arcs) of source `src` whole.
    fn repack(&mut self, src: usize, c: usize, t: &CanvasPrims) {
        assert!(c < ONE_TO_ONE.len(), "a 1:1 column");
        let (l, r, a) = (&t.lines, &t.rects, &t.arcs);
        match c {
            0 => self.repack_with(
                src,
                c,
                (&l.layer, &l.style),
                |_| false,
                |i, co, st| seg_fields(l, i, co, st),
            ),
            1 => self.repack_with(
                src,
                c,
                (&r.layer, &r.style),
                |_| true,
                |i, co, st| seg_fields(r, i, co, st),
            ),
            _ => self.repack_with(
                src,
                c,
                (&a.layer, &a.style),
                |i| a.sweep[i].abs() >= 360_000,
                |i, co, st| arc_fields(a, i, co, st),
            ),
        }
    }

    /// Repacks column `c` of source `src` (its `layer` and `style` columns, `closed(i)` when
    /// row `i` may fill, `fields(i, colour, style word)` its words): one pass keys each row by
    /// its layer (or [`NO_KEY`]) and counts the layers, the layout places the layer ranges,
    /// and one pass writes each live row into the next slot of its layer. Each row's pen comes
    /// from the cached [`Pens`]; the key pass reads only the layer and style columns.
    ///
    /// Cost: O(rows); about 10 ns a row (`pack_100k_whole`).
    fn repack_with(
        &mut self,
        src: usize,
        c: usize,
        (layer, style): (&[u8], &[LineStyle]),
        closed: impl Fn(usize) -> bool,
        fields: impl Fn(usize, u32, u32) -> [u32; 7],
    ) {
        let kind = ONE_TO_ONE[c];
        let (n, stride) = (layer.len(), STRIDE[kind]);
        assert_eq!(style.len(), n, "columns in step");
        let (keys, pens) = (&mut self.keys, &mut self.pens);
        keys.clear();
        let mut count = [0u32; LAYERS];
        for i in 0..n {
            let k = match pens.get(layer[i], style[i], closed(i)) {
                Some(_) => layer[i],
                None => NO_KEY,
            };
            if k != NO_KEY {
                count[usize::from(k)] += 1;
            }
            keys.push(k);
        }
        let table = &mut self.tables[src][kind];
        table.layout(kind, &count, true);
        let slots = &mut self.slots[src][c];
        slots.clear();
        slots.reserve(n);
        for (i, &k) in keys.iter().enumerate() {
            if k == NO_KEY {
                slots.push(NO_SLOT);
                continue;
            }
            let pen = pens.get(k, style[i], closed(i)).expect("keyed live");
            let l = usize::from(k);
            debug_assert!(table.used[l] < table.cap[l], "laid out for every row");
            let s = table.start[l] + table.used[l];
            table.used[l] += 1;
            let at = s as usize * stride;
            let out = &mut table.bytes[at..at + stride];
            for (b, f) in out.chunks_exact_mut(4).zip(fields(i, pen.color, pen.style)) {
                b.copy_from_slice(&f.to_ne_bytes());
            }
            slots.push(s);
        }
        assert_eq!(table.used, count, "every live row placed");
        assert_eq!(slots.len(), n, "a slot per table row");
    }

    /// Follows `killed` and the rows appended to column `c` of source `src`. False when the
    /// column shrank or a layer ran out of free slots: the caller then repacks it.
    fn patch(&mut self, src: usize, c: usize, t: &CanvasPrims, killed: &[Vec<u32>; 6]) -> bool {
        assert!(c < ONE_TO_ONE.len(), "a 1:1 column");
        let kind = ONE_TO_ONE[c];
        let words = STRIDE[kind] / 4;
        let (was, n) = (self.slots[src][c].len(), column_len(t, c));
        if n < was {
            return false; // removed rows: not a kill-and-append edit after all
        }
        let (table, slots) = (&mut self.tables[src][kind], &mut self.slots[src][c]);
        for &r in &killed[c] {
            let r = r as usize;
            if r < was && slots[r] != NO_SLOT {
                table.put(kind, slots[r], &[0; 7][..words]);
                slots[r] = NO_SLOT;
            }
        }
        for i in was..n {
            let s = match one_row(t, c, i, &mut self.pens) {
                Some(Packed {
                    layer,
                    words: fields,
                }) => {
                    let Some(s) = table.take(layer) else {
                        return false;
                    };
                    table.put(kind, s, &fields[..words]);
                    s
                }
                None => NO_SLOT,
            };
            slots.push(s);
        }
        debug_assert_eq!(slots.len(), n, "a slot per table row");
        true
    }

    /// Repacks the polyline and polygon edges and the polygon fills of source `src` whole,
    /// layer by layer (no free slots: they are rebuilt on any chain change).
    fn repack_polys(&mut self, src: usize, t: &CanvasPrims) {
        // The live chains, polylines then polygons, each in row order; the layer ranges put
        // them in drawing order (one colour per layer, so only the layer order shows).
        let order = &mut self.order;
        order.clear();
        let (mut edges, mut tris) = ([0u32; LAYERS], [0u32; LAYERS]);
        for (closed, p) in [(false, &t.polylines), (true, &t.polygons)] {
            for k in 0..p.end.len() {
                let Some(Pen { style: s, .. }) = self.pens.get(p.layer[k], p.style[k], closed)
                else {
                    continue;
                };
                let l = usize::from(p.layer[k]);
                let m = edges_of(p, k, closed) as u32;
                edges[l] += m;
                if closed && s & (FILL << 20) != 0 {
                    tris[l] += m.saturating_sub(2);
                }
                debug_assert!(k < 1 << 31, "a chain index leaves the closed bit");
                order.push((u32::from(closed) << 31) | k as u32);
            }
        }
        let [tri, .., edge] = &mut self.tables[src];
        edge.layout(EDGE, &edges, false);
        tri.layout(TRI, &tris, false);
        for &o in order.iter() {
            let (closed, k) = (o >> 31 == 1, (o & !(1 << 31)) as usize);
            let p = if closed { &t.polygons } else { &t.polylines };
            let Pen { color: c, style: s } =
                self.pens.get(p.layer[k], p.style[k], closed).expect("live");
            let l = usize::from(p.layer[k]);
            for j in 0..edges_of(p, k, closed) {
                let at = edge.take(l).expect("laid out") as usize;
                row(
                    &mut edge.bytes,
                    STRIDE[EDGE],
                    at,
                    &edge_fields(p, k, j, closed, c, s),
                );
            }
            let pts = schemify_canvas::chain(&p.end, k);
            if closed && s & (FILL << 20) != 0 && pts.len() >= 3 {
                let (xs, ys) = (&p.x[pts.clone()], &p.y[pts]);
                ear::ear_clip(xs, ys, &mut self.ears);
                for &[a, b, d] in &self.ears {
                    let (a, b, d) = (a as usize, b as usize, d as usize);
                    let f = [xs[a], ys[a], xs[b], ys[b], xs[d], ys[d]].map(|v| v as u32);
                    let at = tri.take(l).expect("laid out") as usize;
                    let fields = [f[0], f[1], f[2], f[3], f[4], f[5], c];
                    row(&mut tri.bytes, STRIDE[TRI], at, &fields);
                }
            }
        }
        assert_eq!(edge.used, edges, "every edge placed");
        assert_eq!(tri.used, tris, "every triangle placed");
    }
}

/// Rows of 1:1 column `c` (0 lines, 1 rects, 2 arcs) of `t`.
fn column_len(t: &CanvasPrims, c: usize) -> usize {
    match c {
        0 => t.lines.x0.len(),
        1 => t.rects.x0.len(),
        2 => t.arcs.cx.len(),
        _ => unreachable!("three 1:1 columns"),
    }
}

/// Row `i` of 1:1 column `c` packed, or `None` when it draws nothing (dead, or its layer
/// hidden).
fn one_row(t: &CanvasPrims, c: usize, i: usize, pens: &mut Pens) -> Option<Packed> {
    match c {
        0 => seg_row(&t.lines, i, false, pens),
        1 => seg_row(&t.rects, i, true, pens),
        2 => arc_row(&t.arcs, i, pens),
        _ => unreachable!("three 1:1 columns"),
    }
}

/// Line or rect row `i` of `l` (`closed` for a rect) packed with its pen.
fn seg_row(l: &Lines, i: usize, closed: bool, pens: &mut Pens) -> Option<Packed> {
    let pen = pens.get(l.layer[i], l.style[i], closed)?;
    debug_assert!(
        usize::from(l.layer[i]) < LAYERS,
        "a live row has a palette layer"
    );
    Some(Packed {
        layer: usize::from(l.layer[i]),
        words: seg_fields(l, i, pen.color, pen.style),
    })
}

/// Arc row `i` of `a` packed with its pen (a full circle is closed).
fn arc_row(a: &Arcs, i: usize, pens: &mut Pens) -> Option<Packed> {
    let full = a.sweep[i].abs() >= 360_000;
    let pen = pens.get(a.layer[i], a.style[i], full)?;
    debug_assert!(
        usize::from(a.layer[i]) < LAYERS,
        "a live row has a palette layer"
    );
    Some(Packed {
        layer: usize::from(a.layer[i]),
        words: arc_fields(a, i, pen.color, pen.style),
    })
}

#[cfg(test)]
mod tests {
    use super::super::PARITY;
    use super::super::pens::pen;
    use super::*;
    use schemify_canvas::{CanvasDiff, LineStyle, Marks, Palette, TableChange};

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks_exact(4)
            .map(|c| u32::from_ne_bytes(c.try_into().expect("4 bytes")))
            .collect()
    }

    /// A fresh pack of `t`: the oracle every patched table is compared with.
    fn rebuilt(t: &CanvasPrims, palette: &Palette) -> InstanceRows {
        let mut r = InstanceRows::default();
        r.pack(0, t, palette, Change::All);
        r
    }

    /// As the GPU takes it: every dirty range read.
    fn uploaded(r: &mut InstanceRows) {
        r.clear_dirty();
    }

    /// What the GPU draws from `(src, kind)`: per layer the non-empty rows of its range, in
    /// slot order (the order they are drawn in).
    fn drawn(r: &InstanceRows, src: usize, kind: usize) -> Vec<(usize, Vec<u8>)> {
        let mut out = Vec::new();
        for l in 0..LAYERS {
            for s in r.layer_slots(src, kind, l) {
                let b = &r.bytes(src, kind)[s as usize * STRIDE[kind]..][..STRIDE[kind]];
                if b.iter().any(|&x| x != 0) {
                    out.push((l, b.to_vec()));
                }
            }
        }
        out
    }

    fn same_rows(a: &InstanceRows, b: &InstanceRows, src: usize, what: &str) {
        for k in 0..KINDS {
            assert_eq!(drawn(a, src, k), drawn(b, src, k), "{what}: kind {k}");
        }
    }

    /// Counts and bytes of every kind match a hand computation; rows are grouped by layer
    /// with an eighth plus 16 free slots after each; a row's fields sit where the canvas shaders
    /// read them: coordinates, colour bytes R G B A, width in 1/16 px, dash, flags.
    #[test]
    fn rows_match_a_hand_computation() {
        let palette = Palette::xschem_dark();
        let mut t = CanvasPrims::default();
        for i in 0..7 {
            t.line(i, 10, i + 100, 20, 1);
        }
        t.line_styled(1, 2, 3, 4, 4, LineStyle::new(3, 7));
        for i in 0..5 {
            t.rect(i, 0, i + 10, 10, 4);
        }
        t.rect(0, 0, 5, 5, 5); // layer 5 fills
        t.arc(50, 60, 7, 0, 360_000, 4);
        t.arc_styled(50, 60, 7, 0, 90_000, 4, LineStyle::new(2, 0).filled());
        t.poly(&[(0, 0), (10, 0), (10, 10), (20, 10)], false, 1); // 3 edges
        t.poly_styled(
            &[(0, 0), (10, 0), (10, 10), (5, 15), (0, 10)],
            true,
            4,
            LineStyle::PLAIN.filled(),
        ); // 5 edges, 3 triangles
        let r = rebuilt(&t, &palette);
        assert_eq!((r.count(0, LINE), r.count(0, EDGE)), (8, 3 + 5));
        assert_eq!(
            (r.count(0, RECT), r.count(0, ARC), r.count(0, TRI)),
            (6, 2, 3)
        );
        // Lines: layer 1's 7 rows then 16 free, layer 4's 1 row then 16 free.
        assert_eq!(r.layer_slots(0, LINE, 1), 0..7);
        assert_eq!(r.layer_slots(0, LINE, 4), 23..24);
        assert_eq!(r.bytes(0, LINE).len(), (7 + 16 + 1 + 16) * 24);
        // Edges and fills are rebuilt whole: no free slots.
        assert_eq!(r.bytes(0, EDGE).len(), 8 * 24);
        assert_eq!(r.bytes(0, TRI).len(), 3 * 28);
        // The styled line: weight 3 × width 1.0 = 48/16 px, dash 7, parity snapping.
        let w = words(&r.bytes(0, LINE)[23 * 24..24 * 24]);
        assert_eq!(&w[..4], &[1, 2, 3, 4]);
        assert_eq!(
            w[4].to_ne_bytes(),
            [0x88, 0xdd, 0x00, 0xff],
            "layer 4 colour, R first"
        );
        assert_eq!(w[5], 48 | (7 << 16) | (PARITY << 20));
        assert_eq!(
            words(&r.bytes(0, LINE)[..24])[5],
            16,
            "plain: width 16/16, no flags"
        );
        // The pin rect fills by its layer (its range after layer 4's 5 + 16 slots).
        let rects = words(r.bytes(0, RECT));
        assert_eq!(rects[5], 16, "a symbol rect does not fill");
        assert_eq!(rects[21 * 6 + 5], 16 | (FILL << 20));
        // The arcs: the full circle unfilled on layer 4, the styled quarter arc open, so
        // unfilled whatever its style says; weight 2 → 32.
        let arcs = words(r.bytes(0, ARC));
        assert_eq!(&arcs[..5], &[50, 60, 7, 0, 360_000]);
        assert_eq!((arcs[6], arcs[7 + 6]), (16, 32 | (PARITY << 20)));
        // Edges by layer: the polyline's (layer 1) first, then the polygon's, which closes.
        let e = words(r.bytes(0, EDGE));
        assert_eq!(&e[..4], &[0, 0, 10, 0]);
        assert_eq!(&e[7 * 6..7 * 6 + 4], &[0, 10, 0, 0], "the polygon closes");
        // Chain flags: where each chain starts, and which edges close a polygon.
        assert_eq!((e[5], e[6 + 5]), (16 | (FIRST << 20), 16));
        let poly = 16 | ((FILL | PARITY | CLOSED) << 20);
        assert_eq!((e[3 * 6 + 5], e[4 * 6 + 5]), (poly | (FIRST << 20), poly));
        assert_eq!(
            r.dirty_bytes(),
            40 * 24 + (5 + 16 + 1 + 16) * 24 + (2 + 16) * 28 + 8 * 24 + 3 * 28
        );
    }

    /// A dead row has no slot (nothing drawn); a dead chain adds no edge and no triangle; a
    /// hidden layer draws nothing either.
    #[test]
    fn dead_rows_draw_nothing() {
        let mut palette = Palette::xschem_dark();
        palette.color[2] &= !0xff; // hidden
        let mut t = CanvasPrims::default();
        t.line(0, 0, 10, 10, 1);
        t.line(0, 0, 10, 10, 1);
        t.line(0, 0, 10, 10, 2);
        t.rect(0, 0, 5, 5, 4);
        t.arc(0, 0, 5, 0, 360_000, 4);
        t.poly(&[(0, 0), (10, 0), (10, 10)], true, 5);
        t.poly(&[(0, 0), (10, 0), (10, 10)], true, 5);
        t.kill(0, 1);
        t.kill(1, 0);
        t.kill(2, 0);
        t.kill(4, 0);
        let r = rebuilt(&t, &palette);
        assert_eq!((r.count(0, LINE), r.count(0, EDGE)), (1, 3));
        assert_eq!((r.count(0, RECT), r.count(0, ARC)), (0, 0));
        assert_eq!(r.count(0, TRI), 1, "one live filled triangle");
        assert_eq!(r.slots[0][0], [0, NO_SLOT, NO_SLOT]);
    }

    /// Reported kills and appends rewrite exactly those slots (dirty bytes = rows × stride),
    /// leave what is drawn equal to a fresh pack, and an unchanged table writes nothing; a
    /// layer whose free slots run out is repacked whole, still equal.
    #[test]
    fn killed_and_appended_rows_rewrite_exactly_those_rows() {
        let palette = Palette::xschem_dark();
        let mut t = CanvasPrims::default();
        for i in 0..20 {
            t.line(i, 0, i + 5, 5, 1);
            t.rect(i, 0, i + 5, 5, 4);
            t.arc(i, 0, 3, 0, 360_000, 4);
        }
        let mut r = rebuilt(&t, &palette);
        uploaded(&mut r);
        r.pack(0, &t, &palette, Change::Same);
        assert_eq!(r.dirty_bytes(), 0, "nothing changed, nothing written");
        t.kill(0, 3);
        t.kill(0, 17);
        t.kill(1, 5);
        t.line(100, 100, 110, 110, 1);
        t.line(200, 200, 210, 210, 1);
        t.arc(100, 100, 5, 0, 360_000, 4);
        let mut killed: [Vec<u32>; 6] = Default::default();
        killed[0].extend([3, 17]);
        killed[1].push(5);
        r.pack(0, &t, &palette, Change::Killed(&killed));
        assert_eq!(r.dirty_bytes(), 2 * 24 + 2 * 24 + 24 + 28);
        // Negative space: a "kill" report over a table that shrank repacks it, still equal.
        let mut shrunk = t.clone();
        shrunk.arcs = Default::default();
        shrunk.arc(1, 1, 3, 0, 360_000, 4);
        let mut s = rebuilt(&t, &palette);
        s.pack(0, &shrunk, &palette, Change::Killed(&Default::default()));
        same_rows(&s, &rebuilt(&shrunk, &palette), 0, "shrunk = rebuilt");
        same_rows(&r, &rebuilt(&t, &palette), 0, "patched = rebuilt");
        // Layer 1 had 20 + 18 slots, 2 taken; 16 more appends fill them, the 17th repacks.
        uploaded(&mut r);
        for i in 0..16 {
            t.line(300 + i, 0, 310 + i, 0, 1);
        }
        r.pack(0, &t, &palette, Change::Killed(&Default::default()));
        assert_eq!(r.dirty_bytes(), 16 * 24, "the free slots take them");
        uploaded(&mut r);
        t.line(400, 0, 410, 0, 1);
        r.pack(0, &t, &palette, Change::Killed(&Default::default()));
        assert!(r.dirty_bytes() > 24, "a full layer repacks");
        same_rows(&r, &rebuilt(&t, &palette), 0, "overflowed = rebuilt");
    }

    /// An edit script (rows removed and added by position, as a hashed diff reports them)
    /// repacks the table whole, equal to a fresh pack, after a removal that shrinks it too.
    #[test]
    fn edits_repack_the_table() {
        let palette = Palette::xschem_dark();
        let mut t = CanvasPrims::default();
        for i in 0..10 {
            t.line(i, 0, i + 5, 5, 1);
        }
        let mut r = rebuilt(&t, &palette);
        let (mut diff, marks) = (CanvasDiff::default(), Marks::default());
        assert!(!diff.update(&[(&t, TableChange::Any)], (&t, &marks, true)));
        let l = &mut t.lines;
        for c in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
            c.swap_remove(3);
        }
        l.layer.swap_remove(3);
        l.style.swap_remove(3);
        t.line(500, 500, 505, 505, 4);
        assert!(diff.update(&[(&t, TableChange::Any)], (&t, &marks, false)));
        r.pack(0, &t, &palette, Change::Edits(diff.edits_of(0)));
        assert_eq!(r.count(0, LINE), 10);
        same_rows(&r, &rebuilt(&t, &palette), 0, "edited = rebuilt");
    }

    /// Oracle: after random kill-and-append edits reported the way the editor's scene reports
    /// them, what is drawn equals a fresh pack of the same table, poly edges and fills
    /// included, layer order and row order within a layer.
    #[test]
    fn patched_rows_equal_rebuilt_rows() {
        let palette = Palette::xschem_dark();
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |m: i32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % m as u64) as i32
        };
        let mut t = CanvasPrims::default();
        let add = |t: &mut CanvasPrims, next: &mut dyn FnMut(i32) -> i32| {
            let (x, y) = (next(1_000_000), next(600_000));
            let layer = [1u8, 4, 5][next(3) as usize];
            match next(5) {
                0 => t.line(x, y, x + next(30_000), y + next(9_000), layer),
                1 => t.rect(x, y, x + next(20_000), y + next(20_000), layer),
                2 => t.arc(x, y, next(10_000) + 1, 0, 360_000, layer),
                3 => t.poly(
                    &[(x, y), (x + 5_000, y + 8_000), (x + 10_000, y)],
                    false,
                    layer,
                ),
                _ => t.poly_styled(
                    &[
                        (x, y),
                        (x + 5_000, y + 8_000),
                        (x + 10_000, y),
                        (x + 5_000, y - 3_000),
                    ],
                    true,
                    layer,
                    LineStyle::new(next(4) as u8 + 1, next(3) as u8),
                ),
            }
        };
        for _ in 0..500 {
            add(&mut t, &mut next);
        }
        let mut r = rebuilt(&t, &palette);
        for step in 0..60 {
            let mut killed: [Vec<u32>; 6] = Default::default();
            for _ in 0..next(5) {
                let kind = next(5) as u8;
                let n = match kind {
                    0 => t.lines.x0.len(),
                    1 => t.rects.x0.len(),
                    2 => t.arcs.cx.len(),
                    3 => t.polylines.end.len(),
                    _ => t.polygons.end.len(),
                };
                let row = next(n as i32) as usize;
                t.kill(kind, row);
                killed[usize::from(kind)].push(row as u32);
            }
            for _ in 0..next(12) {
                add(&mut t, &mut next);
            }
            r.pack(0, &t, &palette, Change::Killed(&killed));
            same_rows(&r, &rebuilt(&t, &palette), 0, &format!("step {step}"));
        }
    }

    /// The pack's 1:1 columns equal the per-row reference it replaced (each row's pen
    /// resolved from the palette on the spot, counted then written through `Table::take`),
    /// byte for byte with every slot, over random tables with dead rows, hidden layers, every
    /// style byte and partial arcs, while the palette changes under the cached pens.
    #[test]
    fn the_pack_equals_the_per_row_reference() {
        fn reference(t: &CanvasPrims, c: usize, palette: &Palette) -> (Vec<u8>, Vec<u32>) {
            let kind = ONE_TO_ONE[c];
            let n = column_len(t, c);
            let row_of = |i: usize| -> Option<(usize, [u32; 7], usize)> {
                let (layer, style, closed, f): (u8, LineStyle, bool, Vec<i32>) = match c {
                    0 => {
                        let l = &t.lines;
                        (
                            l.layer[i],
                            l.style[i],
                            false,
                            vec![l.x0[i], l.y0[i], l.x1[i], l.y1[i]],
                        )
                    }
                    1 => {
                        let r = &t.rects;
                        (
                            r.layer[i],
                            r.style[i],
                            true,
                            vec![r.x0[i], r.y0[i], r.x1[i], r.y1[i]],
                        )
                    }
                    _ => {
                        let a = &t.arcs;
                        let full = a.sweep[i].abs() >= 360_000;
                        let f = vec![a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i]];
                        (a.layer[i], a.style[i], full, f)
                    }
                };
                let Pen {
                    color: co,
                    style: st,
                } = pen(palette, layer, style, closed)?;
                let mut w = [0u32; 7];
                for (k, v) in f.iter().enumerate() {
                    w[k] = *v as u32;
                }
                (w[f.len()], w[f.len() + 1]) = (co, st);
                Some((usize::from(layer), w, f.len() + 2))
            };
            let mut count = [0u32; LAYERS];
            for i in 0..n {
                if let Some((l, ..)) = row_of(i) {
                    count[l] += 1;
                }
            }
            let mut table = Table::default();
            table.layout(kind, &count, true);
            let mut slots = Vec::new();
            for i in 0..n {
                slots.push(match row_of(i) {
                    Some((l, w, len)) => {
                        let s = table.take(l).expect("laid out");
                        row(&mut table.bytes, STRIDE[kind], s as usize, &w[..len]);
                        s
                    }
                    None => NO_SLOT,
                });
            }
            (table.bytes, slots)
        }
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |m: u32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % u64::from(m)) as i32
        };
        let mut packed = InstanceRows::default();
        for round in 0..40 {
            let mut palette = Palette::xschem_dark();
            palette.color[next(8) as usize] &= !0xff; // a hidden layer
            palette.fill ^= 1 << next(8);
            palette.width[next(8) as usize] = next(5) as f32 + 0.5;
            let mut t = CanvasPrims::default();
            for _ in 0..next(400) {
                let (x, y) = (next(1_000_000), next(600_000));
                let layer = next(8) as u8;
                let style = LineStyle::new(next(5) as u8, next(16) as u8);
                let style = if next(3) == 0 { style.filled() } else { style };
                match next(3) {
                    0 => t.line_styled(x, y, x + next(30_000), y - next(9_000), layer, style),
                    1 => t.rect_styled(x, y, x + next(20_000), y + next(20_000), layer, style),
                    _ => {
                        let sweep = [360_000, -360_000, 90_000, -45_000][next(4) as usize];
                        t.arc_styled(x, y, next(10_000) + 1, next(360_000), sweep, layer, style)
                    }
                }
            }
            for _ in 0..next(20) {
                let kind = next(3) as u8;
                let n = [t.lines.x0.len(), t.rects.x0.len(), t.arcs.cx.len()][usize::from(kind)];
                if n > 0 {
                    t.kill(kind, next(n as u32) as usize);
                }
            }
            packed.pack(0, &t, &palette, Change::All);
            for (c, &kind) in ONE_TO_ONE.iter().enumerate() {
                let (bytes, slots) = reference(&t, c, &palette);
                assert_eq!(
                    packed.bytes(0, kind),
                    &bytes[..],
                    "round {round}, column {c}"
                );
                assert_eq!(
                    packed.slots[0][c], slots,
                    "round {round}, column {c}: slots"
                );
            }
        }
    }
}
