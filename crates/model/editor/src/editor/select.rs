//! The selection: set, hit-tested, boxed out, its bounds and pivot.
//!
//! Contents:
//! - `Editor::set_selection`
//! - `Editor::unselected`
//! - `Editor::hit_at`
//! - `Editor::selection_bbox`
//! - `Editor::selection_pivot`
//! - `Editor::one_instance`

use schemify_doc::TableId;

use crate::geometry::Pt;
use crate::scene::ObjKey;
use crate::{edit, geometry, scene};

use super::Editor;
use super::tool::HIT_PX;

impl Editor {
    pub(super) fn set_selection(&mut self, sel: Vec<ObjKey>) {
        if self.session.select(sel) {
            self.sel_changed = true;
            self.sel_moved = true;
        }
        debug_assert!(
            self.selection().windows(2).all(|w| w[0] < w[1]),
            "sorted, distinct"
        );
    }
    /// The selection without the scene objects `hits` (an unselect box). While the scene's
    /// selected set is current and holds every selected key, a bitset of the hits and one
    /// pass over the objects do it: O(objects + hits), ~0.2 ms at 100k, where a search of
    /// the hits per selected key cost ~7 ms (HANDOFF item 3's sort and binary search).
    /// Otherwise (keys the scene does not draw) each key is looked up in the scene.
    pub(super) fn unselected(&self, hits: &[u32]) -> Vec<ObjKey> {
        let objs = &self.scene.objs;
        let n = objs.len();
        let mut hit = vec![0u64; n.div_ceil(64)];
        for &o in hits {
            assert!((o as usize) < n, "a hit is a scene object");
            hit[o as usize / 64] |= 1 << (o % 64);
        }
        let is_hit = |o: u32| hit[o as usize / 64] >> (o % 64) & 1 != 0;
        let sel = self.selection();
        let current = !self.sel_changed && self.selected.universe() == n;
        let out: Vec<ObjKey> = if current && self.selected.len() == sel.len() {
            (0..n as u32)
                .filter(|&o| self.selected.contains(o) && !is_hit(o))
                .map(|o| objs.key[o as usize])
                .collect()
        } else {
            let drawn = |k: ObjKey| self.scene.find(k);
            sel.iter()
                .copied()
                .filter(|&k| drawn(k).is_none_or(|o| !is_hit(o)))
                .collect()
        };
        assert!(out.len() <= sel.len());
        out
    }
    pub(super) fn hit_at(&self, s: (f32, f32)) -> Option<u32> {
        let tol = HIT_PX / self.camera().scale;
        let o = scene::hit(&self.scene.objs, self.world_at(s), tol);
        assert!(o.is_none_or(|o| (o as usize) < self.scene.objs.len()));
        o
    }
    pub(super) fn selection_bbox(&self) -> Option<[i32; 4]> {
        let mut b: Option<[i32; 4]> = None;
        for &k in self.selection() {
            if let Some(i) = self.scene.find(k) {
                let o = self.scene.objs.bbox(i as usize);
                b = Some(match b {
                    None => o,
                    Some(c) => [
                        c[0].min(o[0]),
                        c[1].min(o[1]),
                        c[2].max(o[2]),
                        c[3].max(o[3]),
                    ],
                });
            }
        }
        assert!(
            b.is_none_or(|b| b[0] <= b[2] && b[1] <= b[3]),
            "an ordered box"
        );
        b
    }
    /// Centroid of the selected objects' anchors, snapped: the group rotate/flip pivot and
    /// the clipboard origin. A single object turns about its own anchor (instance origin,
    /// label point, first wire end), which keeps it on the grid; drawn extents (texts) would
    /// move the pivot with font metrics.
    pub(super) fn selection_pivot(&self) -> Pt {
        let anchors: Vec<Pt> = self
            .active()
            .and_then(|(h, _)| self.session.project.doc(h))
            .map(|doc| {
                edit::selected_records(doc, self.selection())
                    .iter()
                    .filter_map(|(_, _, r)| crate::geometry::anchor(r))
                    .collect()
            })
            .unwrap_or_default();
        if anchors.is_empty() {
            return self.pointer_world_snapped();
        }
        assert!(
            anchors.len() <= self.selection().len(),
            "one anchor per object at most"
        );
        let n = anchors.len() as f64;
        let c = anchors.iter().fold((0.0, 0.0), |a, p| {
            (a.0 + f64::from(p.0) / n, a.1 + f64::from(p.1) / n)
        });
        (
            geometry::snap(c.0, self.opts.snap),
            geometry::snap(c.1, self.opts.snap),
        )
    }
    /// The single selected instance (or the instance under the pointer).
    pub(super) fn one_instance(&self) -> Option<ObjKey> {
        let sel: Vec<ObjKey> = self
            .selection()
            .iter()
            .copied()
            .filter(|k| k.table == TableId::Instances)
            .collect();
        assert!(sel.len() <= self.selection().len());
        match sel.as_slice() {
            [k] => Some(*k),
            [] => self
                .hit_at(self.pointer)
                .map(|o| self.scene.objs.key[o as usize])
                .filter(|k| k.table == TableId::Instances),
            _ => None,
        }
    }
}
