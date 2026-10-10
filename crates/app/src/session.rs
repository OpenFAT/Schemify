//! `session`: the project on disk and what the editor has open — library directories, open
//! documents with their file and journal paths, tabs with their hierarchy stacks, and the
//! status line. Owner of every file read and write the editor does (load spine, saves,
//! journals), through [`crate::platform`].
//!
//! Data design:
//! 1. In: a project directory or one cell file; edit outcomes. Out: `doc::Project` loaded and
//!    indexed; journal and file writes.
//! 2. How many: ≤ [`TABS_MAX`] tabs, each ≤ [`DEPTH_MAX`] hierarchy levels (xschem's
//!    `CADMAXHIER`); open documents ≤ `doc::DOCS_MAX`.
//! 3. Widths: doc handles are `doc::DocHandle`; per-document edit epochs `u32` (wrapping).
//! 4. Access: per frame the active level only; per edit one row of `docs`.
//! 5. Lifetime: session. 6. Parallel: no (one writer, I/O bound).

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, LibSource, OpenError, Project, Recovery};
use schemify_format::FileKind;
use schemify_render::canvas::Camera;

use crate::platform;
use crate::scene::ObjKey;

/// Open tabs at once.
pub const TABS_MAX: usize = 64;
/// Hierarchy depth per tab (xschem `CADMAXHIER`).
pub const DEPTH_MAX: usize = 40;

/// Which side of a cell a level shows: its schematic (rank 3) or its symbol (ranks 1–2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Instances, wires, labels, ports, texts.
    Schematic,
    /// Pins and symbol graphics.
    Symbol,
}

/// One hierarchy level of a tab.
#[derive(Clone, Debug)]
pub struct Level {
    /// The document shown.
    pub doc: DocHandle,
    /// Schematic or symbol side.
    pub view: View,
    /// Instance name descended through (empty at the top).
    pub inst: String,
    /// Camera; `None` until first fitted.
    pub cam: Option<Camera>,
    /// Selection, kept per level so returning from a child restores it.
    pub sel: Vec<ObjKey>,
}

/// One tab: a stack of levels; the last is shown (D28: undo stacks live in `doc`, per
/// document, so navigating never touches them).
#[derive(Clone, Debug)]
pub struct Tab {
    /// Levels, top first; never empty.
    pub stack: Vec<Level>,
}

/// Open documents and where they live on disk (SoA, one row per open document).
#[derive(Clone, Debug, Default)]
struct Docs {
    handle: Vec<DocHandle>,
    path: Vec<PathBuf>,
    journal: Vec<PathBuf>,
    epoch: Vec<u32>,
}

/// The project, its open documents and tabs.
#[derive(Debug)]
pub struct Session {
    /// Project root directory (manifest location).
    pub root: PathBuf,
    /// Design data (`doc` is the only writer; requests go through `doc::apply_edits`).
    pub project: Project,
    lib_dir: Vec<Option<PathBuf>>,
    docs: Docs,
    /// Tabs; never empty after [`Session::open_tab`].
    pub tabs: Vec<Tab>,
    /// Active tab index.
    pub active: usize,
    /// Status line message (last command result or error).
    pub status: String,
    untitled: u32,
}

impl Session {
    /// Loads the project rooted at `root` (load spine): manifest + lockfile → library
    /// directories → `index_library` per local library (the project's own files, then
    /// `path` libraries). `git`/`url` libraries are reported, not fetched (M2).
    ///
    /// # Errors
    ///
    /// The manifest or lockfile does not parse.
    pub fn load(root: &Path, seed: u64) -> Result<Session, String> {
        let manifest = platform::read(&root.join("schemify.project")).ok();
        let lock = platform::read(&root.join("schemify.lock")).ok();
        let project = Project::new(manifest.as_deref(), lock.as_deref(), seed)
            .map_err(|e| format!("schemify.project: {:?}", e.diagnostics.first()))?;
        let libs = project.libs();
        let mut lib_dir = Vec::with_capacity(libs.name.len());
        let mut notes = Vec::new();
        for row in 0..libs.name.len() {
            let dir = match &libs.source[row] {
                LibSource::Project => Some(root.to_path_buf()),
                LibSource::Path { path } => {
                    let mut d = root.join(path);
                    if let Some(sub) = &libs.subdir[row] {
                        d = d.join(sub);
                    }
                    Some(d)
                }
                LibSource::Builtin => None,
                other => {
                    notes.push(format!(
                        "lib {}: {} sources are not fetched yet",
                        libs.name[row],
                        other.word()
                    ));
                    None
                }
            };
            lib_dir.push(dir);
        }
        let mut s = Session {
            root: root.to_path_buf(),
            project,
            lib_dir,
            docs: Docs::default(),
            tabs: Vec::new(),
            active: 0,
            status: String::new(),
            untitled: 0,
        };
        let skip: Vec<PathBuf> = s.lib_dir.iter().skip(1).flatten().cloned().collect();
        for row in 0..s.lib_dir.len() {
            let Some(dir) = s.lib_dir[row].clone() else {
                continue;
            };
            let others: Vec<PathBuf> = skip.iter().filter(|d| **d != dir).cloned().collect();
            if let Err(e) = s.index(row, &dir, &others) {
                notes.push(e);
            }
        }
        s.status = notes.join("; ");
        assert_eq!(s.lib_dir.len(), s.project.libs().name.len());
        Ok(s)
    }

    /// Indexes library `row` from the cell files under `dir`.
    fn index(&mut self, row: usize, dir: &Path, skip: &[PathBuf]) -> Result<(), String> {
        let name = self.project.libs().name[row].clone();
        let mut rel = Vec::new();
        let mut bytes = Vec::new();
        for p in platform::list_files(dir, skip) {
            let Some(r) = rel_path(dir, &p) else { continue };
            if !is_cell_file(&r) {
                continue;
            }
            bytes.push(platform::read(&p)?);
            rel.push(r);
        }
        let files: Vec<(&str, &[u8])> = rel
            .iter()
            .map(String::as_str)
            .zip(bytes.iter().map(Vec::as_slice))
            .collect();
        let resolved = dir.display().to_string();
        let (_, diags) = self
            .project
            .index_library(&name, &resolved, &files)
            .map_err(|c| format!("lib {name}: {c}"))?;
        if let Some((path, d)) = diags.first() {
            return Err(format!("{name}/{path}: {} {}", d.code, d.detail));
        }
        Ok(())
    }

    /// Returns the directory of library row `lib`, if it is local.
    pub fn lib_dir(&self, lib: u8) -> Option<&Path> {
        self.lib_dir.get(usize::from(lib))?.as_deref()
    }

    /// Opens (or returns the already open) cell file `relpath` of library row `lib`, with its
    /// journal sidecar. A crash-recovery offer is accepted (replayed) right away and noted in
    /// the status line — never losing the user's edits is the safe default.
    // ponytail: no recovery prompt yet; add a dialog (recover / discard) when one exists.
    ///
    /// # Errors
    ///
    /// The file cannot be read or does not parse.
    pub fn open_file(&mut self, lib: u8, relpath: &str) -> Result<DocHandle, String> {
        let dir = self
            .lib_dir(lib)
            .ok_or_else(|| format!("library row {lib} is not local"))?
            .to_path_buf();
        let path = dir.join(relpath);
        if let Some(h) = self.handle_of(&path) {
            return Ok(h);
        }
        let bytes = platform::read(&path)?;
        let journal_path = self.journal_path(&path);
        let journal = platform::read(&journal_path).ok();
        let name = self.project.libs().name[usize::from(lib)].clone();
        let opened = match self
            .project
            .open(&name, relpath, &bytes, journal.as_deref())
        {
            Ok(o) => o,
            Err(OpenError::AlreadyOpen(h)) => return Ok(h),
            Err(OpenError::Parse(e)) => {
                let d = e.diagnostics.first();
                return Err(format!(
                    "{relpath}:{}: {:?}",
                    d.map_or(0, |d| d.line),
                    d.map(|d| d.code)
                ));
            }
            Err(OpenError::Code(c)) => return Err(format!("{relpath}: {c} {}", c.meaning())),
        };
        let h = opened.handle;
        if opened.recovery != Recovery::None {
            match self.project.recover(h) {
                Ok((applied, dropped)) => {
                    self.status = format!(
                        "{relpath}: recovered {applied} unsaved edit(s) from the journal ({dropped} dropped)"
                    );
                }
                Err(c) => self.status = format!("{relpath}: recovery failed ({c})"),
            }
        }
        self.docs.handle.push(h);
        self.docs.path.push(path);
        self.docs.journal.push(journal_path);
        self.docs.epoch.push(0);
        self.flush_journal(h);
        assert!(self.project.doc(h).is_some());
        Ok(h)
    }

    /// Opens indexed cell `row` (see [`Session::open_file`]).
    ///
    /// # Errors
    ///
    /// As [`Session::open_file`].
    pub fn open_cell(&mut self, row: usize) -> Result<DocHandle, String> {
        let cells = self.project.cells();
        assert!(row < cells.lib.len(), "cell row from a previous index");
        let lib = cells.lib[row];
        let rel = cell_relpath(&self.project, row);
        self.open_file(lib, &rel)
    }

    /// Opens a file given by path (absolute or relative to the working directory): the
    /// library whose directory contains it, or the project.
    ///
    /// # Errors
    ///
    /// The file is outside every local library, unreadable, or does not parse.
    pub fn open_path(&mut self, path: &Path) -> Result<DocHandle, String> {
        let abs = absolute(path);
        // Longest matching library directory wins (libraries may live under the root).
        let mut best: Option<(u8, String, usize)> = None;
        for (row, dir) in self.lib_dir.iter().enumerate() {
            let Some(dir) = dir else { continue };
            let dir = absolute(dir);
            if let Some(rel) = rel_path(&dir, &abs) {
                let depth = dir.components().count();
                if best.as_ref().is_none_or(|b| depth > b.2) {
                    best = Some((row as u8, rel, depth));
                }
            }
        }
        let (lib, rel, _) =
            best.ok_or_else(|| format!("{}: outside the project", path.display()))?;
        self.open_file(lib, &rel)
    }

    /// Creates a new, unsaved cell document in the project directory.
    ///
    /// # Errors
    ///
    /// `doc` refuses (too many documents).
    pub fn create(&mut self, kind: FileKind) -> Result<DocHandle, String> {
        assert!(matches!(kind, FileKind::Shcomp | FileKind::Shprim));
        let ext = kind.word();
        let rel = loop {
            self.untitled += 1;
            let rel = format!("untitled{}.{ext}", self.untitled);
            if !platform::exists(&self.root.join(&rel)) {
                break rel;
            }
        };
        let h = self
            .project
            .create("", &rel, kind)
            .map_err(|c| format!("{rel}: {c}"))?;
        let path = self.root.join(&rel);
        self.docs.handle.push(h);
        self.docs.journal.push(self.journal_path(&path));
        self.docs.path.push(path);
        self.docs.epoch.push(0);
        Ok(h)
    }

    fn row(&self, h: DocHandle) -> Option<usize> {
        self.docs.handle.iter().position(|&d| d == h)
    }

    fn handle_of(&self, path: &Path) -> Option<DocHandle> {
        let i = self.docs.path.iter().position(|p| p == path)?;
        let h = self.docs.handle[i];
        self.project.doc(h).map(|_| h)
    }

    /// File path of an open document.
    pub fn path(&self, h: DocHandle) -> Option<&Path> {
        self.row(h).map(|i| self.docs.path[i].as_path())
    }

    /// Display name of an open document (file name).
    pub fn name(&self, h: DocHandle) -> String {
        self.path(h)
            .and_then(|p| p.file_name())
            .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
    }

    /// Edit epoch of an open document: changes on every commit, undo, redo or reload, so
    /// derived data (scene, symbol cache) knows when to rebuild.
    pub fn epoch(&self, h: DocHandle) -> u32 {
        self.row(h).map_or(0, |i| self.docs.epoch[i])
    }

    /// Advances the edit epoch of `h`.
    pub fn bump(&mut self, h: DocHandle) {
        if let Some(i) = self.row(h) {
            self.docs.epoch[i] = self.docs.epoch[i].wrapping_add(1);
        }
    }

    /// `.schemify/journal/<project-relative cell path>.journal` (edits spec §3).
    fn journal_path(&self, file: &Path) -> PathBuf {
        let rel = rel_path(&absolute(&self.root), &absolute(file)).unwrap_or_else(|| {
            file.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        });
        self.root
            .join(".schemify")
            .join("journal")
            .join(format!("{rel}.journal"))
    }

    /// Writes the journal bytes `doc` produced for `h` since the last call.
    pub fn flush_journal(&mut self, h: DocHandle) {
        let Some(w) = self.project.take_journal(h) else {
            return;
        };
        let Some(i) = self.row(h) else { return };
        let path = &self.docs.journal[i];
        let r = if w.rewrite {
            platform::write_atomic(path, w.text.as_bytes())
        } else {
            platform::append(path, w.text.as_bytes())
        };
        if let Err(e) = r {
            self.status = format!("journal: {e}");
        }
    }

    /// Save spine: `doc::save` → atomic write → `doc::saved` → journal. Returns whether
    /// anything was written (a clean document is not rewritten).
    ///
    /// # Errors
    ///
    /// `doc` refuses (read-only, conflict, kind rules) or the write fails.
    pub fn save(&mut self, h: DocHandle) -> Result<bool, String> {
        let req = self
            .project
            .save(h)
            .map_err(|c| format!("save refused: {c} {}", c.meaning()))?;
        let Some(req) = req else { return Ok(false) };
        let path = self.path(h).ok_or("not a file document")?.to_path_buf();
        platform::write_atomic(&path, req.bytes.as_bytes())?;
        self.project
            .saved(h, req.bytes)
            .map_err(|c| format!("save: {c}"))?;
        self.flush_journal(h);
        assert!(!self.project.is_dirty(h));
        Ok(true)
    }

    /// Opens a new tab showing `doc` (or switches to the tab already showing it at top).
    pub fn open_tab(&mut self, doc: DocHandle, view: View) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.stack[0].doc == doc && t.stack[0].view == view)
        {
            self.active = i;
            return;
        }
        if self.tabs.len() >= TABS_MAX {
            self.status = format!("at most {TABS_MAX} tabs");
            return;
        }
        self.tabs.push(Tab {
            stack: vec![Level {
                doc,
                view,
                inst: String::new(),
                cam: None,
                sel: Vec::new(),
            }],
        });
        self.active = self.tabs.len() - 1;
        assert!(!self.tabs[self.active].stack.is_empty());
    }

    /// The level shown in the active tab.
    pub fn level(&self) -> Option<&Level> {
        self.tabs.get(self.active).and_then(|t| t.stack.last())
    }

    /// The level shown in the active tab, mutably.
    pub fn level_mut(&mut self) -> Option<&mut Level> {
        self.tabs
            .get_mut(self.active)
            .and_then(|t| t.stack.last_mut())
    }

    /// Hierarchy path of the active tab, xschem style (`.x1.x2.`).
    pub fn hier_path(&self) -> String {
        let mut s = String::from(".");
        if let Some(t) = self.tabs.get(self.active) {
            for l in &t.stack[1..] {
                s.push_str(&l.inst);
                s.push('.');
            }
        }
        s
    }

    /// Pushes a level onto the active tab (descend).
    ///
    /// # Errors
    ///
    /// The tab is already [`DEPTH_MAX`] deep.
    pub fn push_level(&mut self, doc: DocHandle, view: View, inst: &str) -> Result<(), String> {
        let tab = self.tabs.get_mut(self.active).ok_or("no tab")?;
        if tab.stack.len() >= DEPTH_MAX {
            return Err(format!("hierarchy deeper than {DEPTH_MAX}"));
        }
        tab.stack.push(Level {
            doc,
            view,
            inst: inst.to_owned(),
            cam: None,
            sel: Vec::new(),
        });
        Ok(())
    }

    /// Pops the active tab's level (return to parent); false at the top.
    pub fn pop_level(&mut self) -> bool {
        match self.tabs.get_mut(self.active) {
            Some(t) if t.stack.len() > 1 => {
                t.stack.pop();
                true
            }
            _ => false,
        }
    }
}

/// `relpath` of indexed cell `row` inside its library (`dir/stem.ext`).
pub fn cell_relpath(project: &Project, row: usize) -> String {
    let c = project.cells();
    let s = project.strings();
    let (dir, stem) = (s.get(c.dir[row]), s.get(c.stem[row]));
    let ext = c.kind[row].word();
    if dir.is_empty() {
        format!("{stem}.{ext}")
    } else {
        format!("{dir}/{stem}.{ext}")
    }
}

fn is_cell_file(rel: &str) -> bool {
    [".shprim", ".shcomp", ".shbench"]
        .iter()
        .any(|e| rel.ends_with(e))
}

/// `path` relative to `dir` with `/` separators, if inside it.
fn rel_path(dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |d| d.join(p))
    }
}
