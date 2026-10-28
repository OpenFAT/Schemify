//! Files, tabs and hierarchy: open, save as, reload, recent files, project switch, merge, delete.

use std::path::{Path, PathBuf};

use schemify_doc::{
    DocHandle, Document, EditRequest, External, Op, RecordBuf, Target as DocTarget,
};
use schemify_format::{FileKind, RecordKind};
use schemify_plugin::abi::Event;

use crate::commands::Cmd;
use crate::geometry::{Pt, Xform, snap};
use crate::session::View;
use crate::{edit, platform, plugins};

use super::Editor;
use super::chooser::ChooseFor;
use super::text::CHANGELOG_ROWS;
use super::tool::{Placement, Tool};

impl Editor {
    /// `file.open <arg>`: the cell `arg` names ([`crate::session::find_cell`]), else the
    /// file at `arg` relative to the project root; a failure goes to the status line.
    pub(super) fn open_named(&mut self, arg: &str) {
        let name = match schemify_plugin::abi::text::values(arg).next() {
            Some(Ok(v)) => v.into_owned(),
            _ => arg.trim().to_owned(),
        };
        assert!(!name.is_empty(), "the chooser handles no argument");
        let opened = match crate::session::find_cell(&self.session.project, &name) {
            Some(row) => self.session.open_cell(row),
            None => {
                let path = self.session.root.join(&name);
                self.session.open_path(&path)
            }
        };
        match opened {
            Ok(d) => {
                self.show_opened(d);
                self.session.say(format!("opened {}", self.session.name(d)));
            }
            Err(e) => self.session.say(format!("file.open {name}: {e}")),
        }
    }
    /// Shows opened document `d` in a tab in its default view, remembers it as recent and
    /// tells the plugins.
    pub(super) fn show_opened(&mut self, d: DocHandle) {
        let view = default_view(self.session.project.doc(d));
        self.session.open_tab(d, view);
        self.remember(d);
        let _ = self.host.push(
            None,
            &Event::DocOpened {
                doc: plugins::doc_id(d),
            },
        );
        debug_assert!(self.session.project.doc(d).is_some());
    }
    pub(super) fn switch_tab(&mut self, i: usize) {
        assert!(i < self.session.tabs().len());
        self.tool = Tool::Idle;
        self.session.switch_tab(i);
    }
    pub(super) fn descend(&mut self, h: DocHandle, cmd: Cmd) {
        let Some(key) = self.one_instance() else {
            self.session.say("select exactly one instance");
            return;
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let row = loc.row as usize;
        let s = doc.strings();
        let cellref = s.get(doc.instances().cell[row]).to_owned();
        let name = s.get(doc.instances().name[row]).to_owned();
        let cell = match self.session.project.resolve(h, &cellref) {
            Ok(r) => r,
            Err(c) => {
                self.session.say(format!("{cellref}: {c} {}", c.meaning()));
                return;
            }
        };
        let kind = self.session.project.cells().kind[cell];
        let symbol = matches!(cmd, Cmd::DescendSymbol | Cmd::OpenSymbolTab);
        if !symbol && kind != FileKind::Shcomp {
            self.session.say(format!(
                "{cellref} is a primitive: no schematic (use i for its symbol)"
            ));
            return;
        }
        let child = match self.session.open_cell(cell) {
            Ok(c) => c,
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
        self.tool = Tool::Idle;
        if matches!(cmd, Cmd::OpenSubTab | Cmd::OpenSymbolTab) {
            self.session.open_tab(child, view);
        } else if let Err(e) = self.session.push_level(child, view, &name) {
            self.session.say(e);
        }
    }
    pub(super) fn close_tab(&mut self) {
        let Some(tab) = self.session.tabs().get(self.session.active()) else {
            return;
        };
        let dirty = tab
            .stack
            .iter()
            .any(|l| self.session.project.is_dirty(l.doc));
        if dirty && self.confirm != Some(Cmd::Close) {
            self.confirm = Some(Cmd::Close);
            self.session
                .say("unsaved changes: Ctrl+S to save, or close again to discard");
            return;
        }
        self.confirm = None;
        if let Err(e) = self.session.close_active_tab() {
            self.session.say(e);
        }
        self.tool = Tool::Idle;
    }
    pub(super) fn reload(&mut self, h: DocHandle) {
        let Some(path) = self.session.path(h).map(Path::to_path_buf) else {
            return;
        };
        if self.session.project.is_dirty(h) {
            self.session
                .say("unsaved changes: save or undo them before reloading");
            return;
        }
        match platform::read(&path) {
            Ok(bytes) => match self.session.project.external_change(h, &bytes) {
                Ok(External::Unchanged) => self.session.say("file unchanged"),
                Ok(e) => {
                    self.session.say(format!("reloaded ({e:?})"));
                    self.session.bump(h);
                    self.session.flush_journal(h);
                }
                Err(c) => self.session.say(format!("reload: {c}")),
            },
            Err(e) => self.session.say(e),
        }
    }
    /// Remembers `h`'s file in the recent-files list (and persists it).
    pub(super) fn remember(&mut self, h: DocHandle) {
        let Some(p) = self.session.path(h) else {
            return;
        };
        if p.starts_with(crate::libs::BUILTIN_DIR) {
            return;
        }
        crate::recent::push(&mut self.recent, crate::libs::absolute(p));
        if let Some(dir) = &self.state_dir
            && let Err(e) = crate::recent::save(dir, &self.recent)
        {
            self.session.say(format!("recent files: {e}"));
        }
    }
    /// Opens recent file `i`: in a tab when it belongs to this project (or one of its local
    /// libraries), else by switching to its project (`switch_project`).
    pub(super) fn open_recent(&mut self, i: usize, reqs: &mut Vec<EditRequest>) {
        let Some(path) = self.recent.get(i).cloned() else {
            self.session.say("no recent file");
            return;
        };
        if !platform::exists(&path) {
            self.recent.remove(i);
            self.session
                .say(format!("{}: no longer exists", path.display()));
            return;
        }
        match self.session.open_path(&path) {
            Ok(d) => {
                let view = super::default_view(self.session.project.doc(d));
                self.session.open_tab(d, view);
                self.remember(d);
                self.tool = Tool::Idle;
            }
            Err(_) if super::project_root(&path) != crate::libs::absolute(&self.session.root) => {
                self.switch_project(path, reqs);
            }
            Err(e) => self.session.say(e),
        }
    }
    /// Opens `file` of another project: that project's session (manifest, libraries,
    /// plugins, journals) replaces this one, started with this session's startup options and
    /// keeping what belongs to the user rather than the project: the view options, layer
    /// colors, clipboard, netlist dialect, recent files, the UI and its state. Refused while a
    /// document of this project has unsaved edits or a simulation runs (both would be lost).
    /// Edit requests already queued this frame name this project's documents and are dropped.
    pub(super) fn switch_project(&mut self, file: PathBuf, reqs: &mut Vec<EditRequest>) {
        let p = &self.session.project;
        if p.handles().any(|d| p.is_dirty(d)) {
            self.session.say(format!(
                "{}: another project; save or undo this project's edits first",
                file.display()
            ));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.sim.wake_ms().is_some() {
                self.session
                    .say("a simulation is running: cancel it before switching project");
                return;
            }
        }
        let mut opts = self.startup.clone();
        opts.path = Some(file);
        let mut next = match Editor::new(opts) {
            Ok(e) => e,
            Err(e) => {
                self.session.say(e);
                return;
            }
        };
        // The UI keeps its string arena: commands routed this frame name its symbols.
        next.shell = std::mem::take(&mut self.shell);
        next.shell.invalidate();
        next.viewport = self.viewport;
        next.now = self.now;
        next.pointer = self.pointer;
        next.mods = self.mods;
        next.opts = self.opts;
        next.layer_colors = std::mem::take(&mut self.layer_colors);
        next.clipboard = std::mem::take(&mut self.clipboard);
        next.dialect = self.dialect;
        next.recent = std::mem::take(&mut self.recent);
        next.queued = std::mem::take(&mut self.queued);
        if let Some((h, _)) = next.active() {
            next.remember(h);
        }
        reqs.clear();
        let root = next.session.root.clone();
        *self = next;
        self.refresh_palette();
        self.session.say(format!("project {}", root.display()));
        assert!(reqs.is_empty() && self.active().is_some());
    }
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
            &schemify_plugin::abi::Event::DocOpened {
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
        let base = (snap(c.0, self.opts.snap), snap(c.1, self.opts.snap));
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
        let target = |f: &schemify_format::Document<'_>, r: u32| -> DocTarget {
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
                schemify_format::ChangeKind::Removed => ops.push(Op::Delete(target(&old, c.a))),
                schemify_format::ChangeKind::Added => ops.push(Op::Insert {
                    rec: RecordBuf::from_format(&new, c.b),
                    at: None,
                }),
                schemify_format::ChangeKind::Changed => ops.push(Op::Put {
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
    /// The schematic documents of the hierarchy below `h` (`h` first), each once, opening
    /// the cells it instantiates (bounded by `CHANGELOG_ROWS`).
    pub(super) fn hierarchy_docs(&mut self, h: DocHandle) -> Vec<DocHandle> {
        let mut out = vec![h];
        let mut i = 0;
        while i < out.len() && out.len() < CHANGELOG_ROWS {
            let d = out[i];
            i += 1;
            let refs: Vec<String> = self.session.project.doc(d).map_or_else(Vec::new, |doc| {
                let s = doc.strings();
                let mut c: Vec<String> = doc
                    .instances()
                    .cell
                    .iter()
                    .map(|&x| s.get(x).to_owned())
                    .collect();
                c.sort_unstable();
                c.dedup();
                c
            });
            for r in refs {
                let Ok(row) = self.session.project.resolve(d, &r) else {
                    continue;
                };
                if self.session.project.cells().kind[row] != FileKind::Shcomp {
                    continue;
                }
                if let Ok(child) = self.session.open_cell(row)
                    && !out.contains(&child)
                {
                    out.push(child);
                }
            }
        }
        out
    }
}

pub(super) fn default_view(doc: Option<&Document>) -> View {
    match doc.map(Document::kind) {
        Some(FileKind::Shprim) => View::Symbol,
        _ => View::Schematic,
    }
}

/// The project directory of a file: the nearest ancestor holding `schemify.project`, else
/// the file's directory.
pub(super) fn project_root(file: &Path) -> PathBuf {
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| file.to_path_buf(), |d| d.join(file))
    };
    let mut dir = abs.parent().map(Path::to_path_buf);
    // Bounded by the path depth.
    while let Some(d) = dir {
        if platform::exists(&d.join("schemify.project")) {
            return d;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    abs.parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}
