//! The GPU canvas (D39; `docs/dev/research/render-backend.md` § 5, step 1): the canvas tables
//! packed into retained instance rows ([`InstanceRows`]), patched where the tables' reported
//! changes say, and drawn by instanced pipelines ([`Pipelines`]) under one camera uniform
//! (`gpu.wgsl`). A pan or zoom writes the uniform only; an edit rewrites the rows it changed.
//! Every frame is complete: there is no tile cache.
//!
//! One producer, two consumers: the pack here is the one place a per-row decision is made
//! (dead and hidden rows draw nothing, colour and stroke width from the palette, fills, the
//! marks' recolouring, the drawing order). The pipelines consume the rows on the GPU;
//! `canvas::draw_rows` consumes the same rows on the CPU (`CpuPainter`, the goldens).
//! Culling to the view and the far-zoom cut (a row at most 2 px across is the whole pixels it
//! touches) are pure functions of (row, camera), evaluated by each consumer: the vertex shader
//! per row, `draw_rows` per row and per chain. Texts are not rows: they are the frame batch's
//! atlas glyphs (`batch.rs`, step 3), which [`Pipelines`] draws in the same pass, each canvas
//! stage at its place among the batch's instances.
//!
//! Six questions:
//! 1. In: the four canvas tables of a frame ([`CanvasPrims`]: the scene as shown, decor,
//!    violation markers, the parity extra) with how each changed since the last pack
//!    ([`Change`]), the selection marks, the gesture overlay, the palette. Out: per source
//!    (the four tables, the marks, the overlay) and kind ([`KINDS`]) a byte array of
//!    instance rows grouped by palette layer, each layer's slot range, and the byte ranges
//!    written since the last upload; per frame one camera ([`Frame`]).
//! 2. ≤ 2^22 rows per table kind (`CanvasPrims::CAPACITY`), 6 sources × 5 kinds of row
//!    arrays, each in ≤ 32 layer ranges; a frame is one draw per non-empty (source, layer,
//!    kind) plus the background: about 40 for a schematic.
//! 3. Rows are AoS (the GPU fetches one row per instance): coordinates `i32` milli-units
//!    (D33); colour `u32`, RGBA8 in memory order, all zero in an empty slot; a style word of
//!    stroke width `u16` in 1/16 px (layer width × weight ≤ 4, under 4096 px), dash 4 bits
//!    (0–15, `LineStyle`) and flags 4 bits (fill, parity snapping, and on an edge where its
//!    chain starts and whether it closes). A segment or rect row is 24 B, an arc row or a
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

use std::collections::HashMap;
use std::ops::Range;

use kurbo::Rect;

use crate::atlas::ATLAS_SIDE;
use crate::batch::{self, Batch, Stage, What};

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
/// Row sources: the four canvas tables in drawing order, the marks, the gesture overlay.
pub const SOURCES: usize = 6;
/// The marks source ([`InstanceRows::pack_marks`]).
pub const MARKS: usize = 4;
/// The gesture overlay's source (`Painter::overlay`), packed whole each frame it shows.
pub const OVERLAY: usize = 5;
/// Most bytes one row array holds: wgpu's default buffer limit (256 MiB).
const BYTES_MAX: usize = 1 << 28;
/// Dirty ranges kept per row array before they merge into one.
const DIRTY_MAX: usize = 8;
/// The canvas pipelines' WGSL module, exactly as `Pipelines::new` hands it to wgpu (the
/// WebGL2 test validates this string, whatever files it is assembled from).
const CANVAS_WGSL: &str = include_str!("gpu.wgsl");
/// The batch pipeline's WGSL module, as `Pipelines::new` hands it to wgpu.
const UI_WGSL: &str = include_str!("ui.wgsl");

/// Camera uniform size (`Cam` in `gpu.wgsl`).
const UNIFORM_BYTES: usize = 96;
/// A table row with no slot (dead, hidden, or nothing to draw).
const NO_SLOT: u32 = u32::MAX;

/// Style flags, bits 20–23 of a row's style word (`gpu.wgsl`): a closed shape also filled.
/// Every stroke has round caps and joins, as kurbo's `Stroke::new` and `canvas::stroke_of`
/// draw them on the CPU.
pub const FILL: u32 = 1;
/// Snap to a pixel centre or edge by the rounded width's parity (`canvas::styled`); else to
/// a centre.
pub const PARITY: u32 = 2;
/// On an edge row: the first edge of its chain (a polyline or polygon), so the CPU painter
/// strokes the chain as the one path it is (joins, a continuous dash). The shader ignores it.
pub const FIRST: u32 = 4;
/// On an edge row: its chain is a polygon (closed), so its last edge is the closing one.
pub const CLOSED: u32 = 8;
/// A mark's stroke: 2 logical px (`canvas::draw_marks`), in 1/16 px.
const MARK_WIDTH_Q16: u32 = 32;

const _: () = assert!(
    UNIFORM_BYTES.is_multiple_of(16),
    "the uniform is vec4-aligned"
);
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
/// ([`Pipelines::upload`]). Headless: plain byte tables, so the pack is tested without a GPU.
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

    /// Slot `slot` of kind `kind` from source `src` as its `STRIDE[kind] / 4` words, the rest
    /// zero (`gpu.wgsl`'s fields: coordinates, then colour and style word; a fill triangle's
    /// colour last). An empty slot is all zero.
    ///
    /// # Panics
    ///
    /// Panics on a slot past the rows.
    #[inline]
    pub fn row(&self, src: usize, kind: usize, slot: u32) -> [u32; 7] {
        let b = &self.tables[src][kind].bytes[slot as usize * STRIDE[kind]..][..STRIDE[kind]];
        let mut w = [0u32; 7];
        for (k, c) in b.chunks_exact(4).enumerate() {
            w[k] = u32::from_ne_bytes(c.try_into().expect("4 bytes"));
        }
        w
    }

    /// Forgets the dirty ranges, as an upload does: for a consumer that reads the rows in
    /// place (the CPU painter), and for tests that count the bytes one frame writes.
    pub fn clear_dirty(&mut self) {
        self.tables
            .iter_mut()
            .flatten()
            .for_each(|t| t.dirty.clear());
        assert_eq!(self.dirty_bytes(), 0);
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

    /// Packs source table `src` (one of the four canvas tables or the overlay, not
    /// [`MARKS`]) of `t` after
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
    /// Panics on `src` = [`MARKS`] or past [`SOURCES`].
    pub fn pack(&mut self, src: usize, t: &CanvasPrims, palette: &Palette, change: Change<'_>) {
        assert!(
            src < SOURCES && src != MARKS,
            "the marks pack through `pack_marks`"
        );
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
                let s = s | chain_flags(j, closed);
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
                        let s = style | chain_flags(j, closed);
                        put(EDGE, range(i), &[f[0], f[1], f[2], f[3], color, s]);
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

/// The chain flags of edge `j` of a chain, in place in a style word.
fn chain_flags(j: usize, closed: bool) -> u32 {
    let first = if j == 0 { FIRST } else { 0 };
    let closed = if closed { CLOSED } else { 0 };
    (first | closed) << 20
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

/// The camera uniform (`Cam` in `gpu.wgsl`) for `frame` at `dev` device px per logical px
/// into a `target`-sized texture.
fn uniform(frame: &Frame, dev: f64, (w, h): (u32, u32)) -> [u8; UNIFORM_BYTES] {
    let (cam, v) = (&frame.cam, frame.view);
    assert!(dev > 0.0 && cam.scale > 0.0, "a drawable camera");
    let (fx, fy) = (cam.cx.floor(), cam.cy.floor());
    let clamp = |c: f64| c.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (ox, oy) = cam.origin(v);
    let grid = match crate::canvas::grid_dots(v, cam, frame.grid) {
        Some((s, ..)) => {
            let p = s * cam.scale;
            [
                p as f32,
                ox.rem_euclid(p) as f32,
                oy.rem_euclid(p) as f32,
                0.0,
            ]
        }
        None => [0.0; 4],
    };
    let color = |rgba: u32| rgba.to_be_bytes().map(|b| f32::from(b) / 255.0);
    let (gc, bg) = (color(frame.grid_color), color(frame.background));
    let words: [u32; UNIFORM_BYTES / 4] = [
        clamp(fx) as u32,
        clamp(fy) as u32,
        ((cam.cx - fx) as f32).to_bits(),
        ((cam.cy - fy) as f32).to_bits(),
        ((v.x0 * dev) as f32).to_bits(),
        ((v.y0 * dev) as f32).to_bits(),
        ((v.x1 * dev) as f32).to_bits(),
        ((v.y1 * dev) as f32).to_bits(),
        ((cam.scale * dev) as f32).to_bits(),
        (dev as f32).to_bits(),
        (w as f32).to_bits(),
        (h as f32).to_bits(),
        grid[0].to_bits(),
        grid[1].to_bits(),
        grid[2].to_bits(),
        grid[3].to_bits(),
        gc[0].to_bits(),
        gc[1].to_bits(),
        gc[2].to_bits(),
        gc[3].to_bits(),
        bg[0].to_bits(),
        bg[1].to_bits(),
        bg[2].to_bits(),
        bg[3].to_bits(),
    ];
    let mut out = [0u8; UNIFORM_BYTES];
    for (i, wd) in words.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&wd.to_ne_bytes());
    }
    out
}

/// The frame's pipelines and their GPU state: the canvas pipelines with one vertex buffer per
/// `(source, kind)` of rows, grown by doubling, and the camera uniform; the batch pipeline
/// (`ui.wgsl`) with the frame's instance buffer, the coverage atlas's texture and the images'.
/// Owner: the window (or a benchmark driving the GPU itself).
pub struct Pipelines {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    background: wgpu::RenderPipeline,
    kinds: [wgpu::RenderPipeline; KINDS],
    buffers: [[Option<wgpu::Buffer>; KINDS]; SOURCES],
    uploaded: u64,
    ui: wgpu::RenderPipeline,
    ui_layout: wgpu::BindGroupLayout,
    ui_uniform: wgpu::Buffer,
    atlas: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// The atlas with a 1 × 1 stand-in image: every batch draw but an image's.
    ui_bind: wgpu::BindGroup,
    /// Image textures' bind groups by blob id (`Batch::images`).
    images: HashMap<u64, wgpu::BindGroup>,
    instances: Option<wgpu::Buffer>,
    atlas_uploaded: u64,
    instances_uploaded: u64,
}

impl std::fmt::Debug for Pipelines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipelines")
            .field("uploaded", &self.uploaded)
            .field("atlas_uploaded", &self.atlas_uploaded)
            .finish()
    }
}

const SEG_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Uint32, 3 => Uint32];
const ARC_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Sint32x2, 1 => Sint32, 2 => Sint32, 3 => Sint32, 4 => Uint32, 5 => Uint32
];
const TRI_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Sint32x2, 1 => Sint32x2, 2 => Sint32x2, 3 => Uint32];
const UI_ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32, 4 => Uint32
];
/// The batch uniform (`Ui` in `ui.wgsl`): the target's size.
const UI_UNIFORM_BYTES: usize = 16;

/// A render pipeline of `module`'s `vs`/`fs` into `format`, its rows `stride`-byte instances
/// with `attributes` (none: a full-quad pipeline), blended by `blend`.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    (vs, fs): (&str, &str),
    rows: Option<(usize, &[wgpu::VertexAttribute])>,
    format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    let buffers: Vec<Option<wgpu::VertexBufferLayout>> = rows
        .map(|(stride, attributes)| {
            Some(wgpu::VertexBufferLayout {
                array_stride: stride as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes,
            })
        })
        .into_iter()
        .collect();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(vs),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// A buffer of `size` bytes for `usage` (plus copies into it).
fn buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A `w × h` 2D texture of `format`, sampled and written by copies.
fn texture(
    device: &wgpu::Device,
    label: &str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Writes `data` (`w × h` texels of `bpp` bytes, row-major) into `t` at `(x, y)`.
fn write_texels(
    queue: &wgpu::Queue,
    t: &wgpu::Texture,
    (x, y, w, h): (u32, u32, u32, u32),
    bpp: u32,
    data: &[u8],
) {
    assert_eq!(data.len(), (w * h * bpp) as usize, "whole texels");
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: t,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bpp),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

impl Pipelines {
    /// Builds the pipelines for a `format` render target (the surface's, or an offscreen
    /// `Rgba8Unorm`). The canvas blends straight alpha, the batch premultiplied.
    ///
    /// # Panics
    ///
    /// Panics on a device whose textures cannot be [`ATLAS_SIDE`] texels across.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipelines {
        let side = u32::from(ATLAS_SIDE);
        assert!(
            device.limits().max_texture_dimension_2d >= side,
            "an atlas-sized texture"
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas"),
            source: wgpu::ShaderSource::Wgsl(CANVAS_WGSL.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let uniform_entry = |bytes: usize| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(bytes as u64),
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas camera"),
            entries: &[entry(0, uniform_entry(UNIFORM_BYTES))],
        });
        let uniform = buffer(
            device,
            "canvas camera",
            UNIFORM_BYTES as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas camera"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let canvas = |vs_fs, rows| {
            let blend = wgpu::BlendState::ALPHA_BLENDING;
            pipeline(
                device,
                &pipeline_layout,
                &module,
                vs_fs,
                rows,
                format,
                blend,
            )
        };
        // The batch: a uniform, the atlas, an image and a sampler.
        let tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let ui_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("batch"),
            entries: &[
                entry(0, uniform_entry(UI_UNIFORM_BYTES)),
                entry(1, tex),
                entry(2, tex),
                entry(
                    3,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let ui_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("batch"),
            source: wgpu::ShaderSource::Wgsl(UI_WGSL.into()),
        });
        let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("batch"),
            bind_group_layouts: &[Some(&ui_layout)],
            immediate_size: 0,
        });
        let ui = pipeline(
            device,
            &ui_pipeline_layout,
            &ui_module,
            ("vs_ui", "fs_ui"),
            Some((batch::STRIDE, &UI_ATTRS)),
            format,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        let ui_uniform = buffer(
            device,
            "batch",
            UI_UNIFORM_BYTES as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let atlas = texture(device, "atlas", (side, side), wgpu::TextureFormat::R8Unorm);
        let atlas_view = atlas.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("batch"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = texture(device, "no image", (1, 1), wgpu::TextureFormat::Rgba8Unorm);
        let ui_bind = bind_batch(
            device,
            &ui_layout,
            &ui_uniform,
            &atlas_view,
            &blank.create_view(&Default::default()),
            &sampler,
        );
        Pipelines {
            background: canvas(("vs_bg", "fs_bg"), None),
            kinds: [
                canvas(("vs_tri", "fs_tri"), Some((STRIDE[TRI], &TRI_ATTRS))),
                canvas(("vs_rect", "fs_rect"), Some((STRIDE[RECT], &SEG_ATTRS))),
                canvas(("vs_arc", "fs_arc"), Some((STRIDE[ARC], &ARC_ATTRS))),
                canvas(("vs_seg", "fs_seg"), Some((STRIDE[LINE], &SEG_ATTRS))),
                canvas(("vs_seg", "fs_seg"), Some((STRIDE[EDGE], &SEG_ATTRS))),
            ],
            uniform,
            bind,
            buffers: Default::default(),
            uploaded: 0,
            ui,
            ui_bind,
            ui_layout,
            ui_uniform,
            atlas,
            atlas_view,
            sampler,
            images: HashMap::new(),
            instances: None,
            atlas_uploaded: 0,
            instances_uploaded: 0,
        }
    }

    /// Writes the rows' dirty ranges to their buffers (a buffer too small is replaced by one
    /// twice the size and written whole) and clears them. Call before [`Pipelines::draw`].
    ///
    /// Cost: one `write_buffer` per dirty range.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rows: &mut InstanceRows) {
        for (src, tables) in rows.tables.iter_mut().enumerate() {
            for (kind, t) in tables.iter_mut().enumerate() {
                let dirty = std::mem::take(&mut t.dirty);
                if dirty.is_empty() {
                    continue;
                }
                let bytes = &t.bytes;
                assert!(bytes.len() <= BYTES_MAX, "rows fit wgpu's buffer limit");
                let slot = &mut self.buffers[src][kind];
                let fits = slot
                    .as_ref()
                    .is_some_and(|b| b.size() >= bytes.len() as u64);
                if !fits {
                    let size = bytes.len().next_power_of_two().max(4096);
                    let usage = wgpu::BufferUsages::VERTEX;
                    let b = buffer(device, "canvas rows", size as u64, usage);
                    if !bytes.is_empty() {
                        queue.write_buffer(&b, 0, bytes);
                        self.uploaded += bytes.len() as u64;
                    }
                    *slot = Some(b);
                    continue;
                }
                let b = slot.as_ref().expect("fits");
                for r in dirty {
                    let r = r.start.min(bytes.len())..r.end.min(bytes.len());
                    if !r.is_empty() {
                        queue.write_buffer(b, r.start as u64, &bytes[r.clone()]);
                        self.uploaded += r.len() as u64;
                    }
                }
            }
        }
        assert_eq!(rows.dirty_bytes(), 0, "every dirty range uploaded");
    }

    /// Writes the batch's instances, its atlas's pending masks and the textures of images it
    /// draws for the first time, and clears the pending masks. Call before
    /// [`Pipelines::draw`].
    ///
    /// Cost: one `write_buffer`, one `write_texture` per new mask and per new image.
    pub fn upload_batch(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, batch: &mut Batch) {
        let bytes = batch.instances();
        if !bytes.is_empty() {
            let fits = self
                .instances
                .as_ref()
                .is_some_and(|b| b.size() >= bytes.len() as u64);
            if !fits {
                let size = bytes.len().next_power_of_two().max(1 << 16);
                let usage = wgpu::BufferUsages::VERTEX;
                self.instances = Some(buffer(device, "batch", size as u64, usage));
            }
            queue.write_buffer(self.instances.as_ref().expect("made"), 0, bytes);
            self.instances_uploaded += bytes.len() as u64;
        }
        let (slots, coverage) = batch.atlas.pending();
        for &(s, at) in slots {
            let n = usize::from(s.w) * usize::from(s.h);
            let at = at as usize;
            let rect = (
                u32::from(s.x),
                u32::from(s.y),
                u32::from(s.w),
                u32::from(s.h),
            );
            write_texels(queue, &self.atlas, rect, 1, &coverage[at..at + n]);
        }
        self.atlas_uploaded += batch.atlas.pending_bytes() as u64;
        batch.atlas.clear_pending();
        // ponytail: image textures are dropped wholesale past twice the prose image cap (as
        // `painter::image_data` drops its blobs); an LRU when pages show more images.
        if self.images.len() + batch.images().len() > 2 * schemify_ui::TextSystem::IMAGES_MAX {
            self.images.clear();
        }
        for image in batch.images() {
            let id = image.data.id();
            if self.images.contains_key(&id) {
                continue;
            }
            assert!(
                image.format == peniko::ImageFormat::Rgba8,
                "prose images are RGBA8"
            );
            let size = (image.width, image.height);
            let t = texture(device, "image", size, wgpu::TextureFormat::Rgba8Unorm);
            write_texels(queue, &t, (0, 0, size.0, size.1), 4, image.data.data());
            let view = t.create_view(&Default::default());
            let bind = bind_batch(
                device,
                &self.ui_layout,
                &self.ui_uniform,
                &self.atlas_view,
                &view,
                &self.sampler,
            );
            self.images.insert(id, bind);
        }
        debug_assert!(self.atlas.width() == u32::from(ATLAS_SIDE));
    }

    /// Bytes written to the GPU by [`Pipelines::upload`] since the last call (a report's
    /// "bytes per edit").
    pub fn take_uploaded(&mut self) -> u64 {
        std::mem::take(&mut self.uploaded)
    }

    /// Atlas bytes and batch instance bytes written by [`Pipelines::upload_batch`] since the
    /// last call.
    pub fn take_batch_uploaded(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.atlas_uploaded),
            std::mem::take(&mut self.instances_uploaded),
        )
    }

    /// Draws one frame into `target` (a `size`-px texture of these pipelines' format) as one
    /// render pass cleared to `clear`: the batch's draws in order, each canvas stage among
    /// them (when `frame` has a canvas) scissored to its view at `dev` device px per logical
    /// px, the rows as the CPU painter draws them: source by source, layer by layer, fills
    /// before strokes. A batch with no canvas stage draws the whole canvas first. Call after
    /// [`Pipelines::upload`] and [`Pipelines::upload_batch`].
    ///
    /// Cost: one pass, two uniform writes, one draw per batch draw and per non-empty (source,
    /// layer, kind).
    ///
    /// # Panics
    ///
    /// Panics when rows were packed since the last [`Pipelines::upload`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        clear: wgpu::Color,
        rows: &InstanceRows,
        batch: &Batch,
        frame: Option<&Frame>,
        dev: f64,
        size: (u32, u32),
    ) {
        let (w, h) = size;
        assert!(w > 0 && h > 0, "a target has a size");
        assert_eq!(rows.dirty_bytes(), 0, "rows uploaded before they are drawn");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let mut ui = [0u8; UI_UNIFORM_BYTES];
        ui[..4].copy_from_slice(&(w as f32).to_ne_bytes());
        ui[4..8].copy_from_slice(&(h as f32).to_ne_bytes());
        queue.write_buffer(&self.ui_uniform, 0, &ui);
        // The canvas's scissor, when there is a canvas in view.
        let scissor = frame.and_then(|frame| {
            let v = frame.view;
            let at = |c: f64, max: u32| c.clamp(0.0, f64::from(max)) as u32;
            let (x0, y0) = (at((v.x0 * dev).floor(), w), at((v.y0 * dev).floor(), h));
            let (x1, y1) = (at((v.x1 * dev).ceil(), w), at((v.y1 * dev).ceil(), h));
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            queue.write_buffer(&self.uniform, 0, &uniform(frame, dev, size));
            Some([x0, y0, x1 - x0, y1 - y0])
        });
        let stage = |pass: &mut wgpu::RenderPass<'_>, stage: Stage| {
            let Some([x, y, sw, sh]) = scissor else {
                return;
            };
            pass.set_scissor_rect(x, y, sw, sh);
            pass.set_bind_group(0, &self.bind, &[]);
            match stage {
                Stage::Background => {
                    pass.set_pipeline(&self.background);
                    pass.draw(0..6, 0..1);
                }
                Stage::Rows(src) => self.draw_rows(pass, rows, src),
            }
        };
        let staged = batch
            .draws()
            .iter()
            .any(|d| matches!(d.what, What::Canvas(_)));
        if !staged {
            stage(&mut pass, Stage::Background);
            for src in 0..SOURCES {
                stage(&mut pass, Stage::Rows(src));
            }
        }
        for d in batch.draws() {
            let bind = match d.what {
                What::Canvas(s) => {
                    stage(&mut pass, s);
                    continue;
                }
                What::Ui => &self.ui_bind,
                What::Image(k) => {
                    let id = batch.images()[k as usize].data.id();
                    self.images
                        .get(&id)
                        .expect("images uploaded before they are drawn")
                }
            };
            if d.range.is_empty() {
                continue;
            }
            let buf = self
                .instances
                .as_ref()
                .expect("instances uploaded before they are drawn");
            assert!(
                buf.size() >= u64::from(d.range.end) * batch::STRIDE as u64,
                "the buffer holds every instance"
            );
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.ui);
            pass.set_bind_group(0, bind, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..6, d.range.clone());
        }
    }

    /// The rows of source `src`, layer by layer, each layer's kinds in order.
    fn draw_rows(&self, pass: &mut wgpu::RenderPass<'_>, rows: &InstanceRows, src: usize) {
        let tables = &rows.tables[src];
        for layer in 0..LAYERS {
            for (kind, t) in tables.iter().enumerate() {
                let used = t.used[layer];
                if used == 0 {
                    continue;
                }
                let buf = self.buffers[src][kind]
                    .as_ref()
                    .expect("rows uploaded before they are drawn");
                assert!(
                    buf.size() >= t.bytes.len() as u64,
                    "the buffer holds every row"
                );
                pass.set_pipeline(&self.kinds[kind]);
                pass.set_vertex_buffer(0, buf.slice(..));
                let verts = if kind == TRI { 3 } else { 6 };
                let first = t.start[layer];
                pass.draw(0..verts, first..first + used);
            }
        }
    }
}

/// The batch's bind group: its uniform, the atlas, `image`, the sampler.
fn bind_batch(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    atlas: &wgpu::TextureView,
    image: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("batch"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(atlas),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(image),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
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

    /// User locations an entry point's arguments (`args`) or result take, struct members
    /// counted one by one.
    fn locations(module: &naga::Module, ep: &naga::EntryPoint, args: bool) -> usize {
        let count = |ty: naga::Handle<naga::Type>, binding: &Option<naga::Binding>| match (
            &module.types[ty].inner,
            binding,
        ) {
            (naga::TypeInner::Struct { members, .. }, None) => members
                .iter()
                .filter(|m| matches!(m.binding, Some(naga::Binding::Location { .. })))
                .count(),
            (_, Some(naga::Binding::Location { .. })) => 1,
            _ => 0,
        };
        if args {
            ep.function
                .arguments
                .iter()
                .map(|a| count(a.ty, &a.binding))
                .sum()
        } else {
            ep.function
                .result
                .as_ref()
                .map_or(0, |r| count(r.ty, &r.binding))
        }
    }

    /// The WebGL2 tier (web spec §7) runs these shaders through wgpu's GL backend, which
    /// translates them with naga to GLSL ES 3.00. Oracle: naga's own validator with no
    /// optional capability and its GLSL writer set up as wgpu-hal sets it for WebGL2 (no draw
    /// parameters, so `first_instance` goes through a uniform), plus WebGL2's guaranteed limits
    /// (`Limits::downlevel_webgl2_defaults`: 16 vertex attributes, 15 inter-stage variables).
    /// Negative space: a compute entry point and a storage buffer, which WebGL2 lacks, are
    /// rejected by the same checks.
    #[test]
    fn shaders_compile_to_glsl_es_300_for_webgl2() {
        use naga::back::glsl;
        let limits = wgpu::Limits::downlevel_webgl2_defaults();
        let glsl_of = |src: &str| -> Result<Vec<String>, String> {
            let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .map_err(|e| format!("{e:?}"))?;
            let mut binding_map = glsl::BindingMap::default();
            for (_, g) in module.global_variables.iter() {
                if let Some(b) = g.binding {
                    let slot = u8::try_from(binding_map.len()).expect("few bindings");
                    binding_map.insert(b, slot);
                }
            }
            let options = glsl::Options {
                version: glsl::Version::Embedded {
                    version: 300,
                    is_webgl: true,
                },
                writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE
                    | glsl::WriterFlags::FORCE_POINT_SIZE,
                binding_map,
                zero_initialize_workgroup_memory: true,
            };
            let mut out = Vec::new();
            for ep in &module.entry_points {
                if ep.stage == naga::ShaderStage::Vertex {
                    let (ins, outs) = (locations(&module, ep, true), locations(&module, ep, false));
                    if ins > limits.max_vertex_attributes as usize {
                        return Err(format!("{}: {ins} vertex attributes", ep.name));
                    }
                    if outs > limits.max_inter_stage_shader_variables as usize {
                        return Err(format!("{}: {outs} inter-stage variables", ep.name));
                    }
                }
                let pipeline = glsl::PipelineOptions {
                    shader_stage: ep.stage,
                    entry_point: ep.name.clone(),
                    multiview: None,
                };
                let mut text = String::new();
                glsl::Writer::new(
                    &mut text,
                    &module,
                    &info,
                    &options,
                    &pipeline,
                    naga::proc::BoundsCheckPolicies::default(),
                )
                .and_then(|mut w| w.write())
                .map_err(|e| format!("{}: {e}", ep.name))?;
                out.push(text);
            }
            Ok(out)
        };
        // Every module exactly as `Pipelines::new` hands it to wgpu.
        for (name, src) in [("canvas", CANVAS_WGSL), ("ui", UI_WGSL)] {
            let out = glsl_of(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!out.is_empty(), "{name}: has entry points");
            for text in &out {
                assert!(text.starts_with("#version 300 es"), "{name}: {text:.40}");
            }
        }
        // Negative space: what WebGL2 lacks fails the same checks.
        let compute = "@compute @workgroup_size(1) fn main() {}";
        assert!(glsl_of(compute).is_err(), "no compute in GLSL ES 3.00");
        let storage = "@group(0) @binding(0) var<storage, read> s: array<u32>;
            @vertex fn main() -> @builtin(position) vec4<f32> { return vec4<f32>(f32(s[0])); }";
        assert!(
            glsl_of(storage).is_err(),
            "no storage buffers in GLSL ES 3.00"
        );
    }

    /// The uniform carries the camera split into an integer centre and a fraction, the view in
    /// device px, and the grid's pitch and phase (none when the grid is off).
    #[test]
    fn uniform_splits_the_camera() {
        let frame = Frame {
            view: Rect::new(10.0, 20.0, 650.0, 420.0),
            cam: Camera {
                cx: 12_345.75,
                cy: -6_789.5,
                scale: 0.02,
            },
            grid: 20_000,
            background: 0x0000_00ff,
            grid_color: 0x7070_70ff,
        };
        let u = words(&uniform(&frame, 2.0, (1300, 840)));
        assert_eq!((u[0] as i32, u[1] as i32), (12_345, -6_790));
        assert_eq!((f32::from_bits(u[2]), f32::from_bits(u[3])), (0.75, 0.5));
        assert_eq!(f32::from_bits(u[4]), 20.0);
        assert_eq!(f32::from_bits(u[7]), 840.0);
        assert_eq!((f32::from_bits(u[8]), f32::from_bits(u[9])), (0.04, 2.0));
        // 20 000 mu × 0.02 = 400 px per dot: no doubling; the phase is the origin mod pitch.
        let (ox, _) = frame.cam.origin(frame.view);
        assert_eq!(f32::from_bits(u[12]), 400.0);
        assert_eq!(f32::from_bits(u[13]), ox.rem_euclid(400.0) as f32);
        // "No grid" is a pitch past any view (`Editor::picture`); a view holding the world
        // origin would still show that one dot, as the CPU painter does.
        let off = Frame {
            grid: i32::MAX / 4,
            cam: Camera {
                cx: 12_345_675.75,
                ..frame.cam
            },
            ..frame
        };
        assert_eq!(
            f32::from_bits(words(&uniform(&off, 1.0, (1300, 840)))[12]),
            0.0
        );
    }
}
