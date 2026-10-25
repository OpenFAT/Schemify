//! The coverage atlas (`docs/dev/research/render-backend.md` § 5, step 3): one single-channel
//! texture of [`ATLAS_SIDE`]² (WebGL2's guaranteed texture size) holding every glyph and path
//! mask the GPU painter draws, packed in shelves and evicted least recently used, a shelf at a
//! time. Headless: the masks wait in [`Atlas::pending`] until the GPU side copies them into
//! its texture, so packing and eviction are tested without a device.
//!
//! Six questions:
//! 1. In: masks (a key, a size, the cell's offset from the glyph's whole-pixel origin, the
//!    coverage bytes), and per frame which keys it draws. Out: per key the slot it lives in;
//!    per frame the slots written and their bytes.
//! 2. ≤ 2048² bytes of coverage; a few thousand entries (a UI frame draws a few hundred
//!    distinct glyphs, a zoom a few hundred more per frame).
//! 3. Slots are `u16` texels (the side is 2048); keys three `u64`s (font blob, glyph id and
//!    face index, a hash of size, transform and phase; or a path's two hashes); frame stamps
//!    `u32` (a frame a millisecond for 49 days).
//! 4. Lookups hash a key per glyph drawn; inserts append to the shelf of their height class.
//! 5. Entries live until their shelf is evicted: the least recently drawn shelf not drawn this
//!    frame, when a mask fits nowhere else. Pending bytes live one frame.
//! 6. Serial (the painter is).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Texels per side of the atlas: WebGL2's guaranteed `MAX_TEXTURE_SIZE`.
pub const ATLAS_SIDE: u16 = 2048;
/// Shelf heights are multiples of this many texels (height classes).
const CLASS: u16 = 4;

/// An atlas entry's key: `[font blob, (face index << 32) | glyph id, hash]` for a glyph,
/// `[PATH, hash, hash]` for a path mask ([`PATH`]).
pub type Key = [u64; 3];
/// First word of a path mask's key (a font blob id is a counter, never this).
pub const PATH: u64 = u64::MAX;

/// Multiply-rotate hashing (fxhash's mix) for the map's keys, which are hashes and ids
/// already: SipHash per glyph drawn cost more than the rest of its lookup.
#[derive(Clone, Copy, Debug, Default)]
struct Mix(u64);

impl Hasher for Mix {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(w));
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// A rectangle of the atlas, texels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot {
    /// Left and top texel.
    pub x: u16,
    /// Top texel.
    pub y: u16,
    /// Width and height (0 × 0: a mask with no coverage, drawn as nothing).
    pub w: u16,
    /// Height.
    pub h: u16,
}

impl Slot {
    /// Whether the two slots share a texel.
    pub fn overlaps(&self, o: &Slot) -> bool {
        let (a, b) = (
            [self.x, self.y, self.x + self.w, self.y + self.h],
            [o.x, o.y, o.x + o.w, o.y + o.h],
        );
        a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
    }
}

/// A key's slot and where its cell sits relative to what it was keyed at (a glyph: its
/// whole-pixel origin; a path: its cell's corner, so 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The texels.
    pub slot: Slot,
    /// The cell's top-left, device px from the key's anchor.
    pub off: [i16; 2],
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    placed: Placed,
    /// Its shelf (`u16::MAX` for an empty mask, which takes no texels).
    shelf: u16,
}

#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u16,
    h: u16,
    /// Next free texel along the shelf.
    x: u16,
    /// The last frame a mask of this shelf was drawn in.
    used: u32,
}

/// The atlas: shelves of height classes, the entries placed in them, and the bytes written
/// since the GPU last copied them. Owner: the GPU painter's batch (`batch::Batch`).
#[derive(Debug)]
pub struct Atlas {
    side: u16,
    frame: u32,
    shelves: Vec<Shelf>,
    /// Top of the space no shelf has taken.
    next_y: u16,
    map: HashMap<Key, Entry, BuildHasherDefault<Mix>>,
    /// Slots written since [`Atlas::clear_pending`], each with its bytes' offset in `staging`
    /// (`w × h`, row-major).
    pending: Vec<(Slot, u32)>,
    staging: Vec<u8>,
    /// Masks that fit nowhere (every shelf tall enough is drawn this frame): not drawn.
    pub overflows: u64,
    /// Shelves evicted so far.
    pub evictions: u64,
}

impl Default for Atlas {
    fn default() -> Self {
        Atlas::new(ATLAS_SIDE)
    }
}

impl Atlas {
    /// An empty atlas of `side`² texels (tests use small ones to exercise eviction).
    ///
    /// # Panics
    ///
    /// Panics unless `CLASS ≤ side ≤ ATLAS_SIDE`.
    pub fn new(side: u16) -> Atlas {
        assert!(
            (CLASS..=ATLAS_SIDE).contains(&side),
            "an atlas side in range"
        );
        Atlas {
            side,
            frame: 1,
            shelves: Vec::new(),
            next_y: 0,
            map: HashMap::default(),
            pending: Vec::new(),
            staging: Vec::new(),
            overflows: 0,
            evictions: 0,
        }
    }

    /// Texels per side.
    pub fn side(&self) -> u16 {
        self.side
    }

    /// Starts a frame: entries drawn from now on are this frame's, which eviction keeps.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        assert!(self.frame > 0, "frame stamps do not wrap");
    }

    /// The placed mask of `key`, marked drawn this frame; `None` when it is not held.
    pub fn get(&mut self, key: &Key) -> Option<Placed> {
        let e = self.map.get(key)?;
        if e.shelf != u16::MAX {
            self.shelves[usize::from(e.shelf)].used = self.frame;
        }
        Some(e.placed)
    }

    /// Stores the `w × h` mask `coverage` (row-major) under `key`, drawn this frame, its cell
    /// at `off` from the key's anchor, evicting the least recently drawn shelf not drawn
    /// this frame when nothing else fits. `None` when it fits nowhere (counted in
    /// [`Atlas::overflows`]; no texel of a mask drawn this frame is overwritten). An empty
    /// mask (`w` or `h` 0) takes no texels.
    ///
    /// Cost: O(shelves) to place; an eviction O(entries).
    ///
    /// # Panics
    ///
    /// Panics when `key` is held, the mask is wider or taller than the atlas, or `coverage`
    /// is not `w × h` bytes.
    pub fn insert(
        &mut self,
        key: Key,
        w: u16,
        h: u16,
        off: [i16; 2],
        coverage: &[u8],
    ) -> Option<Placed> {
        assert!(!self.map.contains_key(&key), "a key is placed once");
        assert_eq!(
            coverage.len(),
            usize::from(w) * usize::from(h),
            "a whole mask"
        );
        assert!(w <= self.side && h <= self.side, "a mask fits the atlas");
        if w == 0 || h == 0 {
            let placed = Placed {
                slot: Slot::default(),
                off,
            };
            self.map.insert(
                key,
                Entry {
                    placed,
                    shelf: u16::MAX,
                },
            );
            return Some(placed);
        }
        let Some((shelf, slot)) = self.alloc(w, h) else {
            self.overflows += 1;
            return None;
        };
        let placed = Placed { slot, off };
        self.map.insert(key, Entry { placed, shelf });
        self.pending.push((slot, self.staging.len() as u32));
        self.staging.extend_from_slice(coverage);
        debug_assert!(self.shelves[usize::from(shelf)].used == self.frame);
        Some(placed)
    }

    /// A `w × h` slot on a shelf of its height class: the first with room, else a new shelf,
    /// else the least recently drawn tall enough shelf not drawn this frame, emptied.
    fn alloc(&mut self, w: u16, h: u16) -> Option<(u16, Slot)> {
        let class = h.div_ceil(CLASS) * CLASS;
        let fits = |s: &Shelf| s.h == class && self.side - s.x >= w;
        let at = match self.shelves.iter().position(fits) {
            Some(at) => at,
            None if self.side - self.next_y >= class => {
                self.shelves.push(Shelf {
                    y: self.next_y,
                    h: class,
                    x: 0,
                    used: 0,
                });
                self.next_y += class;
                self.shelves.len() - 1
            }
            None => {
                let frame = self.frame;
                let (at, _) = self
                    .shelves
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.used < frame && s.h >= class)
                    .min_by_key(|(_, s)| (s.used, s.h))?;
                self.evict(at);
                at
            }
        };
        let s = &mut self.shelves[at];
        assert!(self.side - s.x >= w && s.h >= h, "the shelf has room");
        let slot = Slot {
            x: s.x,
            y: s.y,
            w,
            h,
        };
        s.x += w;
        s.used = self.frame;
        Some((at as u16, slot))
    }

    /// Empties shelf `at`: its entries are dropped (drawn again, they are rasterized again).
    fn evict(&mut self, at: usize) {
        assert!(
            self.shelves[at].used < self.frame,
            "a shelf drawn this frame is kept"
        );
        let shelf = at as u16;
        self.map.retain(|_, e| e.shelf != shelf);
        self.shelves[at].x = 0;
        self.evictions += 1;
    }

    /// Slots written since the last [`Atlas::clear_pending`], each with the offset of its
    /// `w × h` bytes in the second slice, in write order (a later write wins).
    pub fn pending(&self) -> (&[(Slot, u32)], &[u8]) {
        (&self.pending, &self.staging)
    }

    /// Bytes written since the last [`Atlas::clear_pending`].
    pub fn pending_bytes(&self) -> usize {
        self.staging.len()
    }

    /// Forgets the pending writes, as the GPU's copy does.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.staging.clear();
    }

    /// Entries held, empty masks included.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no entry is held.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every held entry's key and slot (tests: overlaps, what survived an eviction).
    pub fn entries(&self) -> impl Iterator<Item = (&Key, Slot)> {
        self.map.iter().map(|(k, e)| (k, e.placed.slot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u64) -> Key {
        [PATH, i, 0]
    }

    fn mask(w: u16, h: u16, v: u8) -> Vec<u8> {
        vec![v; usize::from(w) * usize::from(h)]
    }

    /// Seeded sizes of 1–40 texels.
    fn sizes(n: usize) -> Vec<(u16, u16)> {
        let mut s = 0x2545_f491u32;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                ((s % 40) as u16 + 1, ((s >> 8) % 40) as u16 + 1)
            })
            .collect()
    }

    fn no_overlaps(a: &Atlas) {
        let live: Vec<Slot> = a.entries().map(|(_, s)| s).filter(|s| s.w > 0).collect();
        for (i, s) in live.iter().enumerate() {
            assert!(
                s.x + s.w <= a.side() && s.y + s.h <= a.side(),
                "inside: {s:?}"
            );
            for t in &live[i + 1..] {
                assert!(!s.overlaps(t), "{s:?} overlaps {t:?}");
            }
        }
    }

    /// Packing never overlaps, over many frames of random masks that keep evicting.
    #[test]
    fn packing_never_overlaps() {
        let mut a = Atlas::new(256);
        for (i, (w, h)) in sizes(4000).into_iter().enumerate() {
            if i % 50 == 0 {
                a.begin_frame();
            }
            let _ = a.insert(key(i as u64), w, h, [0, 0], &mask(w, h, 1));
            if i % 97 == 0 {
                no_overlaps(&a);
            }
        }
        no_overlaps(&a);
        assert!(a.evictions > 0, "the run evicted");
        // Negative space: the overlap check sees an overlap.
        let s = Slot {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        assert!(s.overlaps(&Slot {
            x: 3,
            y: 3,
            w: 4,
            h: 4
        }));
        assert!(
            !s.overlaps(&Slot {
                x: 4,
                y: 0,
                w: 4,
                h: 4
            }),
            "edges touch only"
        );
    }

    /// Eviction keeps every mask of the current frame: a full atlas evicts what earlier frames
    /// drew, and the masks this frame drew (looked up or inserted) keep their slots.
    #[test]
    fn eviction_keeps_the_current_frame() {
        let mut a = Atlas::new(64);
        // Frame 1 fills the atlas with 16 × 16 masks (16 of them).
        for i in 0..16 {
            assert!(a.insert(key(i), 16, 16, [0, 0], &mask(16, 16, 9)).is_some());
        }
        assert!(
            a.insert(key(99), 16, 16, [0, 0], &mask(16, 16, 9))
                .is_none(),
            "full"
        );
        // Frame 2 draws the first shelf's masks again, then needs room for new ones.
        a.begin_frame();
        let kept: Vec<Slot> = (0..4).map(|i| a.get(&key(i)).expect("held").slot).collect();
        for i in 100..108 {
            assert!(a.insert(key(i), 16, 16, [0, 0], &mask(16, 16, 7)).is_some());
        }
        for (i, s) in kept.iter().enumerate() {
            assert_eq!(a.get(&key(i as u64)).map(|p| p.slot), Some(*s), "kept {i}");
        }
        for i in 100..108 {
            assert!(a.get(&key(i)).is_some(), "this frame's new mask {i}");
        }
        assert_eq!(a.evictions, 2, "two shelves of the last frame made room");
        assert!(a.get(&key(4)).is_none(), "an evicted mask is gone");
        no_overlaps(&a);
    }

    /// A mask that would overflow (every tall enough shelf drawn this frame) is refused, and
    /// no texel of this frame's masks is written; once a frame passes, it evicts instead.
    #[test]
    fn an_overflowing_mask_evicts_rather_than_corrupts() {
        let mut a = Atlas::new(32);
        for i in 0..4 {
            assert!(a.insert(key(i), 32, 8, [0, 0], &mask(32, 8, 5)).is_some());
        }
        a.clear_pending();
        let before: Vec<(Key, Slot)> = a.entries().map(|(k, s)| (*k, s)).collect();
        assert!(a.insert(key(9), 8, 8, [0, 0], &mask(8, 8, 1)).is_none());
        assert_eq!(a.overflows, 1);
        assert_eq!(a.pending_bytes(), 0, "nothing written");
        let mut after: Vec<(Key, Slot)> = a.entries().map(|(k, s)| (*k, s)).collect();
        let mut before = before;
        before.sort();
        after.sort();
        assert_eq!(before, after, "every mask of this frame where it was");
        a.begin_frame();
        a.get(&key(0));
        let p = a
            .insert(key(9), 8, 8, [0, 0], &mask(8, 8, 1))
            .expect("evicts");
        assert!(
            !p.slot
                .overlaps(&a.get(&key(0)).expect("drawn this frame").slot)
        );
        assert_eq!(a.evictions, 1);
        no_overlaps(&a);
    }

    /// Pending writes carry each mask's bytes at its offset; an empty mask takes no texels and
    /// writes nothing.
    #[test]
    fn pending_writes_hold_the_masks() {
        let mut a = Atlas::new(64);
        let p = a
            .insert(key(1), 3, 2, [-1, 4], &[1, 2, 3, 4, 5, 6])
            .expect("fits");
        let e = a.insert(key(2), 0, 0, [0, 0], &[]).expect("empty");
        assert_eq!(e.slot.w, 0);
        assert_eq!(p.off, [-1, 4]);
        let (slots, bytes) = a.pending();
        assert_eq!(slots, &[(p.slot, 0)]);
        assert_eq!(bytes, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(a.len(), 2);
        a.clear_pending();
        assert_eq!(a.pending_bytes(), 0);
    }
}
