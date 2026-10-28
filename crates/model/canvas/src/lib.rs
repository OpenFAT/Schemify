//! Schemify's canvas tables: the schematic as batched primitive rows in `i32` world milli-units
//! (D33), the pan/zoom [`Camera`], the per-layer [`Palette`], recolor [`Marks`], the spatial
//! index ([`PrimIndex`]) and the row diff ([`CanvasDiff`]) that let a cached canvas follow an
//! edit in O(change) (D39).
//!
//! No GPU, window or text dependency: the editor fills and edits these tables headless, and
//! `schemify-render` draws them. Their only owner is the producer that fills them (the
//! editor's scene); everyone else reads.
//!
//! # Data design
//!
//! 1. **In → out.** [`CanvasPrims`] (filled by the editor's scene) + [`Camera`] + [`Palette`]
//!    → one stroke path and one fill path per layer → painter calls.
//! 2. **How many.** 10^2–10^5 visible primitives per frame (D39 budgets 120 fps at 10^5).
//! 3. **How wide.** Coordinates `i32` milli-units (±2.1 M covers the corpus, D33); layer `u8`
//!    (xschem has 22); arc angles `i32` millidegrees; text size `i32` milli-units; screen math
//!    in `f64` (the camera) and `f32` only at the GPU boundary (the instance rows).
//! 4. **Access.** Every frame reads all coordinates of one primitive kind in one linear pass;
//!    layer decides the batch. So each kind is SoA (`x0`, `y0`, … columns) and batching is by
//!    layer into reused paths: one draw call per (layer, stroke|fill), not per primitive.
//! 5. **Lifetime.** Prims: document lifetime (rebuilt by their owner on edit, killed in place
//!    on removal, [`LAYER_DEAD`]). The index and the diff snapshot live with the cached canvas
//!    until the tables change in a way they cannot follow.
//! 6. **Parallel.** The per-kind tables are independent; the draw passes over them run only
//!    when the raster layer is re-recorded, so they stay serial.
//!
//! Contents (re-exported from the modules):
//! - `pub const LAYERS: usize`, `pub const LAYER_DEAD: u8`
//! - `pub fn chain(end: &[u32], i: usize) -> Range<usize>`
//! - `camera`: `Camera`
//! - `prims`: `CanvasPrims`, `Lines`, `Rects`, `Arcs`, `Polys`, `Texts`, `LineStyle`
//! - `palette`: `Palette`, `Marks`, `PrimKind`
//! - `damage`: `CanvasDiff`, `TableChange`, `DIFF_EDITS_MAX`
//! - `index`: `PrimIndex`, `Rows`

#![forbid(unsafe_code)]

pub use kurbo;

mod camera;
mod damage;
mod index;
mod palette;
mod prims;
mod row;

pub use camera::Camera;
pub use damage::{CanvasDiff, DIFF_EDITS_MAX, TableChange};
pub use index::{PrimIndex, Rows};
pub use palette::{Marks, Palette, PrimKind};
pub use prims::{Arcs, CanvasPrims, LineStyle, Lines, Polys, Rects, Texts};

/// Palette layers: every layer index is below it, and [`Palette::fill`] keeps one bit per layer
/// in a `u32`.
pub const LAYERS: usize = 32;

/// The layer of a dead row ([`CanvasPrims::kill`]): not a palette layer, so nothing draws,
/// lists or exports it. A producer that edits its table in place (the editor's scene) kills a
/// removed row instead of shifting or swapping rows, so every other row keeps its index and
/// [`CanvasDiff`] and [`PrimIndex`] follow the edit in O(change).
pub const LAYER_DEAD: u8 = u8::MAX;

const _: () = assert!(LAYER_DEAD as usize >= LAYERS, "dead is no palette layer");
const _: () = assert!(LAYERS == u32::BITS as usize, "one fill bit per layer");

/// The points of chain `i` of a CSR table whose `end[i]` is one past its last point
/// ([`Polys::end`], [`Texts::end`]).
///
/// # Panics
///
/// If `i` is past `end`, or the offsets are not ascending.
#[inline]
pub fn chain(end: &[u32], i: usize) -> std::ops::Range<usize> {
    let start = if i == 0 { 0 } else { end[i - 1] as usize };
    let stop = end[i] as usize;
    debug_assert!(start <= stop, "CSR offsets ascend");
    start..stop
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Xorshift64: the deterministic generator the module tests share.
    pub(crate) fn rng(mut s: u64) -> impl FnMut(u64) -> u64 {
        assert_ne!(s, 0, "xorshift seed");
        move |m| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s % m
        }
    }

    #[test]
    fn chain_reads_csr_offsets() {
        let end = [2, 2, 5];
        assert_eq!(chain(&end, 0), 0..2);
        assert_eq!(chain(&end, 1), 2..2, "an empty chain");
        assert_eq!(chain(&end, 2), 2..5);
    }
}
