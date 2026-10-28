//! Edit requests: commit helpers, plugin delivery, `doc::apply_edits` and its bookkeeping.
//!
//! Contents:
//! - `Editor::with_doc`
//! - `Editor::commit`
//! - `Editor::commit_docs`
//! - `Editor::deliver`
//! - `Editor::snapshot_now`
//! - `Editor::apply`
//! - `pub(super) fn push_req`

use schemify_doc::{
    DocHandle, Document, Edit, EditRequest, Mode, Op, Outcome, TableId, Transaction, apply_edits,
    ops_from_text,
};
use schemify_plugin::abi::Event;
use schemify_ui::{CommandRequests, Sym};

use crate::scene::ObjKey;
use crate::{docs, platform, plugins};

use super::Editor;
use super::queue::KEPT_LOG_MAX;
use crate::plugins::PluginEdit;

/// Edit requests one frame may carry.
const REQUESTS_MAX: usize = 1 << 10;

impl Editor {
    pub(super) fn with_doc<R>(&self, h: DocHandle, f: impl FnOnce(&Document, &[ObjKey]) -> R) -> R
    where
        R: Default,
    {
        let sel = self.selection();
        self.session
            .project
            .doc(h)
            .map_or_else(R::default, |d| f(d, sel))
    }
    /// Appends a commit transaction (skipped when empty).
    pub(super) fn commit(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        label: &str,
        ops: Vec<Op>,
    ) {
        assert!(!label.is_empty(), "an undo entry has a label");
        if ops.is_empty() {
            return;
        }
        let before = reqs.len();
        let tx = Transaction {
            label: label.to_owned(),
            mode: Mode::Commit,
            ops,
        };
        push_req(reqs, h, Edit::Tx(tx));
        assert_eq!(reqs.len(), before + 1, "one transaction per commit");
    }
    /// [`Editor::commit`] for a docs-page edit, remembered so a D20 cache write it causes
    /// lands in the same undo entry (`sync_cache`).
    pub(super) fn commit_docs(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        label: &str,
        ops: Vec<Op>,
    ) {
        if ops.is_empty() {
            return;
        }
        self.amend = Some((h, label.to_owned(), ops.clone(), None));
        self.commit(reqs, h, label, ops);
        assert!(self.amend.as_ref().is_some_and(|a| a.0 == h));
    }
    /// `plugin::deliver`: snapshots, events, and the plugins' requests into `reqs`.
    pub(super) fn deliver(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        if !self.host.has_work() {
            return false;
        }
        if !self.host.plugins.ids().is_empty() {
            self.snapshot_now(); // the tables are for plugins; the user config reads none
        }
        let root = self.session.root.clone();
        let d = self.host.deliver(&self.session.project, &root, self.now);
        if !d.commands.is_empty() {
            // `schemify.cmd` from the user config: dispatched like key presses.
            let mut cmds = CommandRequests::default();
            for (name, args) in &d.commands {
                cmds.command.push(self.shell.strings.intern(name));
                cmds.node.push(Sym::NONE);
                cmds.args.push_str(args);
                cmds.arg_end.push(cmds.args.len() as u32);
            }
            self.dispatch_all(&cmds, reqs);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.sim.enqueue(d.runs);
        for PluginEdit {
            doc,
            label,
            ops: text,
            stage,
        } in d.edits
        {
            let Some(kind) = self.session.project.doc(doc).map(Document::kind) else {
                continue;
            };
            match ops_from_text(&text, kind) {
                Ok(ops) => {
                    let mode = if stage { Mode::Stage } else { Mode::Commit };
                    push_req(reqs, doc, Edit::Tx(Transaction { label, mode, ops }));
                }
                Err(e) => {
                    self.session
                        .say(format!("plugin edit rejected: {:?}", e.first()));
                }
            }
        }
        if let Some(l) = d.logs.last() {
            self.session.say(l.clone());
        }
        if let Some((lines, failed)) = &mut self.kept_log {
            // Bounded by the caller draining it each frame; a stuck caller keeps the newest.
            lines.extend(d.logs.iter().cloned());
            let over = lines.len().saturating_sub(KEPT_LOG_MAX);
            lines.drain(..over);
            assert!(lines.len() <= KEPT_LOG_MAX);
            *failed |= d.failed;
        }
        self.resolve_said(&d.logs);
        if d.state_changed {
            self.invalidate_cells(); // plugin functions may answer differently now (D20)
        }
        true
    }
    /// Fills the plugin snapshot tables from the active document, its cell state, the
    /// selection and the run's rows (`Host::snapshot`, which refills only what moved).
    pub(super) fn snapshot_now(&mut self) {
        let active = self.active().map(|a| a.0);
        let sel: Vec<u32> = self
            .selection()
            .iter()
            .filter(|k| !matches!(k.table, TableId::Pins | TableId::Graphics))
            .map(|k| k.key)
            .collect();
        assert!(sel.len() <= self.selection().len());
        #[cfg(not(target_arch = "wasm32"))]
        let rows = self.sim.result_rows();
        #[cfg(target_arch = "wasm32")]
        let rows = Vec::new();
        let cell = self
            .cell
            .as_ref()
            .filter(|_| self.cell_key.is_some_and(|k| Some(k.0) == active));
        // What the document tables are filled from: unchanged on a click, so a selection
        // change does not copy every row again.
        let key = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            let project = &self.session.project;
            for d in project.handles() {
                (d, project.is_dirty(d), self.session.epoch(d)).hash(&mut h);
            }
            (active, cell.map(|c| c.pass), &rows).hash(&mut h);
            h.finish()
        };
        self.host
            .snapshot(&self.session.project, active, &sel, cell, &rows, key);
        self.snapshot_fresh = true;
    }
    /// `doc::apply_edits`, then per-document bookkeeping: edit epochs, journals, plugin
    /// `DocChanged` events, and the status line for refusals. Every transaction first drops
    /// the D20 caches of the expressions it changes (`docs::drop_stale_caches`).
    pub(super) fn apply(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        assert!(reqs.len() <= REQUESTS_MAX);
        for r in reqs.iter_mut() {
            if let Edit::Tx(tx) = &mut r.edit
                && let Some(doc) = self.session.project.doc(r.doc)
            {
                docs::drop_stale_caches(doc, &mut tx.ops);
            }
        }
        let outcomes = apply_edits(&mut self.session.project, reqs);
        assert_eq!(outcomes.len(), reqs.len());
        let mut changed = false;
        for (r, o) in reqs.iter().zip(&outcomes) {
            match o {
                Outcome::Committed { .. }
                | Outcome::Undone { .. }
                | Outcome::Redone { .. }
                | Outcome::Staged { .. }
                | Outcome::Dropped => {
                    self.track_elab(r, o);
                    self.session.bump(r.doc);
                    // A docs-page commit's record waits for its cache ops (`sync_cache`).
                    if !self.amend.as_ref().is_some_and(|a| a.0 == r.doc) {
                        self.session.flush_journal(r.doc);
                    }
                    self.generation = self.generation.wrapping_add(1);
                    let ev = Event::DocChanged {
                        doc: plugins::doc_id(r.doc),
                        epoch: self.session.epoch(r.doc),
                    };
                    let _ = self.host.push(None, &ev);
                    changed = true;
                }
                Outcome::Nothing => self.session.say("nothing to undo/redo"),
                Outcome::Rejected(errs) => {
                    let e = errs.first().map_or(String::new(), |e| {
                        format!("{} {}", e.code, e.code.meaning())
                    });
                    self.session.say(format!("edit rejected: {e}"));
                }
                Outcome::Refused(c) => {
                    self.session
                        .say(format!("edit refused: {c} {}", c.meaning()));
                }
            }
        }
        reqs.clear();
        changed
    }
}

pub(super) fn push_req(reqs: &mut Vec<EditRequest>, doc: DocHandle, edit: Edit) {
    assert!(reqs.len() < REQUESTS_MAX, "edit requests per frame");
    reqs.push(EditRequest {
        doc,
        edit,
        unix_ms: platform::unix_ms(),
    });
    assert!(reqs.len() <= REQUESTS_MAX);
}
