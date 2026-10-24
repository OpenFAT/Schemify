//! The GPU canvas (D39; `docs/dev/research/render-backend.md` § 5, step 1): the canvas tables
//! packed into retained instance rows ([`InstanceRows`]), patched where the tables' reported
//! changes say, for instanced pipelines to draw under one camera uniform (the next
//! change). A pan or zoom writes the uniform only; an edit rewrites the rows it changed.
//! Every frame is complete: this path has no tile cache.
//!
//! One producer, two consumers: the pack here is the one place a per-row decision is made
//! (dead and hidden rows draw nothing, colour and stroke width from the palette, fills, the
//! marks' recolouring, the drawing order); the pipelines consume the rows now, the CPU
//! painter is to follow (step 2). Culling to the view is the GPU's clip, and the far-zoom cut
//! (a row at most 2 px across is the whole pixels it touches, as `canvas::draw_prims` draws
//! it) is a pure function of (row, zoom) evaluated in the vertex shader. Text stays with
//! vello until the glyph atlas (step 3).
//!
//! Six questions:
//! 1. In: the four canvas tables of a frame ([`CanvasPrims`]: the scene as shown, decor,
//!    violation markers, the parity extra) with how each changed since the last pack
//!    ([`Change`]), the selection marks, the palette. Out: per source (the four tables, then
//!    the marks) and kind ([`KINDS`]) a byte array of instance rows grouped by palette layer,
//!    each layer's slot range, and the byte ranges written since the last upload; per frame
//!    one camera ([`Frame`]).
//! 2. ≤ 2^22 rows per table kind (`CanvasPrims::CAPACITY`), 5 sources × 5 kinds of row
//!    arrays, each in ≤ 32 layer ranges; a frame is one draw per non-empty (source, layer,
//!    kind) plus the background: about 40 for a schematic.
//! 3. Rows are AoS (the GPU fetches one row per instance): coordinates `i32` milli-units
//!    (D33); colour `u32`, RGBA8 in memory order, all zero in an empty slot; a style word of
//!    stroke width `u16` in 1/16 px (layer width × weight ≤ 4, under 4096 px), dash 4 bits
//!    (0–15, `LineStyle`) and flags 4 bits. A segment or rect row is 24 B, an arc row or a
//!    fill triangle 28 B ([`STRIDE`]).
//! 4. The GPU reads every row each frame, layer by layer: the CPU painter's order (source by
//!    source, layer by layer), which decides how antialiased edges of different layers blend.
//!    The CPU writes only the slots a [`Change`] names. The tables are SoA, so a pack is one
//!    counting pass and one writing pass per kind.
//! 5. Rows live with the window's canvas until a table is rebuilt ([`Change::All`]: a document
//!    switch, a palette change, a diff that gave up, a layer whose free slots ran out); each
//!    layer range keeps an eighth of its rows plus 16 free slots for appended rows. The frame
//!    request lives one frame; GPU buffers grow by doubling and never shrink.
//! 6. Sources and kinds are independent; serial (about 4 ms per 10^5 rows packed whole, an
//!    edit's patch O(change)).

use std::ops::Range;

use vello::kurbo::Rect;

use schemify_canvas::{
    Arcs, Camera, CanvasPrims, LAYER_DEAD, LAYERS, LineStyle, Lines, Marks, Palette, PrimKind,
    chain,
};

/// Instance kinds, in drawing order within a layer (fills under strokes).
pub const KINDS: usize = 5;
/// Polygon fill triangles.
pub const TRI: usize = 0;
/// Rectangles (outline, fill).
pub const RECT: usize = 1;
/// Arcs and circles.
pub const ARC: usize = 2;
/// Lines and wires, one per table row.
pub const LINE: usize = 3;
/// Polyline and polygon edges.
pub const EDGE: usize = 4;
/// Bytes per row of each kind.
pub const STRIDE: [usize; KINDS] = [28, 24, 28, 24, 24];
/// Row sources: the four canvas tables in drawing order, then the marks.
pub const SOURCES: usize = 5;
/// The marks source ([`InstanceRows::pack_marks`]).
pub const MARKS: usize = 4;
/// Most bytes one row array holds: wgpu's default buffer limit (256 MiB).
const BYTES_MAX: usize = 1 << 28;
/// Dirty ranges kept per row array before they merge into one.
const DIRTY_MAX: usize = 8;
/// A table row with no slot (dead, hidden, or nothing to draw).
const NO_SLOT: u32 = u32::MAX;

/// Style flags, bits 20–23 of a row's style word (the shader): a closed shape also filled.
/// Every stroke has round caps and joins, as kurbo's `Stroke::new` and `canvas::stroke_of`
/// draw them on the CPU.
const FILL: u32 = 1;
/// Snap to a pixel centre or edge by the rounded width's parity (`canvas::styled`); else to
/// a centre.
const PARITY: u32 = 2;
/// A mark's stroke: 2 logical px (`canvas::draw_marks`), in 1/16 px.
const MARK_WIDTH_Q16: u32 = 32;

const _: () = assert!(LineStyle::DASH_MAX < 16, "the dash is a nibble");

/// A row's style word: stroke width in 1/16 px, dash, flags.
fn style_word(q16: u32, dash: u8, flags: u32) -> u32 {
    assert!(q16 <= 0xffff && dash < 16 && flags < 16, "fields in range");
    q16 | (u32::from(dash) << 16) | (flags << 20)
}

/// One frame's canvas: what the window draws the rows with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The canvas rectangle, logical px.
    pub view: Rect,
    /// The camera.
    pub cam: Camera,
    /// Grid pitch in milli-units (`i32::MAX / 4`: no grid), doubled as `canvas::draw_grid`
    /// doubles it.
    pub grid: i32,
    /// Background colour, `0xRRGGBBAA`.
    pub background: u32,
    /// Grid dot colour, `0xRRGGBBAA`.
    pub grid_color: u32,
}

/// How a source table changed since the [`InstanceRows`] last packed it.
#[derive(Clone, Copy, Debug)]
pub enum Change<'a> {
    /// Not at all.
    Same,
    /// Beyond what a patch follows (a rebuilt table, a palette change): repack it whole.
    All,
    /// Rows killed (`killed[kind]` in [`CanvasPrims::kill`]'s kind order, now on
    /// [`LAYER_DEAD`]) and rows appended past the previous length; every other row as it was
    /// (`TableChange::Rows`).
    Killed(&'a [Vec<u32>; 6]),
    /// Rows removed (old indices) and added (new indices) per kind, as
    /// `CanvasDiff::edits_of` reports them.
    // ponytail: repacked whole (rows after an edit may have moved, so the slot map would need
    // the script replayed); only the decor and the parity extra report edits, and both are
    // small next to the scene. Replay the script into the slot map if a large table does.
    Edits([(&'a [u32], &'a [u32]); 6]),
}

/// One row array: rows grouped by palette layer, each layer a slot range with free slots at
/// its end, and what was written since the GPU last read it.
#[derive(Clone, Debug, Default)]
struct Table {
    bytes: Vec<u8>,
    /// Per layer: first slot, slots in use (drawn), slots reserved.
    start: [u32; LAYERS],
    used: [u32; LAYERS],
    cap: [u32; LAYERS],
    dirty: Vec<Range<usize>>,
}

impl Table {
    /// Lays the table out for `count[l]` rows per layer, with free slots after each non-empty
    /// layer's rows when `slack`; every slot zero (empty), every layer unused, all dirty.
    fn layout(&mut self, kind: usize, count: &[u32; LAYERS], slack: bool) {
        let mut at = 0u32;
        for (l, &n) in count.iter().enumerate() {
            let free = if slack && n > 0 { n / 8 + 16 } else { 0 };
            (self.start[l], self.used[l], self.cap[l]) = (at, 0, n + free);
            at += n + free;
        }
        let len = at as usize * STRIDE[kind];
        assert!(len <= BYTES_MAX, "rows fit wgpu's buffer limit");
        self.bytes.clear();
        self.bytes.resize(len, 0);
        self.dirty.clear();
        push_dirty(&mut self.dirty, 0..len);
    }

    /// Takes the next free slot of layer `l`, or `None` when its range is full.
    fn take(&mut self, l: usize) -> Option<u32> {
        (self.used[l] < self.cap[l]).then(|| {
            self.used[l] += 1;
            self.start[l] + self.used[l] - 1
        })
    }

    /// Writes `fields` into slot `s` of this `kind`'s rows and notes it dirty.
    fn put(&mut self, kind: usize, s: u32, fields: &[u32]) {
        let at = s as usize * STRIDE[kind];
        row(&mut self.bytes, STRIDE[kind], s as usize, fields);
        push_dirty(&mut self.dirty, at..at + STRIDE[kind]);
    }
}

/// The instance rows of one canvas: per source and kind the rows grouped by layer, the slot
/// of each table row, and the byte ranges written since the GPU last read them
/// (the pipelines' upload). Headless: plain byte tables, so the pack is tested without a GPU.
#[derive(Default)]
pub struct InstanceRows {
    tables: [[Table; KINDS]; SOURCES],
    /// Per source: the slot of each lines, rects and arcs row ([`NO_SLOT`]: none).
    slots: [[Vec<u32>; 3]; SOURCES],
    /// Per source: ever packed, and the lengths of its lines, rects, arcs, polylines and
    /// polygons tables at the last pack.
    packed: [bool; SOURCES],
    counts: [[usize; 5]; SOURCES],
    ears: Vec<[u32; 3]>,
    order: Vec<u32>,
    /// The frame to draw, set by the painter's canvas call and taken by the window.
    pub frame: Option<Frame>,
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

/// The kind each 1:1 table column feeds: lines, rects, arcs.
const ONE_TO_ONE: [usize; 3] = [LINE, RECT, ARC];

impl InstanceRows {
    /// Slots of kind `kind` from source `src` in use (drawn; an emptied slot draws nothing).
    pub fn count(&self, src: usize, kind: usize) -> usize {
        self.tables[src][kind]
            .used
            .iter()
            .map(|&u| u as usize)
            .sum()
    }

    /// The rows of kind `kind` from source `src`, `STRIDE[kind]` bytes per slot, every layer
    /// range in turn (free slots zero).
    pub fn bytes(&self, src: usize, kind: usize) -> &[u8] {
        &self.tables[src][kind].bytes
    }

    /// The slots of layer `layer` of kind `kind` from source `src` that are drawn.
    pub fn layer_slots(&self, src: usize, kind: usize, layer: usize) -> Range<u32> {
        let t = &self.tables[src][kind];
        t.start[layer]..t.start[layer] + t.used[layer]
    }

    /// The byte ranges of `(src, kind)` written since the GPU last read the rows.
    pub fn dirty(&self, src: usize, kind: usize) -> &[Range<usize>] {
        &self.tables[src][kind].dirty
    }

    /// Bytes the GPU has to read to be current: the dirty ranges, clipped to the rows.
    pub fn dirty_bytes(&self) -> usize {
        let mut n = 0;
        for t in self.tables.iter().flatten() {
            let len = t.bytes.len();
            n += t
                .dirty
                .iter()
                .map(|r| r.end.min(len).saturating_sub(r.start.min(len)))
                .sum::<usize>();
        }
        n
    }

    /// Packs source table `src` (one of the four canvas tables, `< MARKS`) of `t` after
    /// `change`, resolving each row's colour and width from `palette`. A [`Change::Killed`]
    /// empties the killed rows' slots and puts appended rows into their layer's free slots,
    /// writing only those; a layer out of free slots, a table that shrank, an edit script,
    /// [`Change::All`] or a source never packed repack it whole. Polyline and polygon edges and fills are repacked
    /// whole when either chain table changed.
    ///
    /// Cost: O(rows written); [`Change::Same`] writes none.
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ MARKS`.
    pub fn pack(&mut self, src: usize, t: &CanvasPrims, palette: &Palette, change: Change<'_>) {
        assert!(src < MARKS, "the marks pack through `pack_marks`");
        let n = [
            t.lines.x0.len(),
            t.rects.x0.len(),
            t.arcs.cx.len(),
            t.polylines.end.len(),
            t.polygons.end.len(),
        ];
        assert!(n.iter().all(|&c| c <= CanvasPrims::CAPACITY));
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
                Change::Killed(k) if !whole => self.patch(src, c, t, palette, k),
                Change::Same if !whole => true,
                _ => false,
            };
            if !patched {
                self.repack(src, c, t, palette);
            }
        }
        if whole || polys {
            self.repack_polys(src, t, palette);
        }
        self.counts[src] = n;
        self.packed[src] = true;
        assert!(
            (0..3).all(|c| self.slots[src][c].len() == n[c]),
            "a slot per table row"
        );
    }

    /// Repacks 1:1 column `c` (0 lines, 1 rects, 2 arcs) of source `src` whole.
    fn repack(&mut self, src: usize, c: usize, t: &CanvasPrims, palette: &Palette) {
        let kind = ONE_TO_ONE[c];
        let n = column_len(t, c);
        let mut count = [0u32; LAYERS];
        for i in 0..n {
            if let Some((l, ..)) = one_row(t, c, i, palette) {
                count[l] += 1;
            }
        }
        let table = &mut self.tables[src][kind];
        table.layout(kind, &count, true);
        let slots = &mut self.slots[src][c];
        slots.clear();
        for i in 0..n {
            let s = match one_row(t, c, i, palette) {
                Some((l, fields, len)) => {
                    let s = table.take(l).expect("laid out for every row");
                    row(&mut table.bytes, STRIDE[kind], s as usize, &fields[..len]);
                    s
                }
                None => NO_SLOT,
            };
            slots.push(s);
        }
        assert_eq!(table.used, count, "every live row placed");
    }

    /// Follows `killed` and the rows appended to column `c` of source `src`. False when the
    /// column shrank or a layer ran out of free slots: the caller then repacks it.
    fn patch(
        &mut self,
        src: usize,
        c: usize,
        t: &CanvasPrims,
        palette: &Palette,
        killed: &[Vec<u32>; 6],
    ) -> bool {
        let kind = ONE_TO_ONE[c];
        let (was, n) = (self.slots[src][c].len(), column_len(t, c));
        if n < was {
            return false; // removed rows: not a kill-and-append edit after all
        }
        let (table, slots) = (&mut self.tables[src][kind], &mut self.slots[src][c]);
        for &r in &killed[c] {
            let r = r as usize;
            if r < was && slots[r] != NO_SLOT {
                table.put(kind, slots[r], &[0; 7][..STRIDE[kind] / 4]);
                slots[r] = NO_SLOT;
            }
        }
        for i in was..n {
            let s = match one_row(t, c, i, palette) {
                Some((l, fields, len)) => {
                    let Some(s) = table.take(l) else {
                        return false;
                    };
                    table.put(kind, s, &fields[..len]);
                    s
                }
                None => NO_SLOT,
            };
            slots.push(s);
        }
        true
    }

    /// Repacks the polyline and polygon edges and the polygon fills of source `src` whole,
    /// layer by layer (no free slots: they are rebuilt on any chain change).
    fn repack_polys(&mut self, src: usize, t: &CanvasPrims, palette: &Palette) {
        // The live chains, polylines then polygons, each in row order; the layer ranges put
        // them in drawing order (one colour per layer, so only the layer order shows).
        let order = &mut self.order;
        order.clear();
        let (mut edges, mut tris) = ([0u32; LAYERS], [0u32; LAYERS]);
        for (closed, p) in [(false, &t.polylines), (true, &t.polygons)] {
            for k in 0..p.end.len() {
                let Some((_, s)) = pen(palette, p.layer[k], p.style[k], closed) else {
                    continue;
                };
                let l = usize::from(p.layer[k]);
                let m = chain(&p.end, k).len() as u32;
                assert!(m >= 2, "malformed poly table");
                edges[l] += if closed { m } else { m - 1 };
                if closed && s & (FILL << 20) != 0 {
                    tris[l] += m.saturating_sub(2);
                }
                order.push((u32::from(closed) << 31) | k as u32);
            }
        }
        let [tri, .., edge] = &mut self.tables[src];
        edge.layout(EDGE, &edges, false);
        tri.layout(TRI, &tris, false);
        for &o in order.iter() {
            let (closed, k) = (o >> 31 == 1, (o & !(1 << 31)) as usize);
            let p = if closed { &t.polygons } else { &t.polylines };
            let (c, s) = pen(palette, p.layer[k], p.style[k], closed).expect("live");
            let l = usize::from(p.layer[k]);
            let pts = chain(&p.end, k);
            let m = pts.len();
            for j in 0..if closed { m } else { m - 1 } {
                let (a, b) = (pts.start + j, pts.start + (j + 1) % m);
                let f = [p.x[a], p.y[a], p.x[b], p.y[b]].map(|v| v as u32);
                let at = edge.take(l).expect("laid out") as usize;
                row(
                    &mut edge.bytes,
                    STRIDE[EDGE],
                    at,
                    &[f[0], f[1], f[2], f[3], c, s],
                );
            }
            if closed && s & (FILL << 20) != 0 && m >= 3 {
                let (xs, ys) = (&p.x[pts.clone()], &p.y[pts]);
                ear_clip(xs, ys, &mut self.ears);
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

    /// Packs the marks source whole when `changed` (the marks or `scene` since the last call)
    /// or never packed: every live marked row of `scene` again, in its mark colour, 2 px wide
    /// and unfilled (`canvas::draw_marks`). Each run of marks of one colour is a range of its
    /// own, drawn in mark order, as the CPU strokes a run as one path after the one before.
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
        let range = |i: usize| runs[i].min(LAYERS - 1);
        let mut count = [[0u32; LAYERS]; KINDS];
        for i in (0..n).filter(|&i| live(i)) {
            let k = marks.index[i] as usize;
            let (kind, rows) = match marks.kind[i] {
                PrimKind::Line => (LINE, 1),
                PrimKind::Rect => (RECT, 1),
                PrimKind::Arc => (ARC, 1),
                PrimKind::Polyline => (EDGE, chain(&scene.polylines.end, k).len() as u32 - 1),
                PrimKind::Polygon => (EDGE, chain(&scene.polygons.end, k).len() as u32),
            };
            count[kind][range(i)] += rows;
        }
        let tables = &mut self.tables[MARKS];
        for (kind, t) in tables.iter_mut().enumerate() {
            t.layout(kind, &count[kind], false);
        }
        let style = style_word(MARK_WIDTH_Q16, 0, 0);
        let mut put = |kind: usize, run: usize, fields: &[u32]| {
            let t = &mut tables[kind];
            let s = t.take(run).expect("laid out") as usize;
            row(&mut t.bytes, STRIDE[kind], s, fields);
        };
        for i in (0..n).filter(|&i| live(i)) {
            let k = marks.index[i] as usize;
            let color = marks.color[i].swap_bytes();
            match marks.kind[i] {
                PrimKind::Line => {
                    let l = &scene.lines;
                    let f = [l.x0[k], l.y0[k], l.x1[k], l.y1[k]].map(|v| v as u32);
                    put(LINE, range(i), &[f[0], f[1], f[2], f[3], color, style]);
                }
                PrimKind::Rect => {
                    let r = &scene.rects;
                    let f = [r.x0[k], r.y0[k], r.x1[k], r.y1[k]].map(|v| v as u32);
                    put(RECT, range(i), &[f[0], f[1], f[2], f[3], color, style]);
                }
                PrimKind::Arc => {
                    let a = &scene.arcs;
                    let f = [a.cx[k], a.cy[k], a.r[k], a.start[k], a.sweep[k]].map(|v| v as u32);
                    put(ARC, range(i), &[f[0], f[1], f[2], f[3], f[4], color, style]);
                }
                PrimKind::Polyline | PrimKind::Polygon => {
                    let closed = marks.kind[i] == PrimKind::Polygon;
                    let p = if closed {
                        &scene.polygons
                    } else {
                        &scene.polylines
                    };
                    let pts = chain(&p.end, k);
                    let m = pts.len();
                    for j in 0..if closed { m } else { m - 1 } {
                        let (a, b) = (pts.start + j, pts.start + (j + 1) % m);
                        let f = [p.x[a], p.y[a], p.x[b], p.y[b]].map(|v| v as u32);
                        put(EDGE, range(i), &[f[0], f[1], f[2], f[3], color, style]);
                    }
                }
            }
        }
        assert!(
            tables.iter().zip(&count).all(|(t, c)| t.used == *c),
            "every mark placed"
        );
        self.packed[MARKS] = true;
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

/// A packed row: its layer, its fields and how many of them are used.
type Packed = (usize, [u32; 7], usize);

/// Row `i` of 1:1 column `c` packed, or `None` when it draws nothing (dead, or its layer
/// hidden).
fn one_row(t: &CanvasPrims, c: usize, i: usize, palette: &Palette) -> Option<Packed> {
    match c {
        0 => line_row(&t.lines, i, palette),
        1 => rect_row(&t.rects, i, palette),
        2 => arc_row(&t.arcs, i, palette),
        _ => unreachable!("three 1:1 columns"),
    }
}

/// Notes `r` as written; past [`DIRTY_MAX`] ranges they merge into their span.
fn push_dirty(d: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.is_empty() {
        return;
    }
    d.push(r);
    if d.len() > DIRTY_MAX {
        let lo = d.iter().map(|r| r.start).min().expect("ranges");
        let hi = d.iter().map(|r| r.end).max().expect("ranges");
        d.clear();
        d.push(lo..hi);
    }
    assert!(d.len() <= DIRTY_MAX);
}

/// Writes `fields` as row `i` of a `stride`-byte row array (which holds it).
fn row(buf: &mut [u8], stride: usize, i: usize, fields: &[u32]) {
    assert_eq!(fields.len() * 4, stride, "a whole row");
    let at = i * stride;
    for (k, f) in fields.iter().enumerate() {
        buf[at + 4 * k..at + 4 * k + 4].copy_from_slice(&f.to_ne_bytes());
    }
}

/// A row's colour (RGBA bytes in memory order) and style word ([`style_word`]) on `layer` in
/// `style`; `closed` lets the layer's or the style's fill apply. `None` for a dead row
/// ([`LAYER_DEAD`]) or a hidden layer (colour alpha 0): nothing drawn.
fn pen(palette: &Palette, layer: u8, style: LineStyle, closed: bool) -> Option<(u32, u32)> {
    let l = usize::from(layer);
    if l >= LAYERS || palette.color[l] & 0xff == 0 {
        return None;
    }
    let w = f64::from(palette.width[l]) * f64::from(style.weight());
    assert!(w > 0.0, "zero-width layer");
    let q = ((w * 16.0).round() as u32).clamp(1, 0xffff);
    let fill = closed && (palette.fill & (1 << l) != 0 || style.fills());
    let mut f = 0;
    if style != LineStyle::PLAIN {
        f |= PARITY;
    }
    if fill {
        f |= FILL;
    }
    Some((
        palette.color[l].swap_bytes(),
        style_word(q, style.dash(), f),
    ))
}

fn line_row(l: &Lines, i: usize, palette: &Palette) -> Option<Packed> {
    let (c, s) = pen(palette, l.layer[i], l.style[i], false)?;
    let f = [l.x0[i], l.y0[i], l.x1[i], l.y1[i]].map(|v| v as u32);
    Some((
        usize::from(l.layer[i]),
        [f[0], f[1], f[2], f[3], c, s, 0],
        6,
    ))
}

fn rect_row(r: &Lines, i: usize, palette: &Palette) -> Option<Packed> {
    let (c, s) = pen(palette, r.layer[i], r.style[i], true)?;
    let f = [r.x0[i], r.y0[i], r.x1[i], r.y1[i]].map(|v| v as u32);
    Some((
        usize::from(r.layer[i]),
        [f[0], f[1], f[2], f[3], c, s, 0],
        6,
    ))
}

fn arc_row(a: &Arcs, i: usize, palette: &Palette) -> Option<Packed> {
    let full = a.sweep[i].abs() >= 360_000;
    let (c, s) = pen(palette, a.layer[i], a.style[i], full)?;
    let f = [a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i]].map(|v| v as u32);
    Some((
        usize::from(a.layer[i]),
        [f[0], f[1], f[2], f[3], f[4], c, s],
        7,
    ))
}

/// Twice the signed area of triangle `a b c`.
fn cross(xs: &[i32], ys: &[i32], a: usize, b: usize, c: usize) -> i64 {
    let (ax, ay) = (i64::from(xs[a]), i64::from(ys[a]));
    (i64::from(xs[b]) - ax) * (i64::from(ys[c]) - ay)
        - (i64::from(ys[b]) - ay) * (i64::from(xs[c]) - ax)
}

/// Triangulates the simple polygon (`xs`, `ys`), at least 3 points, by ear clipping into
/// `out` as point-index triples. A polygon with no ear left (degenerate or
/// self-intersecting) is fanned off its next vertex, so the output is always `n − 2`
/// triangles. O(n²) per ear.
fn ear_clip(xs: &[i32], ys: &[i32], out: &mut Vec<[u32; 3]>) {
    let n = xs.len();
    assert!(n >= 3 && ys.len() == n, "a polygon of 3 points or more");
    out.clear();
    let mut idx: Vec<u32> = (0..n as u32).collect();
    let area2: i64 = (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            i64::from(xs[i]) * i64::from(ys[j]) - i64::from(xs[j]) * i64::from(ys[i])
        })
        .sum();
    let sign = area2.signum();
    let inside = |k: usize, a: usize, b: usize, c: usize| {
        let (u, v, w) = (
            cross(xs, ys, a, b, k) * sign,
            cross(xs, ys, b, c, k) * sign,
            cross(xs, ys, c, a, k) * sign,
        );
        u >= 0 && v >= 0 && w >= 0
    };
    let mut steps = 0;
    while idx.len() > 3 {
        steps += 1;
        assert!(steps <= n, "one vertex clipped per step");
        let m = idx.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (
                idx[(i + m - 1) % m] as usize,
                idx[i] as usize,
                idx[(i + 1) % m] as usize,
            );
            cross(xs, ys, a, b, c) * sign > 0
                && !idx.iter().any(|&k| {
                    let k = k as usize;
                    k != a && k != b && k != c && inside(k, a, b, c)
                })
        });
        let i = ear.unwrap_or(1);
        out.push([idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]]);
        idx.remove(i);
    }
    out.push([idx[0], idx[1], idx[2]]);
    assert_eq!(out.len(), n - 2, "a simple polygon has n − 2 triangles");
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemify_canvas::{CanvasDiff, TableChange};

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
        r.tables.iter_mut().flatten().for_each(|t| t.dirty.clear());
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
    /// with an eighth plus 16 free slots after each; a row's fields sit where `gpu.wgsl`
    /// reads them: coordinates, colour bytes R G B A, width in 1/16 px, dash, flags.
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
        let (mut diff, mut boxes, marks) = (CanvasDiff::default(), Vec::new(), Marks::default());
        assert!(!diff.update(&[(&t, TableChange::Any)], (&t, &marks, true), &mut boxes));
        let l = &mut t.lines;
        for c in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
            c.swap_remove(3);
        }
        l.layer.swap_remove(3);
        l.style.swap_remove(3);
        t.line(500, 500, 505, 505, 4);
        assert!(diff.update(&[(&t, TableChange::Any)], (&t, &marks, false), &mut boxes));
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
        uploaded(&mut r);
        r.pack_marks(&t, &m, false);
        assert_eq!(r.dirty_bytes(), 0, "unchanged marks write nothing");
    }

    /// Oracle: the triangles of a concave polygon cover its area exactly (the shoelace area),
    /// whichever way it winds.
    #[test]
    fn ear_clipping_covers_the_polygon() {
        let shapes: [&[(i32, i32)]; 3] = [
            &[(0, 0), (10, 0), (10, 10), (5, 4), (0, 10)],
            &[(0, 0), (6, 0), (6, 2), (2, 2), (2, 6), (0, 6)],
            &[(0, 0), (4, 0), (4, 4), (0, 4)],
        ];
        for pts in shapes {
            for reversed in [false, true] {
                let mut p: Vec<(i32, i32)> = pts.to_vec();
                if reversed {
                    p.reverse();
                }
                let xs: Vec<i32> = p.iter().map(|q| q.0).collect();
                let ys: Vec<i32> = p.iter().map(|q| q.1).collect();
                let mut out = Vec::new();
                ear_clip(&xs, &ys, &mut out);
                let area2: i64 = out
                    .iter()
                    .map(|&[a, b, c]| cross(&xs, &ys, a as usize, b as usize, c as usize).abs())
                    .sum();
                let n = xs.len();
                let want: i64 = (0..n)
                    .map(|i| {
                        let j = (i + 1) % n;
                        i64::from(xs[i]) * i64::from(ys[j]) - i64::from(xs[j]) * i64::from(ys[i])
                    })
                    .sum::<i64>()
                    .abs();
                assert_eq!(area2, want, "{pts:?} reversed {reversed}");
                assert_eq!(out.len(), n - 2);
            }
        }
    }
}
