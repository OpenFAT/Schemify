//! Manifest and lockfile bytes plus per-library listings → the project's library index
//! ([`Cells`], with each cell's symbol) and cell-reference resolution, including the
//! derived `tech` cells of a technology map. Holds the open documents (see `open.rs`).
//! Cites: D10, D35 (libraries, lockfile); netlist spec §1 (resolution); pdk spec §5, §9.2
//! (technology classes, `project.reload`).
//!
//! The crate does no I/O: the app lists each library's files and hands them over.
//!
//! Contents:
//! - `pub struct Project`: `new`, `reload_manifest`, `libs`, `plugins`, `settings`, `lock`,
//!   `cells`, `strings`, `index_library`, `techmap`, `resolve_tech`, `explain_tech`,
//!   `tech_source`, `lock_library`, `write_lock`, `resolve`, `resolve_in`, `open_handle`,
//!   `interface`; crate: `resolve_at`, `find_open`
//! - `pub enum ReloadError`

use crate::document::Document;
use crate::library::{
    self, CellRef, Cells, LibSource, Libs, LockStatus, Locked, Manifest, ManifestRest, TECH_LIB,
};
use crate::open::{DocHandle, Slot};
use crate::record::RecordBuf;
use crate::sha256;
use crate::strings::{Strings, Sym};
use crate::techmap::{self, TechMap};
use crate::{Code, Diag};
use schemify_format::{FileKind, IdRng, ParseError};

/// Libraries, their index and interfaces, and the open documents.
#[derive(Debug)]
pub struct Project {
    pub(crate) strings: Strings,
    pub(crate) libs: Libs,
    pub(crate) manifest: ManifestRest,
    pub(crate) lock: Locked,
    pub(crate) cells: Cells,
    pub(crate) slots: Vec<Slot>,
    pub(crate) rng: IdRng,
    /// The indexed `tech.map` (`lib tech map`), or why it did not load.
    pub(crate) techmap: Option<Result<TechMap, String>>,
    /// The map rows that did not become a derived cell.
    pub(crate) tech_problems: Vec<TechProblem>,
}

/// Why one technology-map row did not become a derived cell.
#[derive(Debug)]
pub(crate) struct TechProblem {
    pub(crate) class: String,
    pub(crate) flavor: String,
    /// What the checks panel shows.
    pub(crate) why: String,
}

/// Why [`Project::reload_manifest`] refused.
#[derive(Debug)]
pub enum ReloadError {
    /// The manifest or lockfile does not parse.
    Parse(ParseError),
    /// An open document belongs to a library the new manifest no longer declares (close it
    /// first; its file would otherwise have no library to save into).
    LibraryInUse(String),
}

impl Project {
    /// Creates a project from the manifest and lockfile bytes (either may be absent:
    /// a bare directory is a project with only the builtin `core` library). `seed`
    /// seeds id generation (OS entropy in the editor, a constant in tests).
    ///
    /// # Errors
    ///
    /// Format diagnostics when a file does not parse or is the wrong kind (`E028`).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: every project has the project row and `core`.
    pub fn new(
        manifest: Option<&[u8]>,
        lock: Option<&[u8]>,
        seed: u64,
    ) -> Result<Project, ParseError> {
        let Manifest { libs, rest } = library::parse_manifest(manifest)?;
        let lock = library::parse_lock(lock)?;
        assert_eq!(libs.find(""), Some(0), "row 0 is the project");
        assert!(libs.find("core").is_some(), "`core` is declared or builtin");
        Ok(Project {
            strings: Strings::default(),
            libs,
            manifest: rest,
            lock,
            cells: Cells::default(),
            slots: Vec::new(),
            rng: IdRng::new(seed),
            techmap: None,
            tech_problems: Vec::new(),
        })
    }

    /// Replaces the manifest and lockfile (`project.reload`, pdk spec §9.2), keeping the
    /// open documents, their undo history and their journals. The library index is
    /// emptied: the caller indexes every library again (the load spine), and must drop
    /// anything keyed by cell row.
    ///
    /// # Errors
    ///
    /// [`ReloadError::Parse`] when a file does not parse; [`ReloadError::LibraryInUse`]
    /// when an open document's library is gone. The project is unchanged on error.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a reloaded project has the project row and `core` and
    /// an empty index.
    pub fn reload_manifest(
        &mut self,
        manifest: Option<&[u8]>,
        lock: Option<&[u8]>,
    ) -> Result<(), ReloadError> {
        let Manifest { libs, rest } =
            library::parse_manifest(manifest).map_err(ReloadError::Parse)?;
        let lock = library::parse_lock(lock).map_err(ReloadError::Parse)?;
        // Open documents keep their library by name; row indices may move.
        let mut remap = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            let Some(open) = &slot.open else {
                remap.push(None);
                continue;
            };
            let name = &self.libs.name[open.lib as usize];
            let row = libs
                .find(name)
                .ok_or_else(|| ReloadError::LibraryInUse(name.clone()))?;
            remap.push(Some(row as u8));
        }
        for (slot, row) in self.slots.iter_mut().zip(remap) {
            if let (Some(open), Some(row)) = (slot.open.as_mut(), row) {
                open.lib = row;
            }
        }
        self.libs = libs;
        self.manifest = rest;
        self.lock = lock;
        self.cells = Cells::default();
        self.techmap = None;
        self.tech_problems.clear();
        assert_eq!(self.libs.find(""), Some(0), "row 0 is the project");
        assert!(
            self.libs.find("core").is_some(),
            "`core` is declared or builtin"
        );
        assert!(self.cells.lib.is_empty());
        Ok(())
    }

    /// Library rows: row 0 is the project itself (`""`), then the manifest's libraries
    /// with the builtin `core` (unless the manifest declares `core`).
    #[must_use]
    pub fn libs(&self) -> &Libs {
        &self.libs
    }

    /// Manifest `plugin` records, for the plugin host.
    #[must_use]
    pub fn plugins(&self) -> &[RecordBuf] {
        &self.manifest.plugins
    }

    /// Manifest `set <key> <value>` entries, in file order (`${NAME}` expansion source).
    #[must_use]
    pub fn settings(&self) -> &[(String, String)] {
        &self.manifest.sets
    }

    /// Lockfile rows.
    #[must_use]
    pub fn lock(&self) -> &Locked {
        &self.lock
    }

    /// The library index.
    #[must_use]
    pub fn cells(&self) -> &Cells {
        &self.cells
    }

    /// The arena of `cells.dir` / `cells.stem`.
    #[must_use]
    pub fn strings(&self) -> &Strings {
        &self.strings
    }

    /// Indexes library `name` (`""` for the project's own files) from a listing of
    /// `(relpath, bytes)` the app read; `resolved` is what the source resolved to (the
    /// commit of a git rev, the url, the path). Replaces the library's previous rows,
    /// parses every cell's symbol (`parse_prefix`, ranks 0–2), and computes the
    /// library content hash (sha256 over the files sorted by path, each `path`, `0`,
    /// sha256 of its bytes).
    ///
    /// Invalidates cell row indices. Cost: one sha256 pass over all bytes plus the
    /// interface prefixes.
    ///
    /// # Errors
    ///
    /// `E001` when `name` is not a library of the manifest.
    ///
    /// # Panics
    ///
    /// Panics if `files` names one path twice, or past [`CELLS_MAX`](crate::CELLS_MAX)
    /// cells in the project.
    pub fn index_library(
        &mut self,
        name: &str,
        resolved: &str,
        files: &[(&str, &[u8])],
    ) -> Result<(LockStatus, Vec<(String, Diag)>), Code> {
        let lib = self.libs.find(name).ok_or(Code::E001)?;
        let digests: Vec<sha256::Digest> = files.iter().map(|f| sha256::sha256(f.1)).collect();
        let diags = library::index_cells(
            &mut self.cells,
            &mut self.strings,
            lib as u8,
            files,
            &digests,
        );
        let hash = library::content_hash(files, &digests);
        self.libs.hash[lib] = Some(hash);
        self.libs.resolved[lib] = Some(resolved.to_owned());
        if matches!(self.libs.source[lib], LibSource::Map { .. }) {
            self.techmap = Some(match files {
                [(_, bytes)] => TechMap::parse(bytes),
                _ => Err(format!("{} files listed for one tech map", files.len())),
            });
        }
        self.derive_tech();
        let status = match self.libs.source[lib] {
            LibSource::Project | LibSource::Builtin => LockStatus::Unlocked,
            _ => library::lock_status(&self.lock, name, &hash),
        };
        assert!(self.libs.hash[lib].is_some());
        Ok((status, diags))
    }

    /// The indexed technology map, `None` before `lib tech` is indexed or when it failed to
    /// parse (see [`Project::explain_tech`]).
    #[must_use]
    pub fn techmap(&self) -> Option<&TechMap> {
        self.techmap.as_ref().and_then(|m| m.as_ref().ok())
    }

    /// Resolves technology class `class` of flavor `flavor` (pdk spec §5) to its derived
    /// cell row. `tech:<class>` in a file means flavor `core`; elaboration passes an
    /// instance's `flavor` override here.
    ///
    /// # Errors
    ///
    /// `E001` when no technology is selected, the map lacks the row, or the row's PDK cell
    /// did not resolve; [`Project::explain_tech`] says which.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a found row is a row of the `tech` library.
    pub fn resolve_tech(&self, class: &str, flavor: &str) -> Result<usize, Code> {
        let tech = self.libs.find(TECH_LIB).ok_or(Code::E001)?;
        let dir = self.strings.find(flavor).ok_or(Code::E001)?;
        let stem = self.strings.find(class).ok_or(Code::E001)?;
        let row = self
            .cells
            .find(tech as u8, dir, stem, FileKind::Shprim)
            .ok_or(Code::E001)?;
        assert_eq!(self.cells.lib[row] as usize, tech);
        Ok(row)
    }

    /// Why `tech:<class>` of `flavor` does not resolve, for the checks panel (E001 detail):
    /// no technology selected, the map not loaded, not a class, a class/flavor the PDK
    /// lacks (`sky130 has no lvt pmos`), or the derivation problem.
    #[must_use]
    pub fn explain_tech(&self, class: &str, flavor: &str) -> String {
        if self.libs.find(TECH_LIB).is_none() {
            return "no technology selected: add `lib tech map <tech.map>` to schemify.project \
                    (pdk.use writes it)"
                .to_owned();
        }
        let map = match &self.techmap {
            None => return "the technology map is not loaded".to_owned(),
            Some(Err(e)) => return format!("the technology map does not load: {e}"),
            Some(Ok(m)) => m,
        };
        if techmap::core_symbol(class).is_none() {
            let names: Vec<&str> = techmap::CLASSES.iter().map(|c| c.class).collect();
            return format!("`{class}` is not a technology class ({})", names.join(", "));
        }
        if map.find(class, flavor).is_none() {
            return format!("{} has no {flavor} {class}", map.pdk);
        }
        let why = self
            .tech_problems
            .iter()
            .find(|p| p.class == class && p.flavor == flavor)
            .map(|p| p.why.clone());
        why.unwrap_or_else(|| format!("tech:{class} ({flavor}) resolves"))
    }

    /// The PDK cell row derived `tech` cell `row` was built from (pdk spec §5), `None` for a
    /// row outside the `tech` library. A derived cell has no file of its own, so what its
    /// impls name relative to their cell (a Verilog-A `file`) resolves against this row's
    /// library and directory.
    ///
    /// # Panics
    ///
    /// Panics if `row` is not a row of the current index.
    #[must_use]
    pub fn tech_source(&self, row: usize) -> Option<usize> {
        assert!(row < self.cells.lib.len(), "cell row from a previous index");
        let tech = self.libs.find(TECH_LIB)?;
        if self.cells.lib[row] as usize != tech {
            return None;
        }
        let map = self.techmap()?;
        let (flavor, class) = (
            self.strings.get(self.cells.dir[row]),
            self.strings.get(self.cells.stem[row]),
        );
        let pdk = self
            .resolve_at(0, Sym::NONE, &map.cell[map.find(class, flavor)?])
            .ok()?;
        assert!(self.cells.lib[pdk] as usize != tech);
        Some(pdk)
    }

    /// Pins library `name` in the lockfile at its last indexed content.
    ///
    /// # Errors
    ///
    /// `E001` unknown or never indexed library; `E043` for the project row and the
    /// builtin `core` (bundled with the tool, never locked).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: the library has a lockfile row afterwards.
    pub fn lock_library(&mut self, name: &str) -> Result<(), Code> {
        let lib = self.libs.find(name).ok_or(Code::E001)?;
        if matches!(
            self.libs.source[lib],
            LibSource::Project | LibSource::Builtin
        ) {
            return Err(Code::E043);
        }
        let hash = self.libs.hash[lib].ok_or(Code::E001)?;
        let resolved = self.libs.resolved[lib].clone().unwrap_or_default();
        let kind = self.libs.source[lib].word();
        self.lock
            .set("lib", name, kind, &resolved, &sha256::hex(&hash));
        assert!(self.lock.find("lib", name).is_some());
        Ok(())
    }

    /// Returns the lockfile bytes (canonical; sorted by type, then name).
    ///
    /// # Panics
    ///
    /// As [`Locked::write`].
    #[must_use]
    pub fn write_lock(&self) -> String {
        self.lock.write()
    }

    /// Resolves a cell reference written in document `from` (netlist spec §1):
    /// `lib:cell` through the manifest, `./cell` in `from`'s directory; `.shcomp` is
    /// tried before `.shprim`; a `.shbench` is never instantiable. Returns the cell row.
    ///
    /// # Errors
    ///
    /// `E001` missing library, unindexed library, missing cell, or only a bench;
    /// `E040` stale handle.
    pub fn resolve(&self, from: DocHandle, cellref: &str) -> Result<usize, Code> {
        let open = self.open_ref(from).ok_or(Code::E040)?;
        self.resolve_at(open.lib, open.dir, cellref)
    }

    /// Resolves a cell reference written in the file of indexed cell `row`, by the
    /// rules of [`Project::resolve`] (netlist spec §1), whether or not that cell is open.
    ///
    /// # Errors
    ///
    /// `E001` missing library, unindexed library, missing cell, or only a bench.
    ///
    /// # Panics
    ///
    /// Panics if `row` is not a row of the current index (`index_library` invalidates
    /// rows).
    pub fn resolve_in(&self, row: usize, cellref: &str) -> Result<usize, Code> {
        assert!(row < self.cells.lib.len(), "cell row from a previous index");
        let found = self.resolve_at(self.cells.lib[row], self.cells.dir[row], cellref);
        if let Ok(r) = found {
            assert!(r < self.cells.lib.len());
            assert!(
                self.cells.kind[r] != FileKind::Shbench,
                "a bench is never instantiable"
            );
        }
        found
    }

    /// Returns the handle of the open document of indexed cell `row`, or `None` when
    /// only its indexed symbol is loaded (the "is this row open" query). Cost: a scan
    /// of the open documents.
    ///
    /// # Panics
    ///
    /// Panics if `row` is not a row of the current index.
    #[must_use]
    pub fn open_handle(&self, row: usize) -> Option<DocHandle> {
        assert!(row < self.cells.lib.len(), "cell row from a previous index");
        let c = &self.cells;
        self.find_open(c.lib[row], c.dir[row], c.stem[row], c.kind[row])
    }

    /// Returns the symbol of indexed cell `row` (interface and graphics, ranks 0–2):
    /// the open document when the cell is open (its edits are the current symbol),
    /// else the indexed one.
    ///
    /// # Panics
    ///
    /// Panics if `row` is not a row of the current index.
    #[must_use]
    pub fn interface(&self, row: usize) -> Option<&Document> {
        let open = self.open_handle(row).and_then(|h| self.doc(h));
        open.or(self.cells.iface[row].as_ref())
    }

    pub(crate) fn resolve_at(&self, lib: u8, dir: Sym, cellref: &str) -> Result<usize, Code> {
        assert!(
            (lib as usize) < self.libs.name.len(),
            "library row from the current manifest"
        );
        let (lib, dir, cell) = match CellRef::parse(cellref).ok_or(Code::E001)? {
            CellRef::Local { cell } => (lib, dir, cell),
            CellRef::Lib {
                lib: TECH_LIB,
                cell,
            } => return self.resolve_tech(cell, "core"),
            CellRef::Lib { lib, cell } => {
                let row = self.libs.find(lib).filter(|&r| r != 0).ok_or(Code::E001)?;
                (row as u8, self.strings.find("").ok_or(Code::E001)?, cell)
            }
        };
        let stem = self.strings.find(cell).ok_or(Code::E001)?;
        let row = [FileKind::Shcomp, FileKind::Shprim]
            .into_iter()
            .find_map(|k| self.cells.find(lib, dir, stem, k))
            .ok_or(Code::E001)?;
        assert!(self.cells.kind[row] != FileKind::Shbench);
        Ok(row)
    }

    /// Returns the open document of file (`lib`, `dir`, `stem`, `kind`).
    pub(crate) fn find_open(
        &self,
        lib: u8,
        dir: Sym,
        stem: Sym,
        kind: FileKind,
    ) -> Option<DocHandle> {
        let found = self.handles().find(|&h| {
            let o = self.open_ref(h).expect("`handles` yields live handles");
            o.lib == lib && o.dir == dir && o.stem == stem && o.doc.kind() == kind
        });
        assert!(found.is_none_or(|h| self.open_ref(h).is_some()));
        found
    }
}

// ---- private ----

impl Project {
    /// Rebuilds the derived `tech` cells (pdk spec §5) from the map and the current index:
    /// one row per map row whose core symbol and PDK cell resolve, `dir` = flavor, `stem`
    /// = class. Rows that fail are kept in `tech_problems` for [`Project::explain_tech`].
    fn derive_tech(&mut self) {
        let Some(tech) = self.libs.find(TECH_LIB) else {
            return;
        };
        self.cells.retain_not_lib(tech as u8);
        self.tech_problems.clear();
        let Some(Ok(map)) = &self.techmap else {
            return;
        };
        let mut built = Vec::new();
        let mut problems: Vec<TechProblem> = Vec::new();
        for row in 0..map.class.len() {
            let (class, flavor) = (&map.class[row], &map.flavor[row]);
            let Some(sym) = techmap::core_symbol(class) else {
                continue; // a class this core does not resolve yet: ignored, not an error
            };
            let core = self.resolve_at(0, Sym::NONE, &format!("core:{sym}"));
            let pdk = self.resolve_at(0, Sym::NONE, &map.cell[row]);
            let doc = match (core, pdk) {
                (Ok(c), Ok(p)) => {
                    match (self.cells.iface[c].as_ref(), self.cells.iface[p].as_ref()) {
                        (Some(c), Some(p)) => techmap::derive(map, row, c, p),
                        _ => Err(format!("`{}` has no readable symbol", map.cell[row])),
                    }
                }
                (Err(_), _) => Err(format!("core:{sym} is not indexed")),
                (_, Err(_)) => Err(format!(
                    "{} cell `{}` not found (is its library in schemify.project?)",
                    map.pdk, map.cell[row]
                )),
            };
            match doc {
                Ok(d) => built.push((flavor.clone(), class.clone(), d)),
                Err(why) => problems.push(TechProblem {
                    class: class.clone(),
                    flavor: flavor.clone(),
                    why,
                }),
            }
        }
        assert!(
            built.len() + problems.len() <= map.class.len(),
            "one outcome per map row"
        );
        for (flavor, class, doc) in built {
            let hash = sha256::sha256(doc.canonical().as_bytes());
            let (dir, stem) = (self.strings.intern(&flavor), self.strings.intern(&class));
            self.cells
                .push_row(tech as u8, dir, stem, FileKind::Shprim, hash, Some(doc));
        }
        self.tech_problems = problems;
        assert!(self.tech_problems.iter().all(|p| !p.why.is_empty()));
    }
}
