//! Files, tabs and hierarchy: open, save as, reload, recent files, project switch, merge, delete.
//!
//! Contents:
//! - `Editor::open_named`
//! - `Editor::show_opened`
//! - `Editor::switch_tab`
//! - `Editor::descend`
//! - `Editor::close_tab`
//! - `Editor::reload`
//! - `Editor::remember`
//! - `Editor::open_recent`
//! - `Editor::switch_project`
//! - `Editor::hierarchy_docs`
//! - `pub(super) fn default_view`
//! - `pub(super) fn project_root`

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Document, EditRequest, External};
use schemify_format::FileKind;
use schemify_plugin::abi::Event;

use crate::commands::Cmd;
use crate::session::View;
use crate::{edit, platform, plugins};

use super::Editor;
use super::text::CHANGELOG_ROWS;
use super::tool::Tool;

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
        assert!(
            matches!(
                cmd,
                Cmd::Descend | Cmd::DescendSymbol | Cmd::OpenSubTab | Cmd::OpenSymbolTab
            ),
            "{cmd:?} is no descent"
        );
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
        let closed = self.session.close_active_tab();
        assert!(
            closed.is_err() || !self.session.tabs().is_empty(),
            "the last tab is replaced"
        );
        if let Err(e) = closed {
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
                    debug_assert!(
                        !self.session.project.is_dirty(h),
                        "a clean document reloads clean"
                    );
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
        assert!(self.recent.len() <= crate::recent::RECENT_MAX);
        if let Some(dir) = &self.state_dir
            && let Err(e) = crate::recent::save(dir, &self.recent)
        {
            self.session.say(format!("recent files: {e}"));
        }
    }
    /// Opens recent file `i`: in a tab when it belongs to this project (or one of its local
    /// libraries), else by switching to its project (`switch_project`).
    pub(super) fn open_recent(&mut self, i: usize, reqs: &mut Vec<EditRequest>) {
        assert!(
            self.recent.len() <= crate::recent::RECENT_MAX,
            "the recent list keeps its cap"
        );
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
                let view = default_view(self.session.project.doc(d));
                self.session.open_tab(d, view);
                self.remember(d);
                self.tool = Tool::Idle;
            }
            Err(_) if project_root(&path) != crate::libs::absolute(&self.session.root) => {
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
        assert!(reqs.is_empty(), "the old project's requests are dropped");
        assert!(self.active().is_some(), "the new project shows a document");
    }
}

impl Editor {
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
        assert_eq!(out[0], h, "the top document first");
        debug_assert!(
            out.iter().enumerate().all(|(k, d)| !out[..k].contains(d)),
            "each once"
        );
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
            assert!(abs.starts_with(&d), "an ancestor of the file");
            return d;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    let root = abs
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    assert!(abs.starts_with(&root) || root == Path::new("."));
    root
}
