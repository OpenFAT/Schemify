//! Highlight → compare schematics, and the parity decor (`extra`) it is drawn into.
//!
//! Contents:
//! - `pub(super) const HILITE_LAYERS`
//! - `pub(crate) struct Compare`
//! - `Editor::compare_with`
//! - `Editor::compare_swap`
//! - `Editor::build_extra`
//! - `Editor::draw_compare`

use schemify_canvas::CanvasPrims;
use schemify_doc::{DocHandle, RecordBuf, TableId};

use crate::geometry::Xform;
use crate::scene::{LAYER_SEL, ObjKey, UNIT};
use crate::session::View;
use crate::{checks, scene};

use super::Editor;

/// Layers the rotating highlight colors use (palette layers with distinct hues).
pub(super) const HILITE_LAYERS: [u8; 8] = [8, 9, 10, 12, 13, 15, 17, 21];

/// The comparison of the active document with another cell (Highlight → compare).
#[derive(Clone, Debug)]
pub(crate) struct Compare {
    /// The other cell's index row.
    pub(super) row: usize,
    /// Shown.
    pub(super) on: bool,
    /// (document, edit epoch, other's epoch) the drawn diff is current for.
    pub(super) key: Option<(DocHandle, u32, u32)>,
}

impl Editor {
    /// Starts comparing the active document with indexed cell `row`.
    pub(super) fn compare_with(&mut self, row: usize) {
        self.compare = Some(Compare {
            row,
            on: true,
            key: None,
        });
        let name = crate::session::cell_relpath(&self.session.project, row);
        self.session.say(format!("comparing with {name}"));
    }
    /// Highlight → Swap compare schematics: the compared cell becomes the active level and
    /// the previous document the reference.
    pub(super) fn compare_swap(&mut self, h: DocHandle) {
        let Some(c) = self.compare.clone() else {
            self.session.say("choose a cell to compare with first");
            return;
        };
        let Some(prev_row) = super::row_of_doc(&self.session.project, h).map(|r| r as usize) else {
            return;
        };
        match self.session.open_cell(c.row) {
            Ok(d) => {
                self.sel_moved |= self.session.show(d, View::Schematic, true);
                self.compare = Some(Compare {
                    row: prev_row,
                    on: true,
                    key: None,
                });
            }
            Err(e) => self.session.say(e),
        }
    }
    /// Rebuilds the parity decor (`extra`): the comparison (objects added or changed here
    /// outlined, objects only in the other cell drawn), per-net highlight colors and
    /// connected-instance outlines. Called after `build_decor` with whether the decor or the
    /// scene changed; returns whether it rebuilt.
    pub(super) fn build_extra(&mut self, decor_rebuilt: bool) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let epoch = self.session.epoch(h);
        let compare_stale = self
            .compare
            .as_ref()
            .is_some_and(|c| c.on && c.key.is_none_or(|k| k.0 != h || k.1 != epoch));
        // Follows the decor (scene, highlight, overlay changes); the options that change it
        // mark the decor dirty when toggled.
        if !decor_rebuilt && !compare_stale {
            return false;
        }
        let mut out = CanvasPrims::default();
        if self.opts.incr_color && !self.highlight.is_empty() {
            self.colored_highlight(&mut out);
        }
        if self.opts.highlight_insts {
            let r = 2 * UNIT;
            let mut rows: Vec<u32> = self.hl.pins.iter().map(|p| p.0).collect();
            rows.dedup();
            if let Some(doc) = self.session.project.doc(h) {
                for row in rows {
                    let Some(&id) = doc.instances().id.get(row as usize) else {
                        continue;
                    };
                    let key = ObjKey {
                        table: TableId::Instances,
                        key: id.0,
                    };
                    if let Some(o) = self.scene.find(key) {
                        let b = self.scene.objs.bbox(o as usize);
                        out.rect(b[0] - r, b[1] - r, b[2] + r, b[3] + r, checks::LAYER_HILITE);
                    }
                }
            }
        }
        if let Some(c) = self.compare.clone()
            && c.on
            && view == View::Schematic
        {
            self.draw_compare(h, c.row, &mut out);
            if let Some(cm) = self.compare.as_mut() {
                cm.key = Some((h, epoch, 0));
            }
        }
        if out.is_empty() && self.extra.is_empty() {
            return false; // nothing drawn before or now
        }
        self.extra = out;
        self.canvas_gen[2] += 1;
        true
    }
    /// The comparison drawn into `out`: objects of the active document added or changed
    /// against cell `row` outlined (diff layer), objects only in `row` drawn in the error
    /// layer at their place.
    pub(super) fn draw_compare(&mut self, h: DocHandle, row: usize, out: &mut CanvasPrims) {
        let Ok(other) = self.session.open_cell(row) else {
            return;
        };
        let (Some(a), Some(b)) = (self.session.project.doc(other), self.session.project.doc(h))
        else {
            return;
        };
        let (ta, tb) = (a.canonical(), b.canonical());
        let (Ok(fa), Ok(fb)) = (
            schemify_format::parse(ta.as_bytes()),
            schemify_format::parse(tb.as_bytes()),
        ) else {
            return;
        };
        let d = schemify_format::diff(&fa, &fb);
        let pad = 4 * UNIT;
        let mut removed: Vec<RecordBuf> = Vec::new();
        let mut changed = 0usize;
        for c in &d.changes {
            match c.kind {
                schemify_format::ChangeKind::Removed => {
                    if fa.record_kind(c.a).rank() == 3 {
                        removed.push(RecordBuf::from_format(&fa, c.a));
                    }
                }
                _ => {
                    let Some(id) = fb.field(c.b, 0) else { continue };
                    let Some(oid) = schemify_doc::ObjId::find(id, b.strings()) else {
                        continue;
                    };
                    let Some(loc) = b.find_id(oid) else { continue };
                    let key = ObjKey {
                        table: loc.table,
                        key: oid.0,
                    };
                    if let Some(o) = self.scene.find(key) {
                        let bb = self.scene.objs.bbox(o as usize);
                        out.rect(
                            bb[0] - pad,
                            bb[1] - pad,
                            bb[2] + pad,
                            bb[3] + pad,
                            checks::LAYER_DIFF,
                        );
                        changed += 1;
                    }
                }
            }
        }
        scene::preview_records(&removed, Xform::IDENTITY, out);
        // Preview prims come on the selection layer: recolor them as removed (error layer).
        for l in &mut out.lines.layer {
            if *l == LAYER_SEL {
                *l = checks::LAYER_ERROR;
            }
        }
        for l in &mut out.rects.layer {
            if *l == LAYER_SEL {
                *l = checks::LAYER_ERROR;
            }
        }
        self.session.say(format!(
            "compare: {changed} added/changed here, {} only in the other cell",
            removed.len()
        ));
    }
}
