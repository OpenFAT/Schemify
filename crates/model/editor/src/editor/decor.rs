//! The decor step: the `checks` overlay source, violation markers and the decor primitives.
//!
//! Contents:
//! - `pub(super) struct DecorKey`
//! - `Editor::post_checks`
//! - `Editor::build_markers`
//! - `Editor::build_decor`

use schemify_doc::{DocHandle, ObjId};
use schemify_elab::AnnotationRequest;

use crate::checks;
use crate::checks::{SOURCE_CHECKS, SOURCE_OP};

use super::Editor;

/// Everything the decor draws besides the scene, as it was last built from: the decor is
/// rebuilt when any of it differs, or when the scene changed while the decor draws on it.
#[derive(Debug)]
pub(super) struct DecorKey {
    highlight: Vec<String>,
    insts: bool,
    incr: bool,
    compare: Option<(usize, bool)>,
    hier: Option<(DocHandle, u32)>,
    op: u32,
    /// The staged edit under review (`track_stage`'s key: the diff is a function of it).
    diff: Option<(DocHandle, u32)>,
}

impl Editor {
    /// The `checks` source of the annotation overlay from the active level's violations: when
    /// the cell state's last check pass continues the pass posted here, only its changes are
    /// patched in (an edit that leaves tens of thousands of `W004` rows alone touches none
    /// of their marks); otherwise (another document, a rebuilt state) everything is re-posted.
    pub(super) fn post_checks(&mut self) {
        let active = self.active().map(|a| a.0);
        let doc = active.and_then(|h| self.session.project.doc(h));
        let Some((c, d)) = self.cell.as_ref().zip(doc) else {
            if self.checks_key.take().is_some() {
                self.annotations.replace(AnnotationRequest {
                    source: SOURCE_CHECKS.to_owned(),
                    marks: Vec::new(),
                });
                self.marker_targets = None;
            }
            return;
        };
        if self.checks_key == Some((active, c.pass)) {
            return;
        }
        let continues = c.prev_pass != 0 && self.checks_key == Some((active, c.prev_pass));
        let delta = continues
            .then(|| {
                let gone = checks::violation_marks_of(&c.changes.0, c, d)?;
                let new = checks::violation_marks_of(&c.changes.1, c, d)?;
                Some((gone, new))
            })
            .flatten();
        self.checks_key = Some((active, c.pass));
        // The markers follow: the targets of the marks that changed, or everything.
        match delta {
            Some((gone, new)) if gone.is_empty() && new.is_empty() => {}
            Some((gone, new)) => {
                if let Some(t) = self.marker_targets.as_mut() {
                    t.extend(gone.iter().chain(&new).map(|m| m.target.clone()));
                }
                self.annotations.patch(SOURCE_CHECKS, gone, new);
            }
            None => {
                self.annotations.replace(checks::violation_request(c, d));
                self.marker_targets = None;
            }
        }
    }
    /// Places the violation markers where the scene or the `checks` marks changed since
    /// they were last placed (`checks::place_markers`: the objects the scene's last update
    /// touched and the targets of the patched marks, or everything after a scene build, a
    /// missed scene generation or a replaced overlay). Returns whether they changed.
    pub(super) fn build_markers(&mut self) -> bool {
        let Some(doc) = self.active().and_then(|(h, _)| self.session.project.doc(h)) else {
            return false;
        };
        let g = self.scene.generation;
        let targets = self.marker_targets.replace(Vec::new());
        let seen = self.markers.generation();
        let ids: Option<Vec<u32>> = match (targets, self.scene.touched()) {
            (Some(t), _) if seen == Some(g) && t.is_empty() => return false,
            (Some(t), _) if seen == Some(g) => Some(ids_of(&t, doc)),
            (Some(t), Some(keys)) if seen == Some(g.wrapping_sub(1)) => {
                let mut ids = ids_of(&t, doc);
                ids.extend(keys.iter().map(|k| k.key));
                ids.sort_unstable();
                ids.dedup();
                Some(ids)
            }
            _ => None,
        };
        checks::place_markers(
            &mut self.markers,
            &self.scene,
            doc,
            &self.annotations,
            ids.as_deref(),
        );
        self.canvas_gen[4] += 1;
        assert_eq!(self.markers.generation(), Some(g));
        true
    }
    /// Rebuilds the decor primitives (highlight, OP annotations, staged diff) when the
    /// overlay, the highlight, the name map or the staged diff changed, or the scene did and
    /// the decor draws something on it. Returns whether it rebuilt.
    pub(super) fn build_decor(&mut self, scene_stale: bool) -> bool {
        // What the decor draws follows the scene only while it has something to draw.
        let draws = !self.decor.is_empty()
            || !self.highlight.is_empty()
            || self.annotations.len() > self.annotations.count_of(SOURCE_CHECKS)
            || self.diff != checks::Diff::default();
        let current = self.decor_key.as_ref().is_some_and(|k| {
            k.highlight == self.highlight
                && (k.insts, k.incr) == (self.opts.highlight_insts, self.opts.incr_color)
                && k.compare == self.compare.as_ref().map(|c| (c.row, c.on))
                && k.hier == self.hier.as_ref().map(|h| (h.0, h.1))
                && k.op == self.op_gen
                && k.diff == self.diff_key
        });
        if current && !(scene_stale && draws) {
            return false;
        }
        self.decor_key = Some(DecorKey {
            highlight: self.highlight.clone(),
            insts: self.opts.highlight_insts,
            incr: self.opts.incr_color,
            compare: self.compare.as_ref().map(|c| (c.row, c.on)),
            hier: self.hier.as_ref().map(|h| (h.0, h.1)),
            op: self.op_gen,
            diff: self.diff_key,
        });
        let Some((h, _)) = self.active() else {
            return false;
        };
        // Components with a highlighted bit (a bus by any of its bits).
        let comps: Vec<u32> = match (self.highlight.is_empty(), self.cell.as_ref()) {
            (false, Some(cell)) => {
                let bits: Vec<(u32, String)> = (0..cell.nets.net_name.len() as u32)
                    .flat_map(|c| checks::bit_names(cell, c).map(move |b| (c, b.to_owned())))
                    .collect();
                let mut out: Vec<u32> = Vec::new();
                for (c, n) in &bits {
                    if out.last() == Some(c) {
                        continue;
                    }
                    let key = self.net_key(n);
                    if self.highlight.binary_search(&key).is_ok() {
                        out.push(*c);
                    }
                }
                out
            }
            _ => Vec::new(),
        };
        let prefix = self.level_prefix();
        let has_op = self.annotations.has_source(SOURCE_OP);
        // Net keys for annotation lookup: computed before borrowing the scene and the doc.
        let net_keys: Vec<(String, String)> = match (has_op, self.cell.as_ref()) {
            (true, Some(cell)) => {
                let names: Vec<String> = (0..cell.nets.net_name.len() as u32)
                    .map(|c| checks::net_name(cell, c).to_owned())
                    .collect();
                names
                    .into_iter()
                    .map(|n| (n.clone(), self.net_key(&n)))
                    .collect()
            }
            _ => Vec::new(),
        };
        let Some(doc) = self.session.project.doc(h) else {
            return false;
        };
        self.hl = match self.cell.as_ref() {
            Some(cell) if cell.inst_iface.len() == doc.instances().id.len() => {
                checks::members(doc, cell, &comps)
            }
            _ => checks::Members::default(),
        };
        let cell = self
            .cell
            .as_ref()
            .filter(|c| c.inst_iface.len() == doc.instances().id.len());
        checks::decor(
            &mut self.decor,
            &self.scene,
            doc,
            cell,
            &self.annotations,
            &self.hl,
            |n| net_keys.iter().find(|k| k.0 == n).map(|k| k.1.clone()),
            |inst| format!("{prefix}/{inst}"),
            &self.diff,
        );
        self.canvas_gen[1] += 1;
        true
    }
}

// ---- private ----

/// The object ids of mark targets `targets` (sorted, unique; a path or name target has none).
fn ids_of(targets: &[String], doc: &schemify_doc::Document) -> Vec<u32> {
    let s = doc.strings();
    let mut ids: Vec<u32> = targets
        .iter()
        .filter_map(|t| ObjId::find(t, s))
        .map(|id| id.0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert!(ids.len() <= targets.len());
    ids
}
