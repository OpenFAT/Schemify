//! Values of the shown documents: the active `CellState`, parked states, the D20 write-back.
//!
//! Contents:
//! - `pub(super) const PARKED_MAX`
//! - `pub(super) struct Parked`
//! - `Editor::track_elab`
//! - `Editor::recompute_elab`
//! - `Editor::reload_project`
//! - `Editor::invalidate_cells`
//! - `Editor::sync_cache`
//! - `Editor::sync_page`
//! - `pub(super) fn row_of_doc`

use schemify_doc::{DocHandle, Edit, EditRequest, Op, Outcome, Project, TableId};
use schemify_elab::CellState;
use schemify_expr::Value;
use schemify_ui::{CommandRequests, InputEvent, InputEvents, Key};

use crate::commands::Cmd;
use crate::scene::ObjKey;
use crate::{docs, spines};

use super::Editor;
use super::apply::push_req;

/// Cell states kept for documents that are not active ([`Editor`]'s `parked`).
pub(super) const PARKED_MAX: usize = 8;

/// The [`CellState`] of a document that is not active, with the doc epoch it is current for
/// (after `ops`), the plugin composition epoch, and the ops committed to it since.
#[derive(Debug)]
pub(super) struct Parked {
    doc: DocHandle,
    key: (u32, u32),
    state: CellState,
    ops: Vec<Op>,
}

impl Editor {
    /// Keeps the cell states in step with one applied edit (called before the epoch bump):
    /// the ops it applied are queued on the document's own state (active or parked) while
    /// that state is current, for the incremental recompute; a state that is not current
    /// rebuilds, and a state whose cell instantiates the edited one rebuilds (its interface
    /// may have changed).
    pub(super) fn track_elab(&mut self, r: &EditRequest, o: &Outcome) {
        let design = matches!(
            o,
            Outcome::Committed { .. } | Outcome::Undone { .. } | Outcome::Redone { .. }
        );
        if !design {
            return;
        }
        if let (Edit::Tx(tx), Outcome::Committed { seq }) = (&r.edit, o)
            && let Some((d, _, a_ops, done)) = self.amend.as_mut()
            && *d == r.doc
            && tx.ops.starts_with(a_ops)
        {
            *done = Some(*seq);
        }
        let epoch = self.session.epoch(r.doc);
        let project = &self.session.project;
        // The ops this outcome applied, as `doc` resolved them (fresh ids, an undo's inverse);
        // an extension's are the tail of its entry (the rest was queued with the commit).
        let applied = project.applied(r.doc, o).map(|ops| match &r.edit {
            Edit::Extend(_, ext) => &ops[ops.len() - ext.len()..],
            _ => ops,
        });
        let mut row: Option<Option<u32>> = None;
        let mut reads_edited = |state: &CellState| {
            let row = *row.get_or_insert_with(|| row_of_doc(project, r.doc));
            row.is_some_and(|row| state.ifaces.iter().skip(1).any(|f| f.row == row))
        };
        // The active state.
        match (self.cell_key, applied) {
            (Some(k), Some(ops)) if k.0 == r.doc && k.1 == epoch => {
                self.cell_ops.extend(ops.iter().cloned());
                self.cell_key = Some((k.0, epoch.wrapping_add(1), k.2)); // the bump that follows
            }
            (Some(k), _) if k.0 != r.doc && !self.cell.as_ref().is_some_and(&mut reads_edited) => {}
            _ => {
                self.cell_key = None;
                self.cell_ops.clear();
            }
        }
        // Parked states, the same way.
        self.parked.retain_mut(|p| {
            if p.doc != r.doc {
                return !reads_edited(&p.state);
            }
            match applied {
                Some(ops) if p.key.0 == epoch => {
                    p.ops.extend(ops.iter().cloned());
                    p.key.0 = epoch.wrapping_add(1);
                    true
                }
                _ => false,
            }
        });
        self.undid |= matches!(o, Outcome::Undone { .. } | Outcome::Redone { .. });
        debug_assert!(self.cell_key.is_some() || self.cell_ops.is_empty());
        debug_assert!(self.parked.len() <= PARKED_MAX);
    }
    /// `elab::CellState::recompute` for the active document: incremental over the queued ops
    /// when `cell` is current, else a full build (a document shown for the first time or
    /// since dropped, an undo whose entry `doc` no longer holds, a plugin change).
    /// Switching documents parks the state of the one left and takes back the parked state
    /// of the one shown while its key holds (D19: values kept for every open document).
    /// Plugin functions are called through the host (D20). Returns whether values may have
    /// changed.
    pub(super) fn recompute_elab(&mut self) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        // Every shown document is elaborated: its values (the page, `W = W_in` labels), its
        // violations (the live checks, D13) and its connectivity (net highlight, probes).
        // ponytail: structural edits rebuild the whole state (elab's recompute contract), 136 ms
        // at 100k objects; make elab's instance insert/delete incremental if cells that large
        // get edited structurally.
        let epoch = self.session.epoch(h);
        let fe = self.host.plugins.composed_epoch();
        if let Some(k) = self.cell_key.filter(|k| k.0 != h) {
            if let Some(state) = self.cell.take() {
                let ops = std::mem::take(&mut self.cell_ops);
                self.park(Parked {
                    doc: k.0,
                    key: (k.1, k.2),
                    state,
                    ops,
                });
            }
            self.cell_key = None;
            self.cell_ops.clear();
        }
        if self.cell_key.is_none()
            && let Some(i) = self.parked.iter().position(|p| p.doc == h)
        {
            let p = self.parked.swap_remove(i);
            if p.key == (epoch, fe) {
                self.cell = Some(p.state);
                self.cell_key = Some((h, epoch, fe));
                self.cell_ops = p.ops;
            }
        }
        let current = self.cell.is_some() && self.cell_key == Some((h, epoch, fe));
        if current && self.cell_ops.is_empty() {
            return false;
        }
        let ops = std::mem::take(&mut self.cell_ops);
        let (project, host) = (&self.session.project, &mut self.host);
        let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
        let rebuilt = if current {
            let cell = self.cell.as_mut().expect("current state");
            let done = cell.recompute(project, h, &ops, &mut funcs);
            // Symbol texts show values: the instances whose values changed are re-stamped
            // with the edit's objects (`scene::update`).
            let ids = &project.doc(h).expect("active").instances().id;
            self.revalued.extend(cell.revalued.iter().map(|&r| ObjKey {
                table: TableId::Instances,
                key: ids[r as usize].0,
            }));
            done.graph
        } else {
            self.cell = CellState::build(project, h, &mut funcs);
            true
        };
        self.cell_key = self.cell.as_ref().map(|_| (h, epoch, fe));
        // A rebuilt graph may have changed every value: the scene is built again.
        if rebuilt {
            self.values_gen = self.values_gen.wrapping_add(1);
        }
        assert!(self.cell_ops.is_empty());
        true
    }
    /// Keeps `p` among the parked states (replacing an older one of its document), dropping
    /// states of closed documents and, past [`PARKED_MAX`], the oldest.
    fn park(&mut self, p: Parked) {
        let project = &self.session.project;
        self.parked
            .retain(|q| q.doc != p.doc && project.doc(q.doc).is_some());
        if self.parked.len() >= PARKED_MAX {
            self.parked.remove(0);
        }
        self.parked.push(p);
        assert!(self.parked.len() <= PARKED_MAX);
    }
    /// `project.reload` (pdk spec §9.2): the session re-reads the manifest and lockfile and
    /// re-indexes; everything keyed by cell row (symbol cache, cell states, scenes) rebuilds.
    pub(super) fn reload_project(&mut self) {
        match self.session.reload() {
            Ok(()) => {
                let n = self.session.project.libs().name.len() - 1;
                if self.session.status().is_empty() {
                    self.session.say(format!("project reloaded: {n} libraries"));
                }
            }
            Err(e) => self.session.say(e),
        }
        // `pdk.use` reloads the project: the netlist dialect follows the new PDK's pin.
        if let Ok(Some((_, d))) = spines::pin(&self.session.project) {
            self.dialect = d;
        }
        self.symbols.clear();
        self.symbols_key = None;
        self.invalidate_cells();
        let docs: Vec<DocHandle> = self.session.project.handles().collect();
        for d in docs {
            self.session.bump(d);
        }
        assert!(self.cell_key.is_none() && self.symbols_key.is_none());
    }
    /// Drops every cell state (active and parked): plugin functions may answer differently,
    /// a document was recovered or the cell index changed.
    pub(super) fn invalidate_cells(&mut self) {
        self.cell_key = None;
        self.cell_ops.clear();
        self.parked.clear();
    }
    /// D20 write-back: when a loaded plugin computed a param default or var value whose
    /// `cached <value> <hash>` differs, writes it. After a docs-page edit the write extends
    /// that edit's undo entry and journal record (`Edit::Extend`): one edit, one undo, no
    /// extra journal records. Otherwise it is its own "update plugin cache" commit, skipped
    /// in a frame that undid or redid (it would clear the redo stack). Returns whether
    /// anything was written.
    pub(super) fn sync_cache(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
            return false;
        };
        if self.cell_key.map(|k| k.0) != Some(h) || doc.read_only() {
            return false;
        }
        let host = &self.host;
        let ops = docs::cache_ops(doc, cell, |fns| host.fn_hash(fns));
        if ops.is_empty() {
            return false;
        }
        match self.amend.as_ref().and_then(|a| a.3.filter(|_| a.0 == h)) {
            Some(seq) => push_req(reqs, h, Edit::Extend(seq, ops)),
            None if self.undid => return false,
            None => self.commit(reqs, h, "update plugin cache", ops),
        }
        let changed = self.apply(reqs);
        self.recompute_elab();
        changed
    }
    /// Follows the docs page's text editor: takes what is typed from its `field.typed`
    /// requests; when focus leaves it, Escape cancels and anything else (a click elsewhere)
    /// applies the text. Ctrl+Enter submits through `docs.prose_ok` instead.
    pub(super) fn sync_page(
        &mut self,
        events: &InputEvents,
        cmds: &CommandRequests,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some(row) = self.page.editing.as_ref().map(|e| e.0) else {
            return false;
        };
        let focus = self.shell.ui.focus();
        let id = docs::editor_id(row);
        // Typing reaches the page before the focus test: one batch may type and click away.
        let typed = Cmd::FieldTyped.name();
        for i in 0..cmds.len() {
            let s = &self.shell.strings;
            if s.get(cmds.command[i]) == typed
                && s.get(cmds.node[i]) == id
                && let Some(e) = self.page.editing.as_mut()
            {
                cmds.arg(i).clone_into(&mut e.1);
            }
        }
        if !focus.is_none() && self.shell.strings.get(focus) == id {
            self.page.focused = true;
            return false;
        }
        let submitted = (0..cmds.len())
            .any(|i| self.shell.strings.get(cmds.command[i]) == Cmd::DocsProseOk.name());
        if !self.page.focused || submitted {
            return false;
        }
        let escape = events
            .events
            .iter()
            .any(|e| matches!(e, InputEvent::Key { chord } if chord.key() == Key::ESCAPE));
        let Some((row, text)) = self.page.editing.take() else {
            return false;
        };
        self.page.focused = false;
        if !escape && let Some((h, _)) = self.active() {
            let ops = self.with_doc(h, |d, _| docs::prose_ops(d, row, &text));
            self.commit_docs(reqs, h, "edit text", ops);
        }
        true
    }
}

/// The cell row whose open document is `h` (a scan of the cell index); `None` for a
/// document outside it (a new, unsaved file).
pub(super) fn row_of_doc(project: &Project, h: DocHandle) -> Option<u32> {
    let doc = project.doc(h)?;
    let cells = project.cells();
    (0..cells.lib.len())
        .find(|&r| project.interface(r).is_some_and(|d| std::ptr::eq(d, doc)))
        .map(|r| r as u32)
}
