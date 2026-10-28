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
//!
//! Contents:
//! - `pub const TABS_MAX`
//! - `pub const DEPTH_MAX`
//! - `pub enum View`
//! - `pub struct Level`
//! - `pub struct Tab`
//! - `pub struct TextView`
//! - `pub const TEXT_WINDOW`
//! - `pub struct Session`, `Session::load`, `Session::reload`, `Session::lib_dir`,
//!   `Session::lib_dirs`, `Session::handle_of`, `Session::path`, `Session::name`,
//!   `Session::epoch`, `Session::bump`
//! - `pub fn find_cell`

mod files;
mod tabs;

use std::path::{Path, PathBuf};

use schemify_canvas::Camera;
use schemify_doc::{DocHandle, Project};

use crate::geometry::Pt;
pub use crate::libs::cell_relpath;
use crate::libs::{self, absolute};
use crate::scene::ObjKey;

pub use tabs::{STATUS_MAX, TabView};

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
    /// Partially selected wires (xschem's Ctrl+drag box): one row per selected end, (wire
    /// key, the end's position), sorted. A move moves just those ends and the rows follow
    /// them; a row whose position is no longer an end of its wire (after an undo) selects
    /// nothing. A wire also in `sel` moves whole.
    pub ends: Vec<(ObjKey, Pt)>,
}

/// One tab: a stack of levels; the last is shown (D28: undo stacks live in `doc`, per
/// document, so navigating never touches them).
#[derive(Clone, Debug)]
pub struct Tab {
    /// Levels, top first; never empty.
    pub stack: Vec<Level>,
    /// The docs page is shown beside the canvas (`view.docs`); starts on when the opened
    /// document has doc blocks.
    pub docs: bool,
    /// The docs page shows prose blocks as their Markdown / LaTeX source instead of rendered
    /// (`docs.source`); kept per tab, so it survives switching tabs.
    pub docs_source: bool,
    /// The cell settings panel is shown beside the canvas (`view.cell`).
    pub cell: bool,
    /// A read-only text view (a netlist, xschem's "show netlist") shown instead of the
    /// canvas; `stack` then holds the level it was made from, which editing commands skip.
    pub text: Option<TextView>,
}

/// A read-only text shown in a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextView {
    /// Tab title.
    pub title: String,
    /// The file the text was written to (a second view of the same file replaces it).
    pub path: PathBuf,
    /// The text.
    pub body: String,
    /// The last find (`text.find`), empty for none.
    pub find: String,
    /// Its result: how many lines of the whole file contain it, and those lines (numbered,
    /// at most as many as a text tab shows); `None` shows `body`.
    pub found: Option<(u32, String)>,
}

/// Lines of a text tab's [`TextView`] laid out at once, at least (two screens' worth when
/// that is more): a window around the tab's scroll offset (`ui::records::window`); spacers
/// stand in for the rest.
pub const TEXT_WINDOW: u32 = 256;

/// The project, its open documents and tabs.
#[derive(Debug)]
pub struct Session {
    /// Project root directory (manifest location).
    pub root: PathBuf,
    /// Design data (`doc` is the only writer; requests go through `doc::apply_edits`).
    pub project: Project,
    lib_dir: Vec<Option<PathBuf>>,
    docs: Docs,
    /// Tabs; never empty after [`Session::open_tab`]. Written only by `Session` methods.
    tabs: Vec<Tab>,
    /// Active tab index (`< tabs.len()` once a tab is open).
    active: usize,
    /// Status line message (last command result or error); written only through
    /// [`Session::say`], [`Session::note`] and [`Session::take_status`].
    status: String,
    untitled: u32,
    /// Documents whose journal offers unsaved edits from a crash, waiting for the user's
    /// decision (`decide_recovery`): (document, journal records offered).
    recovery: Vec<(DocHandle, u32)>,
    /// The file to open first as the load's index read it (`libs::Loaded::kept`), until
    /// [`Session::open_file`] takes it or an index makes it stale.
    kept: Option<(PathBuf, Vec<u8>)>,
}

/// Open documents and where they live on disk (SoA, one row per open document).
#[derive(Clone, Debug, Default)]
struct Docs {
    handle: Vec<DocHandle>,
    path: Vec<PathBuf>,
    journal: Vec<PathBuf>,
    epoch: Vec<u32>,
}

/// The indexed cell row `name` names: `lib:cell` or, for the project library, `cell`, where
/// `cell` is the cell's path in its library with or without its extension (`inv`,
/// `inv.shcomp`, `sub/amp`). `None` when no indexed cell matches. Cost: one pass over the
/// cell index.
#[must_use]
pub fn find_cell(p: &Project, name: &str) -> Option<usize> {
    let (lib, rest) = name.split_once(':').unwrap_or(("", name));
    let cells = p.cells();
    let row = (0..cells.lib.len()).find(|&r| {
        let rel = cell_relpath(p, r);
        let stem = rel.rsplit_once('.').map_or(rel.as_str(), |(s, _)| s);
        p.libs().name[usize::from(cells.lib[r])] == lib && (stem == rest || rel == rest)
    });
    debug_assert!(row.is_none_or(|r| r < cells.lib.len()));
    row
}

impl Session {
    /// Loads the project rooted at `root` (load spine): manifest + lockfile → library
    /// directories → `index_library` per local library (the project's own files, then
    /// `path` libraries). `git`/`url` libraries are reported, not fetched (M2). The bytes of
    /// cell file `open` (the file about to be opened) are kept from the index, so opening it
    /// reads and hashes it no second time.
    ///
    /// # Errors
    ///
    /// The manifest or lockfile does not parse.
    pub fn load(root: &Path, seed: u64, open: Option<&Path>) -> Result<Session, String> {
        let open = open.map(absolute);
        let loaded = libs::load(root, seed, open.as_deref())?;
        Ok(Session {
            root: root.to_path_buf(),
            project: loaded.project,
            lib_dir: loaded.lib_dir,
            docs: Docs::default(),
            tabs: Vec::new(),
            active: 0,
            status: loaded.notes.join("; "),
            untitled: 0,
            recovery: Vec::new(),
            kept: loaded.kept,
        })
    }

    /// `project.reload` (pdk spec §9.2): re-reads `schemify.project` and `schemify.lock` and
    /// re-indexes every library, keeping the open documents and tabs. Cell rows change:
    /// callers drop anything keyed by row. The status line takes the load notes.
    ///
    /// # Errors
    ///
    /// A file does not parse or an open document's library is gone; nothing changed then.
    ///
    /// # Panics
    ///
    /// Never: the assert checks one directory per library row.
    pub fn reload(&mut self) -> Result<(), String> {
        let (lib_dir, notes) = libs::reload(&mut self.project, &self.root)?;
        self.kept = None; // the new index may have read other bytes
        self.lib_dir = lib_dir;
        self.status = notes.join("; ");
        assert_eq!(self.lib_dir.len(), self.project.libs().name.len());
        Ok(())
    }

    /// Returns the directory of library row `lib`, if it is local.
    #[must_use]
    pub fn lib_dir(&self, lib: u8) -> Option<&Path> {
        self.lib_dir.get(usize::from(lib))?.as_deref()
    }

    /// The directory of every library row, the project root standing in for libraries
    /// that are not local (a runner's `lib_dirs`: sources are compiled from them).
    ///
    /// # Panics
    ///
    /// Never: the assert checks one directory per library row.
    #[must_use]
    pub fn lib_dirs(&self) -> Vec<PathBuf> {
        let dirs: Vec<PathBuf> = self
            .lib_dir
            .iter()
            .map(|d| d.clone().unwrap_or_else(|| self.root.clone()))
            .collect();
        assert_eq!(dirs.len(), self.lib_dir.len());
        dirs
    }
}

impl Session {
    fn row(&self, h: DocHandle) -> Option<usize> {
        self.docs.handle.iter().position(|&d| d == h)
    }

    /// The open document read from `path`, if any.
    #[must_use]
    pub fn handle_of(&self, path: &Path) -> Option<DocHandle> {
        let i = self.docs.path.iter().position(|p| p == path)?;
        let h = self.docs.handle[i];
        self.project.doc(h).map(|_| h)
    }

    /// File path of an open document; `None` once `h` is closed.
    #[must_use]
    pub fn path(&self, h: DocHandle) -> Option<&Path> {
        self.row(h).map(|i| self.docs.path[i].as_path())
    }

    /// Display name of an open document (file name).
    #[must_use]
    pub fn name(&self, h: DocHandle) -> String {
        self.path(h)
            .and_then(|p| p.file_name())
            .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
    }

    /// Edit epoch of an open document: changes on every commit, undo, redo or reload, so
    /// derived data (scene, symbol cache) knows when to rebuild.
    #[must_use]
    pub fn epoch(&self, h: DocHandle) -> u32 {
        self.row(h).map_or(0, |i| self.docs.epoch[i])
    }

    /// Advances the edit epoch of `h`, so the scene and the symbol cache rebuild from it on
    /// the next frame. A closed `h` is ignored.
    pub fn bump(&mut self, h: DocHandle) {
        if let Some(i) = self.row(h) {
            self.docs.epoch[i] = self.docs.epoch[i].wrapping_add(1);
        }
    }
}

impl crate::spines::Cells for Session {
    fn project(&self) -> &Project {
        &self.project
    }

    fn open_row(&mut self, row: u32) -> Result<(), String> {
        self.open_cell(row as usize).map(|_| ())
    }
}
