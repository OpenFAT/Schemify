//! The project: libraries, open documents and their edit state, and the request
//! intake spine [`apply_edits`] (edits spec §1–§5).
//!
//! Per open document (`Open`): the tables, the undo/redo stacks (capacity 2^16 each,
//! independent of hierarchy navigation), staged edits (≤ 2^8 open stages), the
//! pending journal output, and the last loaded/saved bytes (the merge base `O`).
//!
//! The crate does no I/O: `take_journal` hands the app bytes to append (or a whole new
//! journal), `save` hands it bytes to write, and `saved` confirms the write.

use crate::Diag;
use crate::document::Document;
use crate::edit::{self, Mode, Op, OpError, ParamFacts, ParamMap, Transaction};
use crate::journal::{
    Analysis, Entry, JOURNAL_HEADER, JRec, analyze, encode_records, parse_journal,
};
use crate::library::{
    self, CellRef, Cells, LibSource, Libs, LockStatus, Locked, ManifestRest, TECH_LIB,
};
use crate::record::RecordBuf;
use crate::sha256::{hex, sha256};
use crate::strings::{ObjId, Strings, Sym};
use crate::tables::Instances;
use crate::techmap::{self, TechMap};
use crate::{Code, Target};
use schemify_format::{FileKind, IdRng, ParseError, RecordKind, merge3, parse};
use std::collections::VecDeque;

/// Undo (and redo) stack capacity per document; the oldest entries drop first.
pub const UNDO_MAX: usize = 1 << 16;
/// Open stages per document.
pub const STAGES_MAX: usize = 1 << 8;
/// Open documents per project.
pub const DOCS_MAX: usize = 1 << 16;
/// Journal compaction thresholds (edits spec §3) and what a compaction keeps.
const COMPACT_TXS: u64 = 1 << 16;
const COMPACT_BYTES: u64 = 64 << 20;
const COMPACT_KEEP: usize = 1 << 12;

/// Handle of an open document: slot index plus generation, so a handle kept across a
/// `close` is detected as stale instead of reaching another document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocHandle {
    /// Slot index.
    pub index: u32,
    /// Generation of the slot when the handle was issued.
    pub generation: u32,
}

/// A staged transaction (edits spec §1 mode `stage`): validated, ids drawn, not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// Stage id, unique among the document's open stages.
    pub id: u16,
    /// The transaction label.
    pub label: String,
    /// The resolved ops.
    pub ops: Vec<Op>,
}

/// One document-mutating request (the `edit_requests` table of the frame spine).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// A transaction (commit or stage).
    Tx(Transaction),
    /// Undo the last committed transaction.
    Undo,
    /// Redo the last undone transaction.
    Redo,
    /// Commit a stage as one transaction.
    Accept(u16),
    /// Drop a stage.
    Reject(u16),
    /// Apply ops as part of committed transaction `seq`, the top of the undo stack (edits
    /// spec §1 "extend"): one undo entry covers both; its journal record is rewritten in
    /// place while still pending, so no record is added. The D20 cache write-back that
    /// follows an edit uses it.
    Extend(u64, Vec<Op>),
}

/// A request addressed to one document. `unix_ms` stamps journal entries (the crate
/// has no clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    /// Target document.
    pub doc: DocHandle,
    /// What to do.
    pub edit: Edit,
    /// Wall-clock time of the request.
    pub unix_ms: u64,
}

/// What one request did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A transaction (or accepted stage) was applied as journal entry `seq`.
    Committed {
        /// Journal sequence number.
        seq: u64,
    },
    /// A transaction was validated and stored as a stage.
    Staged {
        /// Stage id.
        stage: u16,
    },
    /// Entry `seq` was undone.
    Undone {
        /// Journal sequence number.
        seq: u64,
    },
    /// Entry `seq` was redone.
    Redone {
        /// Journal sequence number.
        seq: u64,
    },
    /// A stage was dropped.
    Dropped,
    /// Nothing to undo or redo.
    Nothing,
    /// Validation failed; nothing changed.
    Rejected(Vec<OpError>),
    /// The request could not run: `E040` stale handle or unknown stage, `E044`
    /// read-only document, conflict state or pending recovery.
    Refused(Code),
}

/// Bytes for the app to write to the document's journal sidecar
/// (`.schemify/journal/<project-relative cell path>.journal`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalWrite {
    /// `true`: replace the whole file with `text` (it starts with the header);
    /// `false`: append `text`.
    pub rewrite: bool,
    /// The bytes.
    pub text: String,
}

/// Canonical bytes to write for a save. Hand them back to `Project::saved` once they
/// are on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    /// File contents (format spec §2 canonical form).
    pub bytes: String,
}

/// What `save` remembers until `saved`: which undo depth the bytes are.
#[derive(Clone, Copy, Debug)]
struct PendingSave {
    len: usize,
    depth: usize,
    top_seq: u64,
}

/// What the journal offered when a document was opened (edits spec §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing to recover.
    None,
    /// The file is the state the journal last checkpointed and `txs` journal records
    /// follow: unsaved edits from a crash. Call `recover` or `discard_recovery`.
    Offer {
        /// Records to replay (transactions, undos, redos).
        txs: u32,
    },
    /// The file changed outside Schemify since the journal's last checkpoint; `recover`
    /// replays the journal tail onto it, dropping transactions that no longer apply.
    Diverged {
        /// Records to replay.
        txs: u32,
    },
}

/// A successfully opened document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// Its handle.
    pub handle: DocHandle,
    /// Load findings: `W001` re-ids (the document is dirty then), `E024` duplicate
    /// overrides, `E042` dangling references.
    pub diags: Vec<Diag>,
    /// Journal recovery offer.
    pub recovery: Recovery,
}

/// Why `open` failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The bytes do not parse (format diagnostics).
    Parse(ParseError),
    /// Not a cell file (`E027`), unknown library (`E001`), or too many open documents
    /// (`E043`).
    Code(Code),
    /// That file is already open.
    AlreadyOpen(DocHandle),
}

/// What `external_change` did (edits spec §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum External {
    /// The bytes equal the last loaded/saved bytes.
    Unchanged,
    /// No unsaved edits: reloaded; undo history was cleared.
    Reloaded {
        /// Load findings of the new bytes.
        diags: Vec<Diag>,
    },
    /// Merged cleanly: the merged result is loaded, unsaved edits stay dirty and the
    /// undo stack holds them rebased onto the new bytes.
    Merged {
        /// Unsaved transactions that no longer applied and were dropped from undo.
        dropped: u32,
    },
    /// The merge conflicts: the document is in conflict state (edits and saves refused
    /// with `E044`) until `resolve_conflicts`.
    Conflict {
        /// Number of conflict blocks.
        blocks: u32,
    },
    /// The new bytes do not parse (e.g. a git merge left markers); nothing changed.
    Unreadable(ParseError),
}

/// One conflicting region: both sides' records as canonical text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictBlock {
    /// Our (in-memory) side.
    pub ours: String,
    /// Their (on-disk) side.
    pub theirs: String,
}

/// A resolution for one conflict block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep our records.
    Ours,
    /// Keep their records.
    Theirs,
    /// Use this text (record lines) instead.
    Text(String),
}

#[derive(Debug)]
struct Conflict {
    /// Text around the blocks: `pieces.len() == blocks.len() + 1`.
    pieces: Vec<String>,
    blocks: Vec<ConflictBlock>,
    theirs: Vec<u8>,
}

#[derive(Debug, Default)]
struct JournalOut {
    /// The journal file exists and holds this document's lineage.
    started: bool,
    rewrite: bool,
    text: String,
    txs: u64,
    bytes: u64,
    /// `(seq, byte offset in text)` of the last transaction record appended and not yet
    /// taken: [`Edit::Extend`] rewrites it in place.
    last_tx: Option<(u64, usize)>,
}

#[derive(Debug)]
struct Open {
    lib: u8,
    dir: Sym,
    stem: Sym,
    doc: Document,
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    next_seq: u64,
    /// Undo depth at which the state equals `disk`; `None` when unreachable.
    saved_depth: Option<usize>,
    stages: Vec<Stage>,
    next_stage: u16,
    journal: JournalOut,
    disk: Vec<u8>,
    /// sha256 hex of `disk`, computed on first need (journaling only): hashing a 6 MB
    /// file costs more than loading it.
    disk_hash: Option<String>,
    pending_save: Option<PendingSave>,
    conflict: Option<Conflict>,
    /// Journal records after the checkpoint, awaiting `recover`/`discard_recovery`.
    tail: Option<Vec<JRec>>,
    /// Load changed the model (duplicate ids re-assigned): dirty until saved. Replay
    /// from the journal's `base` reproduces it, because re-iding is deterministic.
    fixups: bool,
}

impl Open {
    /// A document with no history and no disk state (a new, unsaved cell); `open`
    /// overrides the disk fields.
    fn new(lib: u8, dir: Sym, stem: Sym, doc: Document) -> Open {
        Open {
            lib,
            dir,
            stem,
            doc,
            undo: VecDeque::new(),
            redo: Vec::new(),
            next_seq: 1,
            saved_depth: None,
            stages: Vec::new(),
            next_stage: 0,
            journal: JournalOut::default(),
            disk: Vec::new(),
            disk_hash: None,
            pending_save: None,
            conflict: None,
            tail: None,
            fixups: false,
        }
    }
}

/// Returns whether the document differs from its last loaded/saved bytes.
fn dirty(open: &Open) -> bool {
    open.fixups || open.saved_depth != Some(open.undo.len())
}

/// Returns the sha256 hex of the disk bytes, hashing them on first use.
fn disk_hash(open: &mut Open) -> String {
    let hash = open
        .disk_hash
        .get_or_insert_with(|| hex(&sha256(&open.disk)));
    assert_eq!(hash.len(), 64);
    hash.clone()
}

#[derive(Debug, Default)]
struct Slot {
    generation: u32,
    open: Option<Box<Open>>,
}

/// Libraries, their index and interfaces, and the open documents.
#[derive(Debug)]
pub struct Project {
    strings: Strings,
    libs: Libs,
    manifest: ManifestRest,
    lock: Locked,
    cells: Cells,
    slots: Vec<Slot>,
    rng: IdRng,
    /// The indexed `tech.map` (`lib tech map`), or why it did not load.
    techmap: Option<Result<TechMap, String>>,
    /// `(class, flavor, why)` per map row that did not become a derived cell.
    tech_problems: Vec<(String, String, String)>,
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
    pub fn new(
        manifest: Option<&[u8]>,
        lock: Option<&[u8]>,
        seed: u64,
    ) -> Result<Project, ParseError> {
        let (libs, manifest) = library::parse_manifest(manifest)?;
        let lock = library::parse_lock(lock)?;
        assert!(libs.find("").is_some() && libs.find("core").is_some());
        Ok(Project {
            strings: Strings::default(),
            libs,
            manifest,
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
    pub fn reload_manifest(
        &mut self,
        manifest: Option<&[u8]>,
        lock: Option<&[u8]>,
    ) -> Result<(), ReloadError> {
        let (libs, rest) = library::parse_manifest(manifest).map_err(ReloadError::Parse)?;
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
        assert!(self.libs.find("").is_some() && self.libs.find("core").is_some());
        assert!(self.cells.lib.is_empty());
        Ok(())
    }

    // ------------------------------------------------------------------ libraries

    /// Library rows: row 0 is the project itself (`""`), then the manifest's libraries
    /// with the builtin `core` (unless the manifest declares `core`).
    pub fn libs(&self) -> &Libs {
        &self.libs
    }

    /// Manifest `plugin` records, for the plugin host.
    pub fn plugins(&self) -> &[RecordBuf] {
        &self.manifest.plugins
    }

    /// Manifest `set <key> <value>` entries, in file order (`${NAME}` expansion source).
    pub fn settings(&self) -> &[(String, String)] {
        &self.manifest.sets
    }

    /// Lockfile rows.
    pub fn lock(&self) -> &Locked {
        &self.lock
    }

    /// The library index.
    pub fn cells(&self) -> &Cells {
        &self.cells
    }

    /// The arena of `cells.dir` / `cells.stem`.
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
    pub fn index_library(
        &mut self,
        name: &str,
        resolved: &str,
        files: &[(&str, &[u8])],
    ) -> Result<(LockStatus, Vec<(String, Diag)>), Code> {
        let lib = self.libs.find(name).ok_or(Code::E001)?;
        let digests: Vec<_> = files.iter().map(|f| sha256(f.1)).collect();
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
        let mut problems = Vec::new();
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
                Err(why) => problems.push((class.clone(), flavor.clone(), why)),
            }
        }
        for (flavor, class, doc) in built {
            let hash = sha256(doc.canonical().as_bytes());
            let (dir, stem) = (self.strings.intern(&flavor), self.strings.intern(&class));
            self.cells
                .push_row(tech as u8, dir, stem, FileKind::Shprim, hash, Some(doc));
        }
        self.tech_problems = problems;
        assert!(self.tech_problems.iter().all(|p| !p.2.is_empty()));
    }

    /// The indexed technology map, `None` before `lib tech` is indexed or when it failed to
    /// parse (see [`Project::explain_tech`]).
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
            let names: Vec<&str> = techmap::CLASSES.iter().map(|c| c.0).collect();
            return format!("`{class}` is not a technology class ({})", names.join(", "));
        }
        if map.find(class, flavor).is_none() {
            return format!("{} has no {flavor} {class}", map.pdk);
        }
        let why = self
            .tech_problems
            .iter()
            .find(|p| p.0 == class && p.1 == flavor)
            .map(|p| p.2.clone());
        why.unwrap_or_else(|| format!("tech:{class} ({flavor}) resolves"))
    }

    /// Pins library `name` in the lockfile at its last indexed content.
    ///
    /// # Errors
    ///
    /// `E001` unknown or never indexed library; `E043` for the project row and the
    /// builtin `core` (bundled with the tool, never locked).
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
        self.lock.set("lib", name, kind, &resolved, &hex(&hash));
        assert!(self.lock.find("lib", name).is_some());
        Ok(())
    }

    /// Returns the lockfile bytes (canonical; sorted by type, then name).
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

    fn resolve_at(&self, lib: u8, dir: Sym, cellref: &str) -> Result<usize, Code> {
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
            assert!(r < self.cells.lib.len() && self.cells.kind[r] != FileKind::Shbench);
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
    pub fn open_handle(&self, row: usize) -> Option<DocHandle> {
        assert!(row < self.cells.lib.len(), "cell row from a previous index");
        let c = &self.cells;
        let found = self.handles().find(|&h| {
            let o = self.open_ref(h).expect("live handle");
            o.lib == c.lib[row]
                && o.dir == c.dir[row]
                && o.stem == c.stem[row]
                && o.doc.kind() == c.kind[row]
        });
        assert!(found.is_none_or(|h| self.open_ref(h).is_some()));
        found
    }

    /// Returns the symbol of indexed cell `row` (interface and graphics, ranks 0–2):
    /// the open document when the cell is open (its edits are the current symbol),
    /// else the indexed one.
    pub fn interface(&self, row: usize) -> Option<&Document> {
        let open = self.open_handle(row).and_then(|h| self.doc(h));
        open.or(self.cells.iface[row].as_ref())
    }

    // ------------------------------------------------------------------ documents

    /// Opens a cell file of library `lib` (`""` for the project's own files) at
    /// `relpath` from its bytes and, when the sidecar exists, its journal bytes.
    ///
    /// The journal's history up to the checkpoint matching the file becomes the undo
    /// history; edits after it are offered as [`Recovery`] and must be recovered or
    /// discarded before the document accepts edits.
    ///
    /// # Errors
    ///
    /// See [`OpenError`].
    pub fn open(
        &mut self,
        lib: &str,
        relpath: &str,
        bytes: &[u8],
        journal: Option<&[u8]>,
    ) -> Result<Opened, OpenError> {
        let lib = self.libs.find(lib).ok_or(OpenError::Code(Code::E001))? as u8;
        let (dir, stem) = split_path(relpath);
        let (dir, stem) = (self.strings.intern(dir), self.strings.intern(stem));
        let model = parse(bytes).map_err(OpenError::Parse)?;
        if !matches!(
            model.kind(),
            FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
        ) {
            return Err(OpenError::Code(Code::E027));
        }
        let twin = self.handles().find(|&h| {
            let o = self.open_ref(h).expect("live handle");
            o.lib == lib && o.dir == dir && o.stem == stem && o.doc.kind() == model.kind()
        });
        if let Some(h) = twin {
            return Err(OpenError::AlreadyOpen(h));
        }
        let loaded = Document::from_format(&model);
        let reids = loaded.diags.iter().any(|d| d.code == Code::W001);
        let mut open = Open {
            saved_depth: Some(0),
            disk: bytes.to_vec(),
            fixups: reids,
            ..Open::new(lib, dir, stem, loaded.doc)
        };
        let recovery = match journal.map(|j| parse_journal(j, open.doc.kind())) {
            Some(Ok(recs)) => {
                let hash = disk_hash(&mut open);
                let analysis = analyze(recs, &hash, UNDO_MAX);
                install_journal(&mut open, analysis)
            }
            _ => Recovery::None,
        };
        let handle = self.alloc(open).map_err(OpenError::Code)?;
        let pending = self.open_ref(handle).is_some_and(|o| o.tail.is_some());
        assert_eq!(
            pending,
            recovery != Recovery::None,
            "an offer is pending until decided"
        );
        Ok(Opened {
            handle,
            diags: loaded.diags,
            recovery,
        })
    }

    /// Creates a new, empty cell document (not yet on disk; dirty until saved).
    ///
    /// # Errors
    ///
    /// `E001` unknown library, `E027` not a cell kind, `E043` too many documents.
    pub fn create(&mut self, lib: &str, relpath: &str, kind: FileKind) -> Result<DocHandle, Code> {
        let lib = self.libs.find(lib).ok_or(Code::E001)? as u8;
        if !matches!(
            kind,
            FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
        ) {
            return Err(Code::E027);
        }
        let (dir, stem) = split_path(relpath);
        let (dir, stem) = (self.strings.intern(dir), self.strings.intern(stem));
        let h = self.alloc(Open::new(lib, dir, stem, Document::new(kind)))?;
        assert!(self.is_dirty(h), "a new document is unsaved");
        Ok(h)
    }

    /// Closes a document; its handle goes stale. Returns whether it had unsaved edits
    /// (the app asks before calling).
    ///
    /// # Errors
    ///
    /// `E040` stale handle.
    pub fn close(&mut self, h: DocHandle) -> Result<bool, Code> {
        let slot = self.slot_mut(h).ok_or(Code::E040)?;
        let open = slot.open.take().expect("live handle");
        slot.generation = slot.generation.wrapping_add(1);
        assert!(self.open_ref(h).is_none());
        Ok(dirty(&open))
    }

    /// Returns the tables of an open document; `None` for a stale handle.
    pub fn doc(&self, h: DocHandle) -> Option<&Document> {
        self.open_ref(h).map(|o| &o.doc)
    }

    /// Returns every live handle, in slot order.
    pub fn handles(&self) -> impl Iterator<Item = DocHandle> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.open.is_some())
            .map(|(i, s)| DocHandle {
                index: i as u32,
                generation: s.generation,
            })
    }

    /// Returns whether the document differs from its last loaded/saved bytes.
    pub fn is_dirty(&self, h: DocHandle) -> bool {
        self.open_ref(h).is_some_and(dirty)
    }

    /// Returns (undo depth, redo depth).
    pub fn history_len(&self, h: DocHandle) -> (usize, usize) {
        self.open_ref(h)
            .map_or((0, 0), |o| (o.undo.len(), o.redo.len()))
    }

    /// Returns the ops an [`apply_edits`] outcome applied to document `h`, in application
    /// order, borrowed from its undo entry: a commit's or redo's forward ops (fresh ids
    /// drawn, values snapped; an extended entry's ops include the extension), an undo's
    /// inverse ops. Committed as one transaction on the state before the request, they
    /// give the state after it; [`Op::subject`] names each changed record.
    ///
    /// `None` when the outcome changed nothing (staged, dropped, nothing, rejected,
    /// refused), the handle is stale, or the entry is gone: a later commit to `h` clears
    /// the redo stack an undone entry sits on, and the oldest undo entries drop past
    /// [`UNDO_MAX`]. Read it before the next request to `h` to be sure of an answer.
    /// Cost: a scan from the top of one stack, O(1) right after the request.
    pub fn applied(&self, h: DocHandle, outcome: &Outcome) -> Option<&[Op]> {
        let open = self.open_ref(h)?;
        let ops = match *outcome {
            Outcome::Committed { seq } | Outcome::Redone { seq } => {
                let e = open.undo.iter().rev().find(|e| e.seq == seq)?;
                &e.forward
            }
            Outcome::Undone { seq } => &open.redo.iter().rev().find(|e| e.seq == seq)?.inverse,
            _ => return None,
        };
        Some(ops)
    }

    /// Returns the labels of the undo stack, oldest first (for undo menus).
    pub fn undo_labels(&self, h: DocHandle) -> Vec<&str> {
        self.open_ref(h).map_or(Vec::new(), |o| {
            o.undo.iter().map(|e| e.label.as_str()).collect()
        })
    }

    /// Returns the open stages (the `staged_edits` table) of a document.
    pub fn stages(&self, h: DocHandle) -> &[Stage] {
        self.open_ref(h).map_or(&[], |o| &o.stages)
    }

    /// Returns the document with stage `stage` applied, for the visual diff
    /// (design ⊕ stage). Cost: one clone of the document.
    ///
    /// # Errors
    ///
    /// `E040` stale handle or unknown stage; the stage's op errors when it no longer
    /// applies.
    pub fn preview(&self, h: DocHandle, stage: u16) -> Result<Document, Vec<OpError>> {
        let refused = |code| vec![OpError { op: 0, code }];
        let open = self.open_ref(h).ok_or_else(|| refused(Code::E040))?;
        let st = open
            .stages
            .iter()
            .find(|s| s.id == stage)
            .ok_or_else(|| refused(Code::E040))?;
        let mut doc = open.doc.clone();
        let mut ops = st.ops.clone();
        let inverse = edit::replay(&mut doc, &mut ops)?;
        assert_eq!(inverse.len(), st.ops.len());
        Ok(doc)
    }

    /// Takes the journal bytes produced since the last call (`None` when there are
    /// none). The app appends them (or replaces the file when `rewrite`), batching
    /// fsyncs at ≤ 100 ms.
    pub fn take_journal(&mut self, h: DocHandle) -> Option<JournalWrite> {
        let open = self.open_mut(h)?;
        if open.journal.text.is_empty() {
            return None;
        }
        open.journal.last_tx = None;
        let w = JournalWrite {
            rewrite: std::mem::take(&mut open.journal.rewrite),
            text: std::mem::take(&mut open.journal.text),
        };
        assert!(!w.rewrite || w.text.starts_with(JOURNAL_HEADER));
        Some(w)
    }

    /// Instance parameter overrides equal (as text) to the cell's default (D40): kept
    /// as the user wrote them, listed for the canonicalizer (`W002`). Sorted by
    /// (instance id, param).
    pub fn redundant_overrides(&self, h: DocHandle) -> Vec<(ObjId, String)> {
        let Some(open) = self.open_ref(h) else {
            return Vec::new();
        };
        let doc = &open.doc;
        let s = doc.strings();
        let mut out = Vec::new();
        for i in 0..doc.instances().id.len() {
            let Ok(row) = self.resolve_at(open.lib, open.dir, s.get(doc.instances().cell[i]))
            else {
                continue;
            };
            let Some(iface) = self.interface(row) else {
                continue;
            };
            let is = iface.strings();
            for (k, vals) in doc.pool().attrs(doc.instances().attrs[i]) {
                let key = s.get(k);
                if !Instances::is_override(key) || vals.len() != 1 {
                    continue;
                }
                let p = &iface.params().fields;
                let default = p.iter().find(|f| is.get(f[0]) == key).map(|f| is.get(f[1]));
                if default == Some(s.get(vals[0])) {
                    out.push((doc.instances().id[i], key.to_owned()));
                }
            }
        }
        out.sort();
        assert!(out.iter().all(|(id, _)| doc.find_id(*id).is_some()));
        out
    }

    /// Builds the transaction that drops the redundant overrides of
    /// [`Project::redundant_overrides`] (the D40 canonicalizer; the user applies it).
    pub fn canonicalize_overrides(&self, h: DocHandle) -> Transaction {
        let strings = self.doc(h).map(Document::strings);
        let redundant = self.redundant_overrides(h);
        let n = redundant.len();
        let ops: Vec<Op> = redundant
            .into_iter()
            .map(|(id, key)| Op::RemoveAttr {
                target: Target::new(RecordKind::Inst, &id.text(strings.expect("live handle"))),
                key,
                first: None,
            })
            .collect();
        assert_eq!(ops.len(), n);
        Transaction {
            label: "drop overrides equal to defaults".into(),
            mode: Mode::Commit,
            ops,
        }
    }

    // ------------------------------------------------------------------ recovery

    /// Replays the journal records offered at `open`. Returns (applied, dropped):
    /// transactions that no longer validate (only possible after `Diverged`) are
    /// dropped from the undo history.
    ///
    /// # Errors
    ///
    /// `E040` stale handle or nothing to recover.
    pub fn recover(&mut self, h: DocHandle) -> Result<(u32, u32), Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        let tail = open.tail.take().ok_or(Code::E040)?;
        let matched = open.journal.started;
        let (mut applied, mut dropped) = (0, 0);
        for rec in tail {
            let ok = match rec {
                JRec::Tx(mut e) => match edit::replay(&mut open.doc, &mut e.forward) {
                    Ok(inv) => {
                        e.inverse = inv;
                        push_commit(open, e);
                        true
                    }
                    Err(_) => false,
                },
                JRec::Undo(s) => {
                    open.undo.back().is_some_and(|e| e.seq == s) && undo_top(open).is_ok()
                }
                JRec::Redo(s) => {
                    open.redo.last().is_some_and(|e| e.seq == s) && redo_top(open).is_ok()
                }
                JRec::Base(_) | JRec::Hist(_) | JRec::Save(..) => continue,
            };
            if ok { applied += 1 } else { dropped += 1 }
        }
        if !matched || dropped > 0 {
            // The journal on disk no longer describes this lineage: rewrite it.
            open.saved_depth = open.saved_depth.filter(|&d| d <= open.undo.len());
            rewrite_journal(open);
        }
        assert!(open.tail.is_none());
        Ok((applied, dropped))
    }

    /// Drops the journal records offered at `open` and rewrites the journal without them.
    ///
    /// # Errors
    ///
    /// `E040` stale handle or nothing to recover.
    pub fn discard_recovery(&mut self, h: DocHandle) -> Result<(), Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        open.tail.take().ok_or(Code::E040)?;
        open.redo.clear();
        rewrite_journal(open);
        assert!(open.tail.is_none());
        Ok(())
    }

    // ------------------------------------------------------------------ saves

    /// Returns the canonical bytes to write (edits spec §5); `None` when not dirty.
    /// The app writes them atomically, then calls [`Project::saved`].
    ///
    /// # Errors
    ///
    /// `E040` stale handle; `E044` read-only, in conflict or pending recovery; `E025`
    /// when the kind rules fail (the file would not parse).
    pub fn save(&mut self, h: DocHandle) -> Result<Option<SaveRequest>, Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        if open.doc.read_only() || open.conflict.is_some() || open.tail.is_some() {
            return Err(Code::E044);
        }
        if !dirty(open) {
            return Ok(None);
        }
        if !open.doc.kind_rules_ok() {
            return Err(Code::E025);
        }
        let bytes = open.doc.canonical();
        open.pending_save = Some(PendingSave {
            len: bytes.len(),
            depth: open.undo.len(),
            top_seq: open.undo.back().map_or(0, |e| e.seq),
        });
        assert!(open.pending_save.is_some_and(|p| p.len == bytes.len()));
        Ok(Some(SaveRequest { bytes }))
    }

    /// Confirms that `written` (the bytes of the last `save`, handed back by value so
    /// they are not copied) is on disk: it becomes the merge base, the state it holds is
    /// clean, the journal gets `save`, and a journal past 2^16 transactions or 64 MiB
    /// is compacted to the last 2^12. Edits made between `save` and `saved` stay dirty.
    ///
    /// # Errors
    ///
    /// `E040` stale handle, no pending save, or `written` is not its bytes (length).
    pub fn saved(&mut self, h: DocHandle, written: String) -> Result<(), Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        let p = open
            .pending_save
            .take()
            .filter(|p| p.len == written.len())
            .ok_or(Code::E040)?;
        open.fixups = false;
        // The saved state is still on the undo stack if the entry it ended with is.
        let reachable =
            p.depth <= open.undo.len() && (p.depth == 0 || open.undo[p.depth - 1].seq == p.top_seq);
        if !reachable {
            // Undone below the save and re-edited before the write finished: describe
            // the current state relative to the new bytes, as after an external change.
            let current = open.doc.canonical();
            open.saved_depth = None;
            rebase(open, &current, written.as_bytes());
            return Ok(());
        }
        open.disk = written.into_bytes();
        open.disk_hash = None;
        open.saved_depth = Some(p.depth);
        let at_save = p.depth == open.undo.len();
        if at_save && (open.journal.txs > COMPACT_TXS || open.journal.bytes > COMPACT_BYTES) {
            while open.undo.len() > COMPACT_KEEP {
                open.undo.pop_front();
            }
            open.redo.clear();
            open.saved_depth = Some(open.undo.len());
            rewrite_journal(open);
        } else if open.journal.started && at_save {
            let rec = JRec::Save(p.top_seq, disk_hash(open));
            append_journal(open, &[rec]);
        } else if open.journal.started {
            // Edits landed between `save` and `saved`: the journal's position no longer
            // matches the saved state, so restate the lineage from the new bytes.
            rewrite_journal(open);
        }
        assert!(open.pending_save.is_none());
        Ok(())
    }

    // ------------------------------------------------------------------ external changes

    /// Handles new bytes of an open file written outside Schemify (edits spec §4).
    ///
    /// # Errors
    ///
    /// `E040` stale handle; `E044` while in conflict state.
    pub fn external_change(&mut self, h: DocHandle, bytes: &[u8]) -> Result<External, Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        if open.conflict.is_some() {
            return Err(Code::E044);
        }
        if bytes == open.disk.as_slice() {
            return Ok(External::Unchanged);
        }
        let theirs = match parse(bytes) {
            Ok(m) => m,
            Err(e) => return Ok(External::Unreadable(e)),
        };
        if open.saved_depth == Some(open.undo.len()) || open.doc.read_only() {
            let loaded = Document::from_format(&theirs);
            open.fixups = loaded.diags.iter().any(|d| d.code == Code::W001);
            open.doc = loaded.doc;
            open.undo.clear();
            open.redo.clear();
            open.stages.clear();
            open.saved_depth = Some(0);
            open.disk = bytes.to_vec();
            open.disk_hash = None;
            if open.journal.started {
                rewrite_journal(open);
            }
            assert!(open.undo.is_empty() && open.saved_depth == Some(0));
            return Ok(External::Reloaded {
                diags: loaded.diags,
            });
        }
        let base = parse(&open.disk).expect("the merge base parsed when it was loaded or saved");
        let ours = open.doc.to_format();
        let merged = merge3(&base, &ours, &theirs);
        if merged.conflicts == 0 {
            let dropped = rebase(open, &merged.text, bytes);
            return Ok(External::Merged { dropped });
        }
        let (pieces, blocks) = split_conflicts(&merged.text);
        assert_eq!(blocks.len() as u32, merged.conflicts);
        let n = blocks.len() as u32;
        open.conflict = Some(Conflict {
            pieces,
            blocks,
            theirs: bytes.to_vec(),
        });
        Ok(External::Conflict { blocks: n })
    }

    /// Returns the conflict blocks of a document in conflict state.
    pub fn conflicts(&self, h: DocHandle) -> Option<&[ConflictBlock]> {
        self.open_ref(h)?
            .conflict
            .as_ref()
            .map(|c| c.blocks.as_slice())
    }

    /// Resolves every conflict block (one choice each, in order) and loads the result
    /// like a clean merge.
    ///
    /// # Errors
    ///
    /// `E040` stale handle or no conflict, `E043` wrong number of choices, or the
    /// assembled text's parse error (the conflict state is kept).
    pub fn resolve_conflicts(
        &mut self,
        h: DocHandle,
        choices: &[Choice],
    ) -> Result<External, OpenError> {
        let open = self.open_mut(h).ok_or(OpenError::Code(Code::E040))?;
        let c = open.conflict.as_ref().ok_or(OpenError::Code(Code::E040))?;
        if choices.len() != c.blocks.len() {
            return Err(OpenError::Code(Code::E043));
        }
        let mut text = c.pieces[0].clone();
        for (i, choice) in choices.iter().enumerate() {
            let side = match choice {
                Choice::Ours => c.blocks[i].ours.as_str(),
                Choice::Theirs => c.blocks[i].theirs.as_str(),
                Choice::Text(t) => t.as_str(),
            };
            text.push_str(side);
            if !side.is_empty() && !side.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&c.pieces[i + 1]);
        }
        let model = parse(text.as_bytes()).map_err(OpenError::Parse)?;
        if model.kind() != open.doc.kind() {
            return Err(OpenError::Code(Code::E043)); // an edited block changed the header kind
        }
        let merged = schemify_format::write(&model);
        let theirs = c.theirs.clone();
        open.conflict = None;
        let dropped = rebase(open, &merged, &theirs);
        assert!(open.conflict.is_none() && open.saved_depth == Some(0));
        Ok(External::Merged { dropped })
    }

    // ------------------------------------------------------------------ internals

    fn alloc(&mut self, open: Open) -> Result<DocHandle, Code> {
        let index = match self.slots.iter().position(|s| s.open.is_none()) {
            Some(i) => i,
            None if self.slots.len() < DOCS_MAX => {
                self.slots.push(Slot::default());
                self.slots.len() - 1
            }
            None => return Err(Code::E043),
        };
        self.slots[index].open = Some(Box::new(open));
        let h = DocHandle {
            index: index as u32,
            generation: self.slots[index].generation,
        };
        assert!(self.open_ref(h).is_some());
        Ok(h)
    }

    fn slot_mut(&mut self, h: DocHandle) -> Option<&mut Slot> {
        self.slots
            .get_mut(h.index as usize)
            .filter(|s| s.generation == h.generation && s.open.is_some())
    }

    fn open_ref(&self, h: DocHandle) -> Option<&Open> {
        let slot = self.slots.get(h.index as usize)?;
        (slot.generation == h.generation)
            .then_some(slot.open.as_deref())
            .flatten()
    }

    fn open_mut(&mut self, h: DocHandle) -> Option<&mut Open> {
        self.slot_mut(h).and_then(|s| s.open.as_deref_mut())
    }

    /// Collects what edits need about the cells instances in `ops` reference.
    fn param_map(&self, open: &Open, ops: &[Op]) -> ParamMap {
        let doc = &open.doc;
        let mut refs: Vec<String> = Vec::new();
        for op in ops {
            match op {
                Op::Insert { rec, .. } | Op::Put { rec, .. } if rec.kind == RecordKind::Inst => {
                    refs.extend(rec.fields.get(1).cloned());
                }
                Op::SetField {
                    target,
                    index: 1,
                    value,
                } if target.kind == RecordKind::Inst => {
                    refs.push(value.clone());
                }
                Op::SetAttr { target, .. } if target.kind == RecordKind::Inst => {
                    if let Some(loc) = doc.locate(RecordKind::Inst, &target.key) {
                        let cell = doc.instances().cell[loc.row as usize];
                        refs.push(doc.strings().get(cell).to_owned());
                    }
                }
                _ => {}
            }
        }
        let mut map = ParamMap::new();
        for r in refs {
            if map.contains_key(&r) {
                continue;
            }
            let facts = self
                .resolve_at(open.lib, open.dir, &r)
                .ok()
                .and_then(|row| self.interface(row))
                .map(param_facts);
            map.insert(r, facts);
        }
        assert!(map.len() <= ops.len());
        map
    }

    fn tx(&mut self, h: DocHandle, tx: &Transaction, unix_ms: u64) -> Outcome {
        let Some(open) = self.open_ref(h) else {
            return Outcome::Refused(Code::E040);
        };
        if refused(open) {
            return Outcome::Refused(Code::E044);
        }
        let params = self.param_map(open, &tx.ops);
        let mut ops = tx.ops.clone();
        let Project { slots, rng, .. } = self;
        let open = slots[h.index as usize]
            .open
            .as_deref_mut()
            .expect("checked live");
        let inverse = match edit::apply(&mut open.doc, &mut ops, &params, rng) {
            Ok(inv) => inv,
            Err(errors) => return Outcome::Rejected(errors),
        };
        match tx.mode {
            Mode::Stage => {
                let mut inv = inverse;
                let restored = edit::restore(&mut open.doc, &mut inv);
                assert!(restored.is_ok(), "an inverse always applies");
                stage(open, &tx.label, ops)
            }
            Mode::Commit => Outcome::Committed {
                seq: commit(open, &tx.label, unix_ms, ops, inverse),
            },
        }
    }
}

impl Project {
    /// [`Edit::Extend`]: applies `ops` inside entry `seq`. Refused (`E040`) unless `seq` is
    /// the top of the undo stack with redo empty, and (`E044`) when the document is
    /// refused, the entry is part of the saved state, or its journal record was written
    /// out and the journal cannot be rewritten (no reachable saved state).
    fn extend(&mut self, h: DocHandle, seq: u64, ops: &[Op]) -> Outcome {
        let Some(open) = self.open_ref(h) else {
            return Outcome::Refused(Code::E040);
        };
        if !open.redo.is_empty() || open.undo.back().is_none_or(|e| e.seq != seq) {
            return Outcome::Refused(Code::E040);
        }
        let pending = open.journal.last_tx.is_some_and(|(s, _)| s == seq);
        let rewritable = pending || !open.journal.started || open.saved_depth.is_some();
        let saved = open.saved_depth.is_some_and(|d| d >= open.undo.len());
        if refused(open) || saved || !rewritable {
            return Outcome::Refused(Code::E044);
        }
        let params = self.param_map(open, ops);
        let mut ops = ops.to_vec();
        let Project { slots, rng, .. } = self;
        let open = slots[h.index as usize]
            .open
            .as_deref_mut()
            .expect("checked live");
        let mut inverse = match edit::apply(&mut open.doc, &mut ops, &params, rng) {
            Ok(inv) => inv,
            Err(errors) => return Outcome::Rejected(errors),
        };
        let entry = open.undo.back_mut().expect("checked above");
        entry.forward.extend(ops);
        // Undo reverts the extension first: it was applied last.
        inverse.append(&mut entry.inverse);
        entry.inverse = inverse;
        let entry = entry.clone();
        match open.journal.last_tx {
            Some((s, at)) if s == seq => {
                let j = &mut open.journal;
                j.bytes -= (j.text.len() - at) as u64;
                j.txs -= 1;
                j.text.truncate(at);
                append_journal(open, &[JRec::Tx(entry)]);
                open.journal.last_tx = Some((seq, at));
            }
            _ if open.journal.started => rewrite_journal(open),
            _ => {}
        }
        open.doc.compact_if_needed();
        assert!(open.undo.back().is_some_and(|e| e.seq == seq) && open.redo.is_empty());
        Outcome::Committed { seq }
    }
}

/// Applies edit requests in order (the frame spine's `doc::apply_edits`): transactions
/// (commit or stage), undo, redo, accept and reject. Each request is all-or-nothing;
/// a failing request does not stop the ones after it. Journal output accumulates per
/// document for [`Project::take_journal`]; [`Project::applied`] reads back the ops a
/// request applied, without copying them.
pub fn apply_edits(project: &mut Project, requests: &[EditRequest]) -> Vec<Outcome> {
    let mut out = Vec::with_capacity(requests.len());
    for req in requests {
        let outcome = match &req.edit {
            Edit::Tx(tx) => project.tx(req.doc, tx, req.unix_ms),
            Edit::Extend(seq, ops) => project.extend(req.doc, *seq, ops),
            Edit::Accept(id) => {
                let stage = project
                    .open_ref(req.doc)
                    .and_then(|o| o.stages.iter().find(|s| s.id == *id).cloned());
                match stage {
                    None => Outcome::Refused(Code::E040),
                    Some(s) => {
                        let tx = Transaction {
                            label: s.label,
                            mode: Mode::Commit,
                            ops: s.ops,
                        };
                        let o = project.tx(req.doc, &tx, req.unix_ms);
                        if matches!(o, Outcome::Committed { .. }) {
                            let open = project.open_mut(req.doc).expect("just committed");
                            open.stages.retain(|s| s.id != *id);
                        }
                        o
                    }
                }
            }
            Edit::Reject(id) => match project.open_mut(req.doc) {
                None => Outcome::Refused(Code::E040),
                Some(open) => {
                    let before = open.stages.len();
                    open.stages.retain(|s| s.id != *id);
                    if open.stages.len() < before {
                        Outcome::Dropped
                    } else {
                        Outcome::Refused(Code::E040)
                    }
                }
            },
            Edit::Undo | Edit::Redo => match project.open_mut(req.doc) {
                None => Outcome::Refused(Code::E040),
                Some(open) if refused(open) => Outcome::Refused(Code::E044),
                Some(open) if req.edit == Edit::Undo => match undo_top(open) {
                    Ok(Some(seq)) => {
                        append_journal(open, &[JRec::Undo(seq)]);
                        Outcome::Undone { seq }
                    }
                    Ok(None) => Outcome::Nothing,
                    Err(e) => Outcome::Rejected(e),
                },
                Some(open) => match redo_top(open) {
                    Ok(Some(seq)) => {
                        append_journal(open, &[JRec::Redo(seq)]);
                        Outcome::Redone { seq }
                    }
                    Ok(None) => Outcome::Nothing,
                    Err(e) => Outcome::Rejected(e),
                },
            },
        };
        out.push(outcome);
    }
    assert_eq!(out.len(), requests.len());
    out
}

// ---------------------------------------------------------------------- per-document

fn refused(open: &Open) -> bool {
    open.doc.read_only() || open.conflict.is_some() || open.tail.is_some()
}

fn split_path(relpath: &str) -> (&str, &str) {
    let (dir, file) = relpath.rsplit_once('/').unwrap_or(("", relpath));
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    assert!(!stem.contains('/') && dir.len() + stem.len() <= relpath.len());
    (dir, stem)
}

fn param_facts(iface: &Document) -> Vec<ParamFacts> {
    assert!(
        matches!(iface.kind(), FileKind::Shprim | FileKind::Shcomp),
        "benches have no params"
    );
    let s = iface.strings();
    let p = iface.params();
    let pool = iface.pool();
    let first = |row: usize, key: &str| {
        pool.find_attr(s, p.attrs[row], key)
            .and_then(|v| v.first())
            .map(|v| s.get(*v).to_owned())
    };
    (0..p.kind.len())
        .map(|row| ParamFacts {
            name: s.get(p.fields[row][0]).to_owned(),
            computed: pool.find_attr(s, p.attrs[row], "computed").is_some(),
            grid: first(row, "grid"),
            step: first(row, "step"),
            min: first(row, "min"),
        })
        .collect()
}

fn stage(open: &mut Open, label: &str, ops: Vec<Op>) -> Outcome {
    if open.stages.len() >= STAGES_MAX {
        return Outcome::Refused(Code::E043);
    }
    let mut id = open.next_stage;
    while open.stages.iter().any(|s| s.id == id) {
        id = id.wrapping_add(1);
    }
    open.next_stage = id.wrapping_add(1);
    open.stages.push(Stage {
        id,
        label: label.to_owned(),
        ops,
    });
    assert!(open.stages.len() <= STAGES_MAX);
    assert_eq!(
        open.stages.iter().filter(|s| s.id == id).count(),
        1,
        "stage ids are unique"
    );
    Outcome::Staged { stage: id }
}

/// Pushes a committed transaction on the undo stack, clears redo, journals it.
fn commit(open: &mut Open, label: &str, unix_ms: u64, forward: Vec<Op>, inverse: Vec<Op>) -> u64 {
    let seq = open.next_seq;
    open.next_seq += 1;
    let entry = Entry {
        seq,
        unix_ms,
        label: label.to_owned(),
        forward,
        inverse,
    };
    // The journal starts with the first commit of a document that exists on disk
    // (`ponytail:` a never-saved document has no journal until its first save).
    if !open.journal.started && !open.disk.is_empty() && open.saved_depth.is_some() {
        rewrite_journal(open);
    }
    let at = open.journal.text.len();
    append_journal(open, &[JRec::Tx(entry.clone())]);
    open.journal.last_tx = open.journal.started.then_some((seq, at));
    push_commit(open, entry);
    open.doc.compact_if_needed();
    assert!(open.redo.is_empty());
    seq
}

/// The stack side of a commit: clears redo, pushes undo.
fn push_commit(open: &mut Open, entry: Entry) {
    let seq = entry.seq;
    if open.saved_depth.is_some_and(|d| d > open.undo.len()) {
        open.saved_depth = None; // the saved state was in redo history, now gone
    }
    open.redo.clear();
    push_undo(open, entry);
    assert!(open.undo.back().is_some_and(|e| e.seq == seq) && open.redo.is_empty());
}

fn push_undo(open: &mut Open, entry: Entry) {
    open.undo.push_back(entry);
    if open.undo.len() > UNDO_MAX {
        open.undo.pop_front();
        open.saved_depth = match open.saved_depth {
            Some(0) | None => None,
            Some(d) => Some(d - 1),
        };
    }
    assert!(open.undo.len() <= UNDO_MAX);
}

fn undo_top(open: &mut Open) -> Result<Option<u64>, Vec<OpError>> {
    let Some(mut entry) = open.undo.pop_back() else {
        return Ok(None);
    };
    let mut ops = entry.inverse.clone();
    if let Err(e) = edit::restore(&mut open.doc, &mut ops) {
        open.undo.push_back(entry);
        return Err(e);
    }
    let seq = entry.seq;
    entry.inverse = ops;
    open.redo.push(entry);
    // Entries only move between the stacks, and a commit clears redo: their sum is
    // bounded by the undo cap, so redo needs no cap of its own.
    assert!(open.undo.len() + open.redo.len() <= UNDO_MAX);
    assert!(open.redo.last().is_some_and(|e| e.seq == seq));
    Ok(Some(seq))
}

fn redo_top(open: &mut Open) -> Result<Option<u64>, Vec<OpError>> {
    let Some(mut entry) = open.redo.pop() else {
        return Ok(None);
    };
    let mut ops = entry.forward.clone();
    match edit::restore(&mut open.doc, &mut ops) {
        Ok(inv) => {
            let seq = entry.seq;
            entry.inverse = inv;
            open.undo.push_back(entry);
            assert!(
                open.undo.len() <= UNDO_MAX,
                "redo holds only undone entries"
            );
            Ok(Some(seq))
        }
        Err(e) => {
            open.redo.push(entry);
            Err(e)
        }
    }
}

fn append_journal(open: &mut Open, recs: &[JRec]) {
    if !open.journal.started {
        return; // nothing committed yet: the journal starts with the first commit
    }
    let text = encode_records(recs);
    assert!(recs.is_empty() || text.ends_with('\n'));
    open.journal.txs += recs.iter().filter(|r| matches!(r, JRec::Tx(_))).count() as u64;
    open.journal.bytes += text.len() as u64;
    open.journal.text.push_str(&text);
    assert!(open.journal.text.len() >= text.len());
}

/// Replaces the journal with one describing the current lineage: `base` (the disk
/// bytes), the undo entries up to the saved depth as history, the rest as
/// transactions to replay. Redo history is not kept across a rewrite.
fn rewrite_journal(open: &mut Open) {
    // Every rewrite happens where the disk state is on the undo stack (open, save,
    // reload, merge, recovery), so replaying from `base` reproduces the state.
    let depth = open
        .saved_depth
        .expect("rewrites happen at a reachable disk state");
    assert!(depth <= open.undo.len());
    let mut recs = vec![JRec::Base(disk_hash(open))];
    recs.extend(open.undo.iter().take(depth).cloned().map(JRec::Hist));
    recs.extend(open.undo.iter().skip(depth).cloned().map(JRec::Tx));
    let mut text = JOURNAL_HEADER.to_owned();
    text.push_str(&encode_records(&recs));
    open.journal = JournalOut {
        started: true,
        rewrite: true,
        txs: (recs.len() - 1) as u64,
        bytes: text.len() as u64,
        text,
        last_tx: None,
    };
    open.redo.clear();
    assert!(open.journal.text.starts_with(JOURNAL_HEADER));
}

/// Installs a journal analysis on a freshly opened document: history if a checkpoint
/// matched, the tail as a pending recovery.
fn install_journal(open: &mut Open, a: Analysis) -> Recovery {
    assert!(
        open.undo.is_empty() && open.tail.is_none(),
        "installed on a fresh open"
    );
    open.next_seq = a.next_seq;
    open.journal.started = a.matched;
    if a.matched {
        open.undo = a.undo.into();
        open.redo = a.redo;
        open.saved_depth = Some(open.undo.len());
    }
    assert!(open.undo.iter().all(|e| e.seq < open.next_seq));
    let n = a.tail.len() as u32;
    if n == 0 {
        return Recovery::None;
    }
    open.tail = Some(a.tail);
    if a.matched {
        Recovery::Offer { txs: n }
    } else {
        Recovery::Diverged { txs: n }
    }
}

/// Loads `merged` (canonical text) as the document state after an external change to
/// `theirs`, keeping unsaved edits as undo entries replayed onto `theirs`. Returns the
/// number of unsaved transactions that no longer applied.
fn rebase(open: &mut Open, merged: &str, theirs: &[u8]) -> u32 {
    assert!(open.conflict.is_none() && open.tail.is_none());
    let model = parse(theirs).expect("theirs parsed before the merge");
    let mut lineage = Document::from_format(&model).doc;
    let local: Vec<Entry> = match open.saved_depth {
        Some(d) if d <= open.undo.len() => open.undo.iter().skip(d).cloned().collect(),
        _ => {
            let base = Document::from_format(&parse(&open.disk).expect("base parsed")).doc;
            let forward = edit::diff_ops(&base, &open.doc);
            vec![Entry {
                seq: 0,
                unix_ms: 0,
                label: "unsaved edits".into(),
                forward,
                inverse: Vec::new(),
            }]
        }
    };
    let mut kept = Vec::new();
    let mut dropped = 0;
    for mut e in local {
        match edit::replay(&mut lineage, &mut e.forward) {
            Ok(inv) => {
                e.inverse = inv;
                kept.push(e);
            }
            Err(_) => dropped += 1,
        }
    }
    let merged_doc =
        Document::from_format(&parse(merged.as_bytes()).expect("merge output parses")).doc;
    if lineage.canonical() != merged_doc.canonical() {
        let mut forward = edit::diff_ops(&lineage, &merged_doc);
        let inverse =
            edit::restore(&mut lineage, &mut forward).expect("a diff applies to its source");
        kept.push(Entry {
            seq: 0,
            unix_ms: 0,
            label: "merge external changes".into(),
            forward,
            inverse,
        });
    }
    for e in &mut kept {
        e.seq = open.next_seq;
        open.next_seq += 1;
    }
    open.doc = merged_doc;
    open.undo = kept.into();
    open.redo.clear();
    open.stages.clear();
    open.saved_depth = Some(0);
    open.disk = theirs.to_vec();
    open.disk_hash = None;
    rewrite_journal(open);
    assert!(
        open.undo
            .iter()
            .zip(open.undo.iter().skip(1))
            .all(|(a, b)| a.seq < b.seq)
    );
    dropped
}

/// Splits merge output with conflict markers into the text around the blocks and the
/// blocks' two sides.
fn split_conflicts(text: &str) -> (Vec<String>, Vec<ConflictBlock>) {
    let (mut pieces, mut blocks) = (vec![String::new()], Vec::new());
    let mut side = 0u8; // 0 outside, 1 ours, 2 theirs
    let (mut ours, mut theirs) = (String::new(), String::new());
    for line in text.split_inclusive('\n') {
        match (side, line) {
            (0, "<<<<<<< ours\n") => side = 1,
            (1, "=======\n") => side = 2,
            (2, ">>>>>>> theirs\n") => {
                blocks.push(ConflictBlock {
                    ours: std::mem::take(&mut ours),
                    theirs: std::mem::take(&mut theirs),
                });
                pieces.push(String::new());
                side = 0;
            }
            (0, l) => pieces.last_mut().expect("starts with one").push_str(l),
            (1, l) => ours.push_str(l),
            (_, l) => theirs.push_str(l),
        }
    }
    assert_eq!(side, 0, "merge output closes every conflict");
    assert_eq!(pieces.len(), blocks.len() + 1);
    (pieces, blocks)
}
