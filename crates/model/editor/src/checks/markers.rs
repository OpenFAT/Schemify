//! The violation markers of the `checks` source on the canvas (D13), kept per marked object.
//!
//! Contents:
//! - `pub struct Markers`, `Markers::edits`, `Markers::clear_edits`, `Markers::generation`
//! - `pub fn place_markers`

use std::collections::HashMap;
use std::iter;

use schemify_canvas::CanvasPrims;
use schemify_doc::{Document, ObjId};
use schemify_elab::{Annotations, Severity};

use crate::scene::{ObjKey, Scene, UNIT};

use super::{LAYER_ERROR, LAYER_INFO, LAYER_WARN, SOURCE_CHECKS};

/// The violation markers of the `checks` source on the canvas (D13): per marked object a box
/// around it and its code, per mark, on the severity's layer. Kept per object, so an edit
/// redraws only the objects whose marks or geometry changed ([`place_markers`]); a removed
/// object's rows die in place (`LAYER_DEAD`) like the scene's, and new rows append.
///
/// Data design: 1. in: the overlay's `checks` marks and the scene's object boxes; out: rect
/// and text rows; 2. ≤ 2^20 marks (tens of thousands on a large cell with floating pins);
/// 3. object ids `u32`, stamps `u32`; 4. a full placement walks the marks once, an update
/// only the marks of the objects it is given; 5. lives with the active level, rebuilt when
/// it changes; 6. not parallel.
#[derive(Clone, Debug, Default)]
pub struct Markers {
    /// The rows (rects and texts; dead rows after an update).
    pub prims: CanvasPrims,
    /// Stamp of each rect row and each text row (non-decreasing).
    stamps: [Vec<u32>; 2],
    /// Object id → the stamp its rows carry.
    by_id: HashMap<u32, u32>,
    next: u32,
    /// Rows killed since [`Markers::clear_edits`] (in `CanvasPrims::kill` kind order), or
    /// `None` after a full placement.
    killed: Option<[Vec<u32>; 6]>,
    dead: usize,
    /// The scene generation the markers were placed against.
    generation: Option<u64>,
}

impl Markers {
    /// The rows killed since the last [`Markers::clear_edits`] (with the rows appended
    /// since, every change in that time), or `None` when a full placement made the table
    /// anew in that time.
    #[must_use]
    pub fn edits(&self) -> Option<&[Vec<u32>; 6]> {
        self.killed.as_ref()
    }

    /// Starts the next [`Markers::edits`] window.
    pub fn clear_edits(&mut self) {
        let killed = self.killed.get_or_insert_with(Default::default);
        for rows in killed.iter_mut() {
            rows.clear();
        }
        debug_assert!(self.edits().is_some_and(|k| k.iter().all(Vec::is_empty)));
    }

    /// The scene generation the markers were last placed against (`None`: never).
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Draws the `checks` marks on object `id` (none when it is not drawn).
    fn place(&mut self, id: ObjId, scene: &Scene, doc: &Document, ann: &Annotations) {
        let Some(loc) = doc.find_id(id) else { return };
        let key = ObjKey {
            table: loc.table,
            key: id.0,
        };
        let Some(o) = scene.find(key) else { return };
        assert!(
            !self.by_id.contains_key(&id.0),
            "an object is unplaced before it is placed"
        );
        assert_eq!(
            self.stamps[0].len(),
            self.prims.rects.x0.len(),
            "rect stamps in step"
        );
        assert_eq!(
            self.stamps[1].len(),
            self.prims.texts.x.len(),
            "text stamps in step"
        );
        let b = scene.objs.bbox(o as usize);
        let target = id.text(doc.strings());
        let (stamp, rects, texts) = (
            self.next,
            self.prims.rects.x0.len(),
            self.prims.texts.x.len(),
        );
        for m in ann.on_by(&target, SOURCE_CHECKS) {
            let layer = match m.severity {
                Severity::Error => LAYER_ERROR,
                Severity::Warning => LAYER_WARN,
                Severity::Info => LAYER_INFO,
            };
            let pad = 2 * UNIT;
            self.prims
                .rect(b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad, layer);
            let code = m.text.split(' ').next().unwrap_or("");
            if !code.is_empty() {
                self.prims
                    .text(b[0] - pad, b[1] - pad - 7 * UNIT, 6 * UNIT, layer, code);
            }
        }
        let (r, t) = (
            self.prims.rects.x0.len() - rects,
            self.prims.texts.x.len() - texts,
        );
        if r + t == 0 {
            return;
        }
        self.stamps[0].extend(iter::repeat_n(stamp, r));
        self.stamps[1].extend(iter::repeat_n(stamp, t));
        self.by_id.insert(id.0, stamp);
        self.next += 1;
    }

    /// Kills the rows of object `id`.
    fn unplace(&mut self, id: u32) {
        let Some(stamp) = self.by_id.remove(&id) else {
            return;
        };
        for (col, kind) in [(0usize, 1u8), (1, 5)] {
            let s = &self.stamps[col];
            let lo = s.partition_point(|&x| x < stamp);
            let hi = lo + s[lo..].partition_point(|&x| x == stamp);
            assert!(hi <= s.len());
            for r in lo..hi {
                self.prims.kill(kind, r);
            }
            if let Some(k) = self.killed.as_mut() {
                k[usize::from(kind)].extend((lo..hi).map(|r| r as u32));
            }
            self.dead += hi - lo;
        }
        assert!(self.dead <= self.prims.len());
    }
}

/// Places the violation markers of the overlay's `checks` source on `scene` (generation
/// `scene.generation`): all of them anew when `ids` is `None`, a quarter of the rows are
/// dead, or there were none, else only those of objects `ids` (object id values: every
/// object whose marks or geometry changed since the last placement, sorted, unique). The
/// result equals a full placement as a set of live rows.
///
/// Cost: O(marks · log) for a full placement; O(ids · (log rows + their marks)) otherwise.
///
/// # Panics
///
/// An object of `ids` placed twice (they must be unique), or the stamp columns out of step
/// with the rows (a bug in this module).
pub fn place_markers(
    m: &mut Markers,
    scene: &Scene,
    doc: &Document,
    ann: &Annotations,
    ids: Option<&[u32]>,
) {
    debug_assert!(ids.is_none_or(|i| i.windows(2).all(|w| w[0] < w[1])));
    // ponytail: once a quarter of the rows are dead the markers are placed anew (O(marks),
    // ~4 ms at 70k marks, once per thousands of edits); compacting in place would amortize it.
    let compact = m.dead * 4 > m.prims.len() + (1 << 12);
    match ids {
        Some(ids) if !compact && m.generation.is_some() => {
            for &id in ids {
                m.unplace(id);
                m.place(ObjId(id), scene, doc, ann);
            }
        }
        _ => {
            *m = Markers::default();
            let s = doc.strings();
            let mut last: Option<&str> = None;
            for (src, mark) in ann.iter() {
                if src != SOURCE_CHECKS || last == Some(mark.target.as_str()) {
                    continue;
                }
                last = Some(mark.target.as_str());
                if let Some(id) = ObjId::find(&mark.target, s) {
                    m.place(id, scene, doc, ann);
                }
            }
        }
    }
    m.generation = Some(scene.generation);
    assert_eq!(m.stamps[0].len(), m.prims.rects.x0.len());
    assert_eq!(m.stamps[1].len(), m.prims.texts.x.len());
}
