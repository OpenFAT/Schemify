//! A cell file's bytes (and journal) → an open document slot of the project, and the
//! per-document reads: tables, dirtiness, history depth, override canonicalization.
//! Cites: edits spec §1–§3 (open, journal history and recovery offer), D40 (overrides
//! equal to defaults).
//!
//! Per open document (`Open`): the tables, the undo/redo stacks (capacity 2^16 each,
//! independent of hierarchy navigation), staged edits (≤ 2^8 open stages), the pending
//! journal output, and the last loaded/saved bytes (the merge base `O`).
//!
//! Contents:
//! - `pub const DOCS_MAX`, `pub struct DocHandle`, `pub struct Opened`,
//!   `pub enum OpenError`, `pub enum Recovery`
//! - `pub(crate) struct Open`: `new`; `pub(crate) struct Slot`
//! - `Project`: `open`, `open_indexed`, `create`, `close`, `doc`, `handles`, `is_dirty`,
//!   `history_len`, `undo_labels`, `redundant_overrides`, `canonicalize_overrides`;
//!   crate: `open_ref`, `open_mut`
//! - `pub(crate) fn dirty(open: &Open) -> bool`, `pub(crate) fn refused(open: &Open) -> bool`

use crate::document::Document;
use crate::journal::{self, Analysis, Entry, JournalRecord};
use crate::ops::{Mode, Op, Target, Transaction};
use crate::project::Project;
use crate::requests::{Stage, UNDO_MAX};
use crate::sha256;
use crate::sidecar::{self, DiskHash, JournalOut};
use crate::strings::{ObjId, Sym};
use crate::sync::{Conflict, PendingSave};
use crate::tables::Instances;
use crate::{Code, Diag};
use schemify_format::{FileKind, ParseError, RecordKind};
use std::collections::VecDeque;

/// Open documents per project.
pub const DOCS_MAX: usize = 1 << 16;

/// Handle of an open document: slot index plus generation, so a handle kept across a
/// `close` is detected as stale instead of reaching another document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocHandle {
    /// Slot index.
    pub index: u32,
    /// Generation of the slot when the handle was issued.
    pub generation: u32,
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

#[derive(Debug)]
pub(crate) struct Open {
    pub(crate) lib: u8,
    pub(crate) dir: Sym,
    pub(crate) stem: Sym,
    pub(crate) doc: Document,
    pub(crate) undo: VecDeque<Entry>,
    pub(crate) redo: Vec<Entry>,
    pub(crate) next_seq: u64,
    /// Undo depth at which the state equals `disk`; `None` when unreachable.
    pub(crate) saved_depth: Option<usize>,
    pub(crate) stages: Vec<Stage>,
    pub(crate) next_stage: u16,
    pub(crate) journal: JournalOut,
    pub(crate) disk: Vec<u8>,
    /// sha256 of `disk`, computed on first need (journaling only): hashing a 6 MB file
    /// costs more than loading it, so the editor spreads it over idle frames
    /// ([`Project::idle_step`]) before the first commit needs it.
    pub(crate) disk_hash: DiskHash,
    pub(crate) pending_save: Option<PendingSave>,
    pub(crate) conflict: Option<Conflict>,
    /// Journal records after the checkpoint, awaiting `recover`/`discard_recovery`.
    pub(crate) tail: Option<Vec<JournalRecord>>,
    /// Load changed the model (duplicate ids re-assigned): dirty until saved. Replay
    /// from the journal's `base` reproduces it, because re-iding is deterministic.
    pub(crate) fixups: bool,
}

/// Where the bytes handed to `open_with` come from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bytes {
    /// The very buffer the library index hashed: its digest is the index's.
    Indexed,
    /// Any other read of the file: hashed when needed.
    Read,
}

#[derive(Debug, Default)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) open: Option<Box<Open>>,
}

impl Open {
    /// A document with no history and no disk state (a new, unsaved cell); `open`
    /// overrides the disk fields.
    pub(crate) fn new(lib: u8, dir: Sym, stem: Sym, doc: Document) -> Open {
        assert!(
            matches!(
                doc.kind(),
                FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
            ),
            "open documents are cell files"
        );
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
            disk_hash: DiskHash::unhashed(),
            pending_save: None,
            conflict: None,
            tail: None,
            fixups: false,
        }
    }
}

impl Project {
    /// Opens a cell file of library `lib` (`""` for the project's own files) at
    /// `relpath` from its bytes and, when the sidecar exists, its journal bytes.
    ///
    /// The journal's history up to the checkpoint matching the file becomes the undo
    /// history; edits after it are offered as [`Recovery`] and must be recovered or
    /// discarded before the document accepts edits.
    ///
    /// The bytes are copied as the document's disk state ([`Project::open_indexed`] moves
    /// them).
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
        self.open_with(lib, relpath, bytes.to_vec(), journal, Bytes::Read)
    }

    /// [`Project::open`] for bytes the current index of `lib` was built from (the very
    /// buffer the caller listed to [`Project::index_library`], not a later read of the
    /// file, which may differ): the bytes move in, and the document's disk digest is the
    /// index's, so neither the journal check nor [`Project::idle_step`] hashes the file
    /// again. A file the index does not list is hashed as by `open`. Debug builds check
    /// the digest.
    ///
    /// # Errors
    ///
    /// See [`OpenError`].
    pub fn open_indexed(
        &mut self,
        lib: &str,
        relpath: &str,
        bytes: Vec<u8>,
        journal: Option<&[u8]>,
    ) -> Result<Opened, OpenError> {
        self.open_with(lib, relpath, bytes, journal, Bytes::Indexed)
    }

    /// Creates a new, empty cell document (not yet on disk; dirty until saved).
    ///
    /// # Errors
    ///
    /// `E001` unknown library, `E027` not a cell kind, `E043` too many documents.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a new document is dirty until saved.
    pub fn create(&mut self, lib: &str, relpath: &str, kind: FileKind) -> Result<DocHandle, Code> {
        let lib = self.libs.find(lib).ok_or(Code::E001)? as u8;
        if !matches!(
            kind,
            FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
        ) {
            return Err(Code::E027);
        }
        let path = split_path(relpath);
        let (dir, stem) = (
            self.strings.intern(path.dir),
            self.strings.intern(path.stem),
        );
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
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: the handle goes stale.
    pub fn close(&mut self, h: DocHandle) -> Result<bool, Code> {
        let slot = self.slot_mut(h).ok_or(Code::E040)?;
        let open = slot
            .open
            .take()
            .expect("`slot_mut` returns slots with a document");
        slot.generation = slot.generation.wrapping_add(1);
        assert!(self.open_ref(h).is_none());
        Ok(dirty(&open))
    }

    /// Returns the tables of an open document; `None` for a stale handle.
    #[must_use]
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
    #[must_use]
    pub fn is_dirty(&self, h: DocHandle) -> bool {
        self.open_ref(h).is_some_and(dirty)
    }

    /// Returns (undo depth, redo depth).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: each stack holds at most 2^16 entries.
    #[must_use]
    pub fn history_len(&self, h: DocHandle) -> (usize, usize) {
        let (undo, redo) = self
            .open_ref(h)
            .map_or((0, 0), |o| (o.undo.len(), o.redo.len()));
        assert!(undo <= UNDO_MAX, "the undo stack is bounded");
        assert!(redo <= UNDO_MAX, "the redo stack is bounded");
        (undo, redo)
    }

    /// Returns the labels of the undo stack, oldest first (for undo menus).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: each stack holds at most 2^16 entries.
    #[must_use]
    pub fn undo_labels(&self, h: DocHandle) -> Vec<&str> {
        let labels: Vec<&str> = self.open_ref(h).map_or(Vec::new(), |o| {
            o.undo.iter().map(|e| e.label.as_str()).collect()
        });
        assert!(labels.len() <= UNDO_MAX, "the undo stack is bounded");
        labels
    }

    /// Instance parameter overrides equal (as text) to the cell's default (D40): kept
    /// as the user wrote them, listed for the canonicalizer (`W002`). Sorted by
    /// (instance id, param).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: every listed instance is in the document.
    #[must_use]
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
            let Some(interface) = self.interface(row) else {
                continue;
            };
            let interface_strings = interface.strings();
            for (k, vals) in doc.pool().attrs(doc.instances().attrs[i]) {
                let key = s.get(k);
                if !Instances::is_override(key) || vals.len() != 1 {
                    continue;
                }
                let default = interface
                    .params()
                    .fields
                    .iter()
                    .find(|f| interface_strings.get(f[0]) == key)
                    .map(|f| interface_strings.get(f[1]));
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
    /// Empty for a stale handle.
    ///
    /// # Panics
    ///
    /// Only on a bug in this function: one op per redundant override.
    #[must_use]
    pub fn canonicalize_overrides(&self, h: DocHandle) -> Transaction {
        let redundant = self.redundant_overrides(h);
        let n = redundant.len();
        let ops: Vec<Op> = match self.doc(h) {
            None => Vec::new(),
            Some(doc) => redundant
                .into_iter()
                .map(|(id, key)| Op::RemoveAttr {
                    target: Target::new(RecordKind::Inst, &id.text(doc.strings())),
                    key,
                    first: None,
                })
                .collect(),
        };
        assert_eq!(ops.len(), n);
        Transaction {
            label: "drop overrides equal to defaults".into(),
            mode: Mode::Commit,
            ops,
        }
    }

    pub(crate) fn open_ref(&self, h: DocHandle) -> Option<&Open> {
        assert!(self.slots.len() <= DOCS_MAX);
        let slot = self.slots.get(h.index as usize)?;
        (slot.generation == h.generation)
            .then_some(slot.open.as_deref())
            .flatten()
    }

    pub(crate) fn open_mut(&mut self, h: DocHandle) -> Option<&mut Open> {
        self.slot_mut(h).and_then(|s| s.open.as_deref_mut())
    }
}

/// Returns whether the document differs from its last loaded/saved bytes.
pub(crate) fn dirty(open: &Open) -> bool {
    assert!(
        open.saved_depth
            .is_none_or(|d| d <= open.undo.len() + open.redo.len()),
        "a reachable saved state lies on the undo/redo path"
    );
    open.fixups || open.saved_depth != Some(open.undo.len())
}

pub(crate) fn refused(open: &Open) -> bool {
    open.doc.read_only() || open.conflict.is_some() || open.tail.is_some()
}

// ---- private ----

impl Project {
    fn open_with(
        &mut self,
        lib: &str,
        relpath: &str,
        bytes: Vec<u8>,
        journal: Option<&[u8]>,
        origin: Bytes,
    ) -> Result<Opened, OpenError> {
        let lib = self.libs.find(lib).ok_or(OpenError::Code(Code::E001))? as u8;
        let path = split_path(relpath);
        let (dir, stem) = (
            self.strings.intern(path.dir),
            self.strings.intern(path.stem),
        );
        let model = schemify_format::parse(&bytes).map_err(OpenError::Parse)?;
        if !matches!(
            model.kind(),
            FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
        ) {
            return Err(OpenError::Code(Code::E027));
        }
        if let Some(h) = self.find_open(lib, dir, stem, model.kind()) {
            return Err(OpenError::AlreadyOpen(h));
        }
        let kind = model.kind();
        let loaded = Document::from_format(&model);
        drop(model);
        let reids = loaded.diags.iter().any(|d| d.code == Code::W001);
        let c = &self.cells;
        let row = (0..c.lib.len())
            .find(|&i| (c.lib[i], c.dir[i], c.stem[i], c.kind[i]) == (lib, dir, stem, kind));
        let digest = match row.filter(|_| origin == Bytes::Indexed) {
            Some(row) => {
                debug_assert_eq!(
                    c.hash[row],
                    sha256::sha256(&bytes),
                    "bytes are the indexed bytes"
                );
                DiskHash::Done(sha256::hex(&c.hash[row]))
            }
            None => DiskHash::unhashed(),
        };
        let mut open = Open {
            saved_depth: Some(0),
            disk: bytes,
            disk_hash: digest,
            fixups: reids,
            ..Open::new(lib, dir, stem, loaded.doc)
        };
        let recovery = match journal.map(|j| journal::parse_journal(j, open.doc.kind())) {
            Some(Ok(recs)) => {
                let hash = sidecar::disk_hash(&mut open);
                let analysis = journal::analyze(recs, &hash, UNDO_MAX);
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

    fn alloc(&mut self, open: Open) -> Result<DocHandle, Code> {
        assert!(self.slots.len() <= DOCS_MAX);
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
        assert!(index < DOCS_MAX, "slots are bounded");
        Ok(h)
    }

    fn slot_mut(&mut self, h: DocHandle) -> Option<&mut Slot> {
        assert!(self.slots.len() <= DOCS_MAX);
        self.slots
            .get_mut(h.index as usize)
            .filter(|s| s.generation == h.generation && s.open.is_some())
    }
}

/// A file's directory and stem within its library.
struct FilePath<'path> {
    dir: &'path str,
    stem: &'path str,
}

fn split_path(relpath: &str) -> FilePath<'_> {
    let (dir, file) = relpath.rsplit_once('/').unwrap_or(("", relpath));
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    assert!(!stem.contains('/'));
    assert!(dir.len() + stem.len() <= relpath.len());
    FilePath { dir, stem }
}

/// Installs a journal analysis on a freshly opened document: history if a checkpoint
/// matched, the tail as a pending recovery.
fn install_journal(open: &mut Open, a: Analysis) -> Recovery {
    assert!(open.undo.is_empty(), "installed on a fresh open");
    assert!(open.tail.is_none(), "installed on a fresh open");
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
