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

use schemify_canvas::CanvasPrims;
use schemify_doc::{Cursor, DocHandle, TableId};

use crate::geometry::Pt;
use crate::session::View;
mod dots;
use dots::Wiring;
mod symbols;
pub use symbols::{SymPrims, Symbols};
mod refresh;
pub use refresh::{build, update};
mod draw;
mod select;
pub use select::{Selected, box_select, hit, marks, selected_prims};
mod preview;
pub use preview::{preview_records, preview_records_on, push_moved, stamp};

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
/// as dead rows ([`schemify_canvas::LAYER_DEAD`], owner `u32::MAX`) so every other row
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
    index: KeyMap,
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

/// Object key → object index ([`Scene::find`]); keys packed into a `u64`, hashed with one
/// multiply (never iterated, so the order is never seen).
type KeyMap = HashMap<u64, u32, std::hash::BuildHasherDefault<KeyHasher>>;

fn pack(k: ObjKey) -> u64 {
    (k.table as u64) << 32 | u64::from(k.key)
}

/// One multiply per key: object ids are random 30-bit values, so their bits spread already.
#[derive(Default)]
struct KeyHasher(u64);

impl std::hash::Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (n ^ (n >> 32)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Scene {
    /// Object index of `key`, if drawn.
    pub fn find(&self, key: ObjKey) -> Option<u32> {
        let o = self.index.get(&pack(key)).copied();
        debug_assert!(o.is_none_or(|o| self.objs.key[o as usize] == key));
        o
    }

    /// The rows [`update`]s killed or rewrote in place (a revalued text, same box) since the
    /// last [`Scene::clear_edits`], per kind in [`CanvasPrims::kill`] order: with the rows
    /// appended since, every change of `prims` in that time (a canvas diffs them as
    /// `TableChange::Rows`). `None` when a [`build`] made
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

#[cfg(test)]
mod tests {
    use super::select::hit_one;
    use super::*;

    /// The f64 box test object by object: the oracle of [`box_select`].
    fn box_select_scalar(objs: &Objects, r: [f64; 4], touch: bool) -> Vec<u32> {
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
        out
    }

    /// `n` objects with random boxes and segments in a ±`span` square (an xorshift stream).
    fn random_objects(n: usize, span: i32, seed: u64) -> Objects {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % (2 * span as u64 + 1)) as i32 - span
        };
        let mut objs = Objects::default();
        for i in 0..n {
            let g = [next(), next(), next(), next()];
            let key = ObjKey {
                table: TableId::Wires,
                key: i as u32,
            };
            objs.push(key, g, i % 3 == 0);
        }
        objs
    }

    /// Oracle: the i32 branchless box select keeps exactly what the f64 test keeps, for
    /// boxes on, between and outside integer coordinates, both drag directions, empty and
    /// inverted boxes, and sizes around every lane boundary.
    #[test]
    fn box_select_equals_the_scalar_box_test() {
        for n in [0, 1, 2, 7, 8, 9, 63, 64, 65, 1000] {
            let objs = random_objects(n, 50, n as u64 + 3);
            for r in [
                [-10.0, -10.0, 10.0, 10.0],
                [10.5, 10.5, -10.5, -10.5],
                [-0.5, -60.0, 0.5, 60.0],
                [-60.0, -60.0, 60.0, 60.0],
                [3.0, 3.0, 3.0, 3.0],
                [1e12, 1e12, 2e12, 2e12],
                [-1e12, -1e12, 1e12, 1e12],
            ] {
                for touch in [false, true] {
                    assert_eq!(
                        box_select(&objs, r, touch),
                        box_select_scalar(&objs, r, touch),
                        "n {n} box {r:?} touch {touch}"
                    );
                }
            }
        }
    }

    /// Selectivity sweep at 10^6 objects, scalar against branchless (`cargo test --release
    /// -p schemify-editor box_select_sweep -- --ignored --nocapture`).
    #[test]
    #[ignore = "benchmark"]
    fn box_select_sweep() {
        let objs = random_objects(1_000_000, 1_000_000, 7);
        for pct in [0, 10, 25, 50, 75, 90, 100] {
            let h = -1e6 + 2e6 * f64::from(pct) / 100.0;
            let r = [-1e6 - 1.0, -1e6 - 1.0, 1e6 + 1.0, h];
            let time = |f: &dyn Fn() -> Vec<u32>| {
                let t = std::time::Instant::now();
                let mut n = 0;
                for _ in 0..10 {
                    n += std::hint::black_box(f()).len();
                }
                (t.elapsed().as_secs_f64() * 100.0, n / 10)
            };
            let (a, na) = time(&|| box_select_scalar(&objs, r, true));
            let (b, nb) = time(&|| box_select(&objs, r, true));
            assert_eq!(na, nb);
            eprintln!("touch {pct:3}%: scalar {a:6.2} ms, branchless {b:6.2} ms ({na} hits)");
        }
    }

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
