//! The GPU canvas (D39; `docs/dev/research/render-backend.md` § 5, step 1): the canvas tables
//! in, retained instance rows ([`InstanceRows`]) and one render pass ([`Pipelines`]) out. The
//! rows are patched where the tables' reported changes say and drawn by instanced pipelines
//! under one camera uniform (`shaders/canvas/`). A pan or zoom writes the uniform only; an
//! edit rewrites the rows it changed. Every frame is complete: there is no tile cache.
//!
//! One producer, two consumers: the pack (`rows`) is the one place a per-row decision is
//! made (dead and hidden rows draw nothing, colour and stroke width from the palette, fills,
//! the marks' recolouring, the drawing order). The pipelines consume the rows on the GPU;
//! [`crate::canvas::draw_rows`] consumes the same rows on the CPU ([`crate::CpuPainter`], the
//! goldens). Culling to the view and the far-zoom cut (a row at most 2 px across is the whole
//! pixels it touches) are pure functions of (row, camera), evaluated by each consumer: the
//! vertex shader per row, `draw_rows` per row and per chain. Texts are not rows: they are the
//! frame batch's atlas glyphs ([`crate::batch`]), which [`Pipelines`] draws in the same pass,
//! each canvas stage at its place among the batch's instances.
//!
//! Six questions:
//! 1. In: the four canvas tables of a frame ([`schemify_canvas::CanvasPrims`]: the scene as shown, decor,
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
//! 6. Sources and kinds are independent; serial (0.7 ms per 10^5 rows packed whole,
//!    `pack_100k_whole`; an edit's patch O(change)).
//!
//! Contents:
//! - `pub const KINDS`, `TRI`, `RECT`, `ARC`, `LINE`, `EDGE`, `STRIDE`, `SOURCES`, `MARKS`,
//!   `OVERLAY`; style flags `FILL`, `PARITY`, `FIRST`, `CLOSED`
//! - `pub struct Frame`, `pub enum Change`
//! - `pub use rows::InstanceRows`, `pub use pipelines::Pipelines`
//! - `pub(crate) fn push_dirty(d: &mut Vec<Range<usize>>, r: Range<usize>)`

mod ear;
mod marks;
mod pass;
mod pens;
mod pipelines;
mod rows;

use std::ops::Range;

use kurbo::Rect;

use schemify_canvas::Camera;

pub use pipelines::Pipelines;
pub use rows::InstanceRows;

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
/// The gesture overlay's source (`Painter::canvas_rows` with [`Change::All`]), packed whole
/// each frame it shows.
pub const OVERLAY: usize = 5;

/// Style flags, bits 20–23 of a row's style word (`shaders/canvas/style.wgsl`): a closed
/// shape also filled. Every stroke has round caps and joins, as kurbo's `Stroke::new` and
/// `canvas::stroke_of` draw them on the CPU.
pub const FILL: u32 = 1;
/// Snap to a pixel centre or edge by the rounded width's parity (`canvas::styled`); else to
/// a centre.
pub const PARITY: u32 = 2;
/// On an edge row: the first edge of its chain (a polyline or polygon), so the CPU painter
/// strokes the chain as the one path it is (joins, a continuous dash). The shader ignores it.
pub const FIRST: u32 = 4;
/// On an edge row: its chain is a polygon (closed), so its last edge is the closing one.
pub const CLOSED: u32 = 8;

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

/// How a source table changed since the [`InstanceRows`] last packed it; `'diff` borrows the
/// diff that reports it (`schemify_canvas::CanvasDiff`).
#[derive(Clone, Copy, Debug)]
pub enum Change<'diff> {
    /// Not at all.
    Same,
    /// Beyond what a patch follows (a rebuilt table, a palette change): repack it whole.
    All,
    /// Rows killed (`killed[kind]` in [`schemify_canvas::CanvasPrims::kill`]'s kind order, now on
    /// [`schemify_canvas::LAYER_DEAD`]) and rows appended past the previous length; every other row as it was
    /// (`TableChange::Rows`).
    Killed(&'diff [Vec<u32>; 6]),
    /// Rows removed (old indices) and added (new indices) per kind, as
    /// `CanvasDiff::edits_of` reports them.
    // ponytail: repacked whole (rows after an edit may have moved, so the slot map would need
    // the script replayed); only the decor and the parity extra report edits, and both are
    // small next to the scene. Replay the script into the slot map if a large table does.
    Edits([(&'diff [u32], &'diff [u32]); 6]),
}

/// Dirty ranges kept per row array before they merge into one.
const DIRTY_MAX: usize = 8;

/// Notes `r` as written in `d`; past [`DIRTY_MAX`] ranges they merge into their span. An
/// empty range is not noted.
pub(crate) fn push_dirty(d: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.is_empty() {
        return;
    }
    assert!(d.len() <= DIRTY_MAX, "dirty ranges bounded");
    let (start, end) = (r.start, r.end);
    d.push(r);
    if d.len() > DIRTY_MAX {
        let lo = d.iter().map(|r| r.start).min().unwrap_or(start);
        let hi = d.iter().map(|r| r.end).max().unwrap_or(end);
        d.clear();
        d.push(lo..hi);
    }
    assert!(d.len() <= DIRTY_MAX, "dirty ranges bounded");
    debug_assert!(
        d.iter().any(|q| q.start <= start && end <= q.end),
        "the new range is noted"
    );
}
