//! Cell file operations: Save As (as a schematic or a symbol), merging a cell into the open
//! one, deleting a cell, and applying the Edit-file dialog's text to a document.
//!
//! Contents:
//! - `Editor::save_as`
//! - `Editor::merge_cell`
//! - `Editor::delete_cell`
//! - `Editor::apply_file_text`

use std::path::PathBuf;

use schemify_doc::{DocHandle, EditRequest, Op, RecordBuf, Target as DocTarget};
use schemify_format::{ChangeKind, Document, RecordKind};
use schemify_plugin::abi::Event;

use crate::geometry::{Pt, Xform};
use crate::session::View;
use crate::{edit, geometry, platform};

use super::Editor;
use super::chooser::ChooseFor;
use super::tool::{Placement, Tool};

impl Editor {
    /// File → Save As: writes the active document's canonical text to `rel` (project-relative
    /// or absolute; the kind's extension is added when missing), indexes it, and edits the
    /// new file in place of the old one. Unsaved edits of the old file move to the new one:
    /// the old document is closed and its journal dropped, so it is not offered for recovery.
    pub(super) fn save_as(&mut self, h: DocHandle, rel: &str, symbol: bool) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let ext = doc.kind().word();
        let mut path = PathBuf::from(rel);
        if path.extension().is_none() {
            path.set_extension(ext);
        }
        if path.extension().and_then(|e| e.to_str()) != Some(ext) {
            self.session
                .say(format!("{}: a {ext} keeps its extension", path.display()));
            return;
        }
        let path = if path.is_absolute() {
            path
        } else {
            self.session.root.join(path)
        };
        if self.session.path(h) == Some(path.as_path()) {
            self.session.say("same file: use Save");
            return;
        }
        if platform::exists(&path) {
            self.session
                .say(format!("{}: exists, choose another name", path.display()));
            return;
        }
        let text = doc.canonical();
        if let Err(e) = schemify_format::parse(text.as_bytes()) {
            let d = e.diagnostics.first();
            self.session.say(format!(
                "save as refused: {}",
                d.map_or_else(String::new, |d| format!("{:?} {}", d.code, d.detail))
            ));
            return;
        }
        let dirty = self.session.project.is_dirty(h);
        if let Err(e) = platform::write_atomic(&path, text.as_bytes()) {
            self.session.say(e);
            return;
        }
        self.session.reindex(&path);
        self.symbols.clear();
        self.symbols_key = None;
        self.invalidate_cells();
        let new = match self.session.open_path(&path) {
            Ok(d) => d,
            Err(e) => {
                self.session.say(e);
                return;
            }
        };
        let view = if symbol {
            View::Symbol
        } else {
            View::Schematic
        };
        let shared = self
            .session
            .tabs()
            .iter()
            .enumerate()
            .flat_map(|(t, tab)| {
                tab.stack
                    .iter()
                    .enumerate()
                    .map(move |(l, lv)| (t, l, lv.doc))
            })
            .filter(|&(t, l, d)| {
                d == h
                    && !(t == self.session.active() && l + 1 == self.session.tabs()[t].stack.len())
            })
            .count();
        let at_top = self.session.tabs()[self.session.active()].stack.len() == 1;
        if at_top {
            self.sel_moved |= self.session.show(new, view, false);
        } else {
            self.session.open_tab(new, view);
        }
        if dirty && shared == 0 && at_top {
            self.session.close_discarding(h);
        }
        self.remember(new);
        let _ = self.host.push(
            None,
            &Event::DocOpened {
                doc: crate::plugins::doc_id(new),
            },
        );
        self.tool = Tool::Idle;
        self.session.say(format!("saved as {}", path.display()));
    }
    /// Inserts the schematic of indexed cell `row` attached to the pointer (File → Merge):
    /// its rank-3 records relative to their snapped anchor centroid, like a paste.
    pub(super) fn merge_cell(&mut self, row: usize) {
        let d = match self.session.open_cell(row) {
            Ok(d) => d,
            Err(e) => {
                self.session.say(e);
                return;
            }
        };
        let Some(doc) = self.session.project.doc(d) else {
            return;
        };
        let keys = edit::schematic_keys(doc);
        let recs: Vec<RecordBuf> = edit::selected_records(doc, &keys)
            .into_iter()
            .map(|(_, _, r)| r)
            .collect();
        if recs.is_empty() {
            self.session.say("that cell has no schematic objects");
            return;
        }
        let anchors: Vec<Pt> = recs.iter().filter_map(crate::geometry::anchor).collect();
        let n = anchors.len().max(1) as f64;
        let c = anchors.iter().fold((0.0, 0.0), |a, p| {
            (a.0 + f64::from(p.0) / n, a.1 + f64::from(p.1) / n)
        });
        let base = (
            geometry::snap(c.0, self.opts.snap),
            geometry::snap(c.1, self.opts.snap),
        );
        let back = Xform::translate(-base.0, -base.1);
        let recs: Vec<RecordBuf> = recs
            .into_iter()
            .filter_map(|mut r| crate::geometry::transform_record(&mut r, back).map(|()| r))
            .collect();
        self.session
            .say(format!("merge: {} object(s), click to place", recs.len()));
        self.tool = Tool::Place {
            what: Placement::Paste(recs),
            at: self.pointer_world_snapped(),
            lin: Xform::IDENTITY,
        };
    }
    /// File → Delete files: the first pick of a cell file arms its deletion and reopens the
    /// chooser; picking the same file again deletes it (refused while it is open).
    pub(super) fn delete_cell(&mut self, row: usize) {
        let p = &self.session.project;
        let cells = p.cells();
        let lib = cells.lib[row];
        let Some(dir) = self.session.lib_dir(lib) else {
            self.session.say("the bundled core library is read-only");
            return;
        };
        let path = dir.join(crate::session::cell_relpath(p, row));
        let open = self.session.handle_of(&path).is_some();
        if open {
            self.session.say(format!(
                "{}: open in the editor, close it first",
                path.display()
            ));
            return;
        }
        if self.pending_delete.as_ref() != Some(&path) {
            self.session.say(format!(
                "delete {}? pick it again to confirm",
                path.display()
            ));
            self.pending_delete = Some(path);
            self.open_chooser(ChooseFor::Delete);
            return;
        }
        self.pending_delete = None;
        match platform::remove(&path) {
            Ok(()) => {
                self.session.drop_journal(&path);
                self.session.reindex(&path);
                self.symbols.clear();
                self.symbols_key = None;
                self.recent.retain(|r| *r != crate::libs::absolute(&path));
                self.session.say(format!("deleted {}", path.display()));
            }
            Err(e) => self.session.say(e),
        }
    }
    /// Properties → Edit file OK: the typed text replaces the document in one transaction —
    /// changed records `put`, removed ones deleted, new ones inserted (format `diff` by
    /// identity); symbol graphics, which have no identity, are replaced as a block.
    pub(super) fn apply_file_text(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        text: &str,
    ) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let new = match schemify_format::parse(text.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                let d = e.diagnostics.first();
                self.session.say(format!(
                    "edit file: {}",
                    d.map_or_else(String::new, |d| format!(
                        "line {}: {:?} {}",
                        d.line, d.code, d.detail
                    ))
                ));
                return;
            }
        };
        if new.kind() != doc.kind() {
            self.session.say("edit file: the file kind cannot change");
            return;
        }
        let current = doc.canonical();
        let old = schemify_format::parse(current.as_bytes()).expect("canonical text parses");
        let d = schemify_format::diff(&old, &new);
        let mut ops = Vec::new();
        let target = |f: &Document<'_>, r: u32| -> DocTarget {
            let kind = f.record_kind(r);
            match kind {
                RecordKind::Header => DocTarget::header(),
                _ => DocTarget::new(kind, f.field(r, 0).unwrap_or("")),
            }
        };
        let graphic = |k: RecordKind| k.rank() == schemify_format::RANK_POSITIONAL;
        let mut graphics = false;
        for c in &d.changes {
            let kind = if c.b != schemify_format::NONE {
                new.record_kind(c.b)
            } else {
                old.record_kind(c.a)
            };
            if graphic(kind) {
                graphics = true;
                continue;
            }
            match c.kind {
                ChangeKind::Removed => ops.push(Op::Delete(target(&old, c.a))),
                ChangeKind::Added => ops.push(Op::Insert {
                    rec: RecordBuf::from_format(&new, c.b),
                    at: None,
                }),
                ChangeKind::Changed => ops.push(Op::Put {
                    target: target(&old, c.a),
                    rec: RecordBuf::from_format(&new, c.b),
                }),
            }
        }
        if graphics {
            // Delete every old graphic (row 0 each time), then insert the new ones in order.
            for &kind in &doc.graphics().shape {
                ops.push(Op::Delete(DocTarget::new(kind, "0")));
            }
            for r in 1..new.record_count() {
                if graphic(new.record_kind(r)) {
                    ops.push(Op::Insert {
                        rec: RecordBuf::from_format(&new, r),
                        at: None,
                    });
                }
            }
        }
        if ops.is_empty() {
            self.session.say("edit file: no change");
        }
        self.commit(reqs, h, "edit file", ops);
    }
}
