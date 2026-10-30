//! The UI's retained instance rows (`docs/dev/research/render-backend.md` § Retained UI rows):
//! each frame's drawn layout rows (a key, and the instances of a changed row) in, per UI source
//! the instance bytes and the ranges written since the GPU last read them out. Per source (the
//! main layer, the overlay) the batch instances of every layout row the GPU painter drew, in
//! layout order, each row a slot range with free slots after it, kept across frames. A row
//! whose [`RowKey`] (its paint inputs) and place are unchanged keeps its slots and costs
//! nothing; a changed row is repainted into its own slots, uploading only the instances that
//! differ; a row that outgrows its slots, or a row inserted or dropped, lays the rest of its
//! source out again. Rows inside a `scroll` pane are kept in the pane's content coordinates and
//! moved by the pane's offset in the batch uniform ([`PANES`]), so a scroll writes no row.
//!
//! Six questions:
//! 1. In: per frame each drawn row's key and, for a changed row, its painted instances. Out:
//!    per source the instance bytes, the byte ranges written since the GPU last read them, the
//!    slot range of each row (the frame's draws), the panes' offsets and clips.
//! 2. One row per laid-out UI row (2^16 nodes at most), a few to a few hundred instances each;
//!    [`INSTANCES_MAX`] per source.
//! 3. Slots `u32` (under 2^18); instances [`crate::batch::STRIDE`] bytes; pane slots `u8`.
//! 4. Each frame walks the rows in layout order once, comparing keys; bytes are written only
//!    for changed rows.
//! 5. Rows live while their node is drawn: a row not drawn in a frame is dropped at its
//!    source's end ([`crate::Painter::ui_end`]). Pane slots live while a row of theirs is drawn.
//! 6. Serial (layout order is drawing order).
//!
//! Contents:
//! - `pub const SOURCES`, `pub const PANES`, `pub const INSTANCES_MAX`
//! - `pub struct UiRows`, `UiRows::dirty_bytes`, `UiRows::clear_dirty`, `UiRows::bytes`,
//!   `UiRows::rows`, `UiRows::slots_of`, `UiRows::dirty`, `UiRows::panes`
//! - `pub(crate) struct Source`
//! - `pub(crate) fn find(keys: &[RowKey], cursor: usize, key: &RowKey) -> Option<usize>`
//! - `pub(crate)` frame calls: `UiRows::begin_frame`, `UiRows::rescale`, `UiRows::pane`,
//!   `UiRows::keep`, `UiRows::put`, `UiRows::end`

use std::mem;
use std::ops::Range;

use schemify_ui::Sym;

use crate::atlas::Atlas;
use crate::batch::{self, STRIDE};
use crate::gpu;
use crate::widgets::RowKey;

/// UI sources: the main layer and the overlay (menus, dialogs).
pub const SOURCES: usize = 2;
/// Scroll panes whose offsets the batch uniform holds; slot 0 is "no pane" (no offset, no
/// clip). A row in a pane past these is kept in screen coordinates (repainted on a scroll).
pub const PANES: usize = 32;
/// Instances one source holds; a row past them is not drawn (counted in `Batch::dropped`).
pub const INSTANCES_MAX: usize = 1 << 18;

/// The UI's retained instance rows and the scroll panes they are drawn through. Owner: the
/// batch ([`crate::Batch::ui`]); written only by the GPU painter's [`crate::Painter::ui_row`]
/// and [`crate::Painter::ui_end`], read by the upload and the pass.
#[derive(Debug)]
pub struct UiRows {
    pub(crate) src: [Source; SOURCES],
    /// Per pane slot: the scroll node it is, the frame a row of it was last drawn in, and its
    /// uniform entry (offset in device px, then the pane's clip, device px).
    pane_id: [Sym; PANES],
    pane_seen: [u32; PANES],
    pub(crate) panes: [[f32; 8]; PANES],
    frame: u32,
    /// Device px per logical px the rows were painted at.
    scale: f64,
}

/// One source's rows: the instance bytes and their dirty ranges (read by the upload), last
/// frame's rows and this frame's so far, and the layout cursor.
#[derive(Debug, Default)]
pub(crate) struct Source {
    pub(crate) bytes: Vec<u8>,
    pub(crate) dirty: Vec<Range<usize>>,
    old: Segs,
    new: Segs,
    /// Next of last frame's rows to match, the row matched for the row being drawn, and the
    /// next free slot of this frame's layout.
    cursor: usize,
    matched: Option<usize>,
    at: u32,
    /// The slot where this frame's layout first left last frame's (rows from there on are
    /// rewritten from `spare`, the bytes as they were).
    moved: Option<u32>,
    spare: Vec<u8>,
}

/// Rows ahead of the cursor a row's identity is looked for among last frame's: an insertion
/// or a removal of up to this many rows keeps the rest.
const SEARCH: usize = 64;
/// A pane slot's uniform entry with no offset and no clip.
const NO_PANE: [f32; 8] = [0.0, 0.0, 0.0, 0.0, -1.0e9, -1.0e9, 1.0e9, 1.0e9];

const _: () = assert!(
    PANES <= 256,
    "a pane slot fits the key's u8 and the kind word"
);
const _: () = assert!(INSTANCES_MAX < u32::MAX as usize, "slots are u32");

/// Rows in layout order (SoA, equal lengths): key, first slot, instances, slots reserved.
#[derive(Clone, Debug, Default)]
struct Segs {
    key: Vec<RowKey>,
    start: Vec<u32>,
    len: Vec<u32>,
    cap: Vec<u32>,
}

/// The index of the row among `keys[cursor..]` (at most [`SEARCH`] ahead) that shows the same
/// node the same way as `key`: the row `key` was last frame. Shared by both painters, so the
/// CPU painter's goldens exercise the same matching as the GPU's rows.
pub(crate) fn find(keys: &[RowKey], cursor: usize, key: &RowKey) -> Option<usize> {
    assert!(cursor <= keys.len(), "the cursor walks last frame's rows");
    let end = keys.len().min(cursor + SEARCH);
    let found = (cursor..end).find(|&o| keys[o].id == key.id && keys[o].shown == key.shown);
    if let Some(o) = found {
        debug_assert!(o - cursor < SEARCH, "found within the search window");
    }
    found
}

impl Default for UiRows {
    fn default() -> Self {
        UiRows {
            src: Default::default(),
            pane_id: [Sym::NONE; PANES],
            pane_seen: [0; PANES],
            panes: [NO_PANE; PANES],
            frame: 2,
            scale: 0.0,
        }
    }
}

impl UiRows {
    /// Bytes the GPU has to read to be current.
    #[must_use]
    pub fn dirty_bytes(&self) -> usize {
        let dirty = || self.src.iter().flat_map(|s| s.dirty.iter());
        debug_assert!(
            self.src
                .iter()
                .all(|s| s.dirty.iter().all(|r| r.end <= s.bytes.len())),
            "dirty ranges inside the rows"
        );
        dirty().map(|r| r.len()).sum()
    }

    /// Forgets the dirty ranges, as an upload does (headless tests).
    pub fn clear_dirty(&mut self) {
        for s in &mut self.src {
            s.dirty.clear();
        }
        debug_assert_eq!(self.dirty_bytes(), 0, "no range left");
    }

    /// Source `src`'s instances, [`STRIDE`] bytes each (free slots zero).
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES`.
    #[must_use]
    pub fn bytes(&self, src: usize) -> &[u8] {
        assert!(src < SOURCES, "a UI source");
        let b = &self.src[src].bytes;
        debug_assert_eq!(b.len() % STRIDE, 0, "whole instances");
        b
    }

    /// Rows source `src` holds (drawn last frame).
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES`.
    #[must_use]
    pub fn rows(&self, src: usize) -> usize {
        assert!(src < SOURCES, "a UI source");
        let old = &self.src[src].old;
        debug_assert_eq!(old.key.len(), old.cap.len(), "rows in step");
        old.key.len()
    }

    /// The slots (reserved ones included) of the first row of source `src` drawn last frame
    /// that shows node `id`.
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES`.
    #[must_use]
    pub fn slots_of(&self, src: usize, id: Sym) -> Option<Range<u32>> {
        assert!(src < SOURCES, "a UI source");
        let o = &self.src[src].old;
        let k = o.key.iter().position(|k| k.id == id)?;
        let slots = o.start[k]..o.start[k] + o.cap[k];
        debug_assert!(o.len[k] <= o.cap[k], "a row fits its slots");
        Some(slots)
    }

    /// The byte ranges of source `src` written since the GPU last read them.
    ///
    /// # Panics
    ///
    /// Panics on `src ≥ SOURCES`.
    #[must_use]
    pub fn dirty(&self, src: usize) -> &[Range<usize>] {
        assert!(src < SOURCES, "a UI source");
        &self.src[src].dirty
    }

    /// Per pane slot its uniform entry: offset (device px), then clip (device px).
    #[must_use]
    pub fn panes(&self) -> [[f32; 8]; PANES] {
        debug_assert_eq!(self.panes[0], NO_PANE, "slot 0 is no pane");
        self.panes
    }

    /// Starts a frame: panes not drawn last frame give up their slots.
    pub(crate) fn begin_frame(&mut self) {
        self.frame = self.frame.checked_add(1).expect("frame stamps do not wrap");
        for k in 1..PANES {
            if self.pane_seen[k] + 1 < self.frame {
                self.pane_id[k] = Sym::NONE;
                self.panes[k] = NO_PANE;
            }
        }
        assert_eq!(self.panes[0], NO_PANE, "slot 0 is no pane");
    }

    /// Repaints every row from the next frame on when the rows were painted at another scale.
    pub(crate) fn rescale(&mut self, scale: f64) {
        assert!(scale > 0.0, "a positive scale, got {scale}");
        if self.scale != scale {
            self.scale = scale;
            for s in &mut self.src {
                for k in &mut s.old.key {
                    k.stale = true;
                }
            }
        }
        debug_assert!(
            self.scale == scale,
            "rows are painted at this scale from now on"
        );
    }

    /// The pane slot of scroll node `id` this frame (taken on its first row), `None` when every
    /// slot is held.
    pub(crate) fn pane(&mut self, id: Sym) -> Option<usize> {
        assert!(!id.is_none(), "a pane is a scroll node");
        let k = (1..PANES)
            .find(|&k| self.pane_id[k] == id)
            .or_else(|| (1..PANES).find(|&k| self.pane_id[k].is_none()))?;
        assert!(k > 0, "slot 0 is no pane");
        self.pane_id[k] = id;
        self.pane_seen[k] = self.frame;
        Some(k)
    }

    /// Row `key` of source `src` keeps last frame's slots when its key is unchanged: the slot
    /// range to draw (rewritten at a new place when the layout moved before it). `None`: paint
    /// it and [`UiRows::put`] it.
    pub(crate) fn keep(
        &mut self,
        src: usize,
        key: &RowKey,
        atlas: &mut Atlas,
    ) -> Option<Range<u32>> {
        assert!(src < SOURCES, "a UI source");
        let s = &mut self.src[src];
        s.matched = find(&s.old.key, s.cursor, key);
        let o = s.matched?;
        s.cursor = o + 1;
        if s.old.key[o] != *key {
            return None;
        }
        let (start, len, cap) = (s.old.start[o], s.old.len[o], s.old.cap[o]);
        debug_assert!(len <= cap, "a row fits its slots");
        if s.moved.is_none() && start != s.at {
            s.begin_moved(atlas);
        }
        if s.moved.is_none() {
            return Some(s.place(*key, len, cap));
        }
        let (lo, hi) = (start as usize * STRIDE, (start + len) as usize * STRIDE);
        let old = mem::take(&mut s.spare);
        assert!(hi <= old.len(), "a kept row's bytes are in last frame's");
        s.write(s.at, &old[lo..hi], len + slack(len));
        s.spare = old;
        Some(s.place(*key, len, len + slack(len)))
    }

    /// Row `key` of source `src` painted as `inst` (after [`UiRows::keep`] returned `None` for
    /// it): written over its own slots when it is where it was and fits them, writing only
    /// the instances that differ; else from here on the source is laid out again. Returns the
    /// slot range to draw, `None` past [`INSTANCES_MAX`].
    pub(crate) fn put(
        &mut self,
        src: usize,
        key: RowKey,
        inst: &[u8],
        atlas: &mut Atlas,
    ) -> Option<Range<u32>> {
        assert_eq!(inst.len() % STRIDE, 0, "whole instances");
        assert!(src < SOURCES, "a UI source");
        let s = &mut self.src[src];
        let n = (inst.len() / STRIDE) as u32;
        let at = s.at;
        if let Some(o) = s.matched.take()
            && s.moved.is_none()
            && s.old.start[o] == at
            && n <= s.old.cap[o]
        {
            let (olen, cap) = (s.old.len[o], s.old.cap[o]);
            let (lo, hi) = (at as usize * STRIDE, (at + olen) as usize * STRIDE);
            pin(atlas, &s.bytes[lo..hi], -1);
            pin(atlas, inst, 1);
            // Only the instances that differ (a hover's colours, a typed glyph and the caret).
            let end = (at + n.max(olen)) as usize * STRIDE;
            let (mut first, mut last) = (usize::MAX, 0);
            for (k, b) in s.bytes[lo..end].chunks_exact(STRIDE).enumerate() {
                let new = inst
                    .get(k * STRIDE..(k + 1) * STRIDE)
                    .unwrap_or(&[0; STRIDE]);
                if b != new {
                    (first, last) = (first.min(k), k + 1);
                }
            }
            s.write(at, inst, cap);
            if first < last {
                gpu::push_dirty(&mut s.dirty, lo + first * STRIDE..lo + last * STRIDE);
            }
            return Some(s.place(key, n, cap));
        }
        let cap = n + slack(n);
        if (at + cap) as usize > INSTANCES_MAX {
            return None;
        }
        if s.moved.is_none() {
            s.begin_moved(atlas);
        }
        s.write(at, inst, cap);
        debug_assert!(
            s.moved.is_some(),
            "a rewritten row moves the rest of the layout"
        );
        Some(s.place(key, n, cap))
    }

    /// Ends source `src`'s frame: last frame's rows not drawn are dropped, the moved part of
    /// the layout is noted for upload, and this frame's rows become the ones kept.
    pub(crate) fn end(&mut self, src: usize, atlas: &mut Atlas) {
        assert!(src < SOURCES, "a UI source");
        let s = &mut self.src[src];
        let end = s.at as usize * STRIDE;
        match s.moved.take() {
            Some(m) => {
                let m = m as usize * STRIDE;
                s.bytes.truncate(end);
                pin(atlas, &s.bytes[m..], 1);
                gpu::push_dirty(&mut s.dirty, m..end);
            }
            None if s.bytes.len() > end => {
                pin(atlas, &s.bytes[end..], -1);
                s.bytes.truncate(end);
            }
            None => {}
        }
        for r in &mut s.dirty {
            r.end = r.end.min(end);
        }
        s.dirty.retain(|r| !r.is_empty());
        mem::swap(&mut s.old, &mut s.new);
        s.new.clear();
        (s.cursor, s.matched, s.at) = (0, None, 0);
        assert_eq!(s.bytes.len(), end, "the bytes are this frame's rows");
        debug_assert!(
            s.dirty.iter().all(|r| r.end <= end),
            "dirty ranges inside the rows"
        );
    }
}

// ---- private ----

/// Free slots kept after a row of `n` instances: room for a hover's background or a few typed
/// characters without laying the source out again.
fn slack(n: u32) -> u32 {
    debug_assert!(n as usize <= INSTANCES_MAX, "a row within a source");
    n / 4 + 4
}

/// Pins (`delta` 1) or unpins (-1) the atlas shelves the masks among `bytes` sample: a shelf a
/// retained row draws from is never evicted, so no retained row names an evicted mask.
fn pin(atlas: &mut Atlas, bytes: &[u8], delta: i32) {
    assert_eq!(bytes.len() % STRIDE, 0, "whole instances");
    assert!(delta == 1 || delta == -1, "one instance at a time");
    for b in bytes.chunks_exact(STRIDE) {
        if let Some(y) = batch::mask_row(b) {
            atlas.pin(y, delta);
        }
    }
}

impl Segs {
    fn push(&mut self, key: RowKey, start: u32, len: u32, cap: u32) {
        assert!(len <= cap, "a row fits its slots");
        let rows = self.len();
        self.key.push(key);
        self.start.push(start);
        self.len.push(len);
        self.cap.push(cap);
        debug_assert_eq!(self.len(), rows + 1, "columns in step");
    }

    fn clear(&mut self) {
        self.key.clear();
        self.start.clear();
        self.len.clear();
        self.cap.clear();
        debug_assert!(self.is_empty(), "no row left");
    }

    fn len(&self) -> usize {
        debug_assert_eq!(self.start.len(), self.key.len(), "rows in step");
        debug_assert_eq!(self.cap.len(), self.key.len(), "rows in step");
        self.key.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Source {
    /// Appends row `key` of `len` instances at the layout cursor with `cap` slots reserved;
    /// returns its slot range.
    fn place(&mut self, key: RowKey, len: u32, cap: u32) -> Range<u32> {
        let at = self.at;
        let end = at.checked_add(cap).expect("slots are u32");
        self.new.push(key, at, len, cap);
        self.at = end;
        debug_assert_eq!(self.new.key.len(), self.new.cap.len(), "rows in step");
        at..end
    }

    /// This frame's layout leaves last frame's at slot `at`: every row from there on is
    /// written again (from `spare` or freshly painted), and last frame's rows there let go of
    /// their shelves.
    fn begin_moved(&mut self, atlas: &mut Atlas) {
        assert!(self.moved.is_none(), "moved once a frame");
        let at = self.at as usize * STRIDE;
        self.moved = Some(self.at);
        self.spare.clear();
        self.spare.extend_from_slice(&self.bytes);
        pin(atlas, &self.bytes[at.min(self.bytes.len())..], -1);
        debug_assert_eq!(
            self.spare.len(),
            self.bytes.len(),
            "the spare is last frame's bytes"
        );
    }

    /// Writes `inst` at slot `at` with `cap` slots reserved (the rest zero), growing the
    /// bytes: the moved part of the layout, uploaded whole at the source's end.
    fn write(&mut self, at: u32, inst: &[u8], cap: u32) {
        let (lo, hi) = (at as usize * STRIDE, (at + cap) as usize * STRIDE);
        assert!(inst.len() <= hi - lo, "a row fits its slots");
        if self.bytes.len() < hi {
            self.bytes.resize(hi, 0);
        }
        self.bytes[lo..lo + inst.len()].copy_from_slice(inst);
        self.bytes[lo + inst.len()..hi].fill(0);
        debug_assert!(self.bytes.len() >= hi, "the slots are written");
    }
}
