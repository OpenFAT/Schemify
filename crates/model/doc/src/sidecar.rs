//! Committed edits of open documents → the bytes the app appends to each journal
//! sidecar, and the bounded idle-frame work commits leave behind (disk hashes,
//! compaction, the first journal's history).
//! Cites: edits spec §3 (journal, `base` checkpoint, compaction), D39 (idle work off the
//! edit frame).
//!
//! Contents:
//! - `pub struct JournalWrite`; `pub(crate) struct JournalOut`, `pub(crate) struct LastTx`,
//!   `pub(crate) struct Prelude`, `pub(crate) enum DiskHash`: `unhashed`;
//!   `pub(crate) const COMPACT_OVERDUE`
//! - `Project`: `take_journal`, `idle_step`, `idle_pending`, `settle_journal`,
//!   `hash_pending`
//! - `pub(crate) fn disk_hash`, `pub(crate) fn rehash`, `pub(crate) fn settle`,
//!   `pub(crate) fn append_journal`, `pub(crate) fn rewrite_journal`,
//!   `pub(crate) fn rewrite_journal_lazy`

use crate::journal::{self, Entry, JournalRecord};
use crate::open::{DocHandle, Open};
use crate::optext::JOURNAL_HEADER;
use crate::project::Project;
use crate::sha256::{self, Sha256};
use std::borrow::Borrow;
use std::mem;

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

#[derive(Debug, Default)]
pub(crate) struct JournalOut {
    /// The journal file exists and holds this document's lineage.
    pub(crate) started: bool,
    pub(crate) rewrite: bool,
    pub(crate) text: String,
    pub(crate) txs: u64,
    pub(crate) bytes: u64,
    /// The last transaction record appended and not yet taken: [`Edit::Extend`]
    /// rewrites it in place.
    pub(crate) last_tx: Option<LastTx>,
    /// `Some` while `base` names [`UNHASHED`] (the journal started before the disk hash was
    /// taken): the text taken since, sent again whole once the hash lands ([`settle`]).
    pub(crate) unhashed: Option<String>,
    /// `hist` records of the bottom undo entries, encoded in idle steps ahead of the first
    /// commit after a document's first save, so that commit's journal does not encode the
    /// whole history in its frame.
    pub(crate) prelude: Prelude,
}

/// Where the last appended transaction record sits in [`JournalOut::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastTx {
    pub(crate) seq: u64,
    /// Byte offset of the record in the journal text.
    pub(crate) offset: usize,
}

/// The journal prelude: `hist` records of the first `entries` undo entries.
#[derive(Debug, Default)]
pub(crate) struct Prelude {
    pub(crate) text: String,
    /// Undo entries encoded; 0 when the prelude is stale or empty.
    pub(crate) entries: usize,
    /// Seq of the last entry encoded, to tell whether it still sits at `entries - 1`.
    pub(crate) last_seq: u64,
}

/// How far the sha256 of an open document's disk bytes is.
#[derive(Debug)]
pub(crate) enum DiskHash {
    /// The first `absorbed` bytes are in `hasher`.
    Partial { hasher: Sha256, absorbed: usize },
    /// The hex digest.
    Done(String),
}

/// Garbage-to-live ratio at which a commit compacts in its own frame
/// ([`Document::compact_due`]); idle steps compact at 1.
pub(crate) const COMPACT_OVERDUE: usize = 3;

/// What `base` names until the disk hash is taken: no sha256 hex, so it matches no file,
/// and a crash before [`settle`] recovers the journal as a diverged tail (edits spec §3).
const UNHASHED: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

impl DiskHash {
    pub(crate) fn unhashed() -> DiskHash {
        DiskHash::Partial {
            hasher: Sha256::default(),
            absorbed: 0,
        }
    }
}

impl Project {
    /// Takes the journal bytes produced since the last call (`None` when there are
    /// none). The app appends them (or replaces the file when `rewrite`), batching
    /// fsyncs at ≤ 100 ms. A journal that started before the disk hash was known names a
    /// placeholder `base` until an idle step (or [`Project::settle_journal`]) takes the
    /// hash; its next write is then the whole journal again.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a rewrite always starts with the journal header.
    pub fn take_journal(&mut self, h: DocHandle) -> Option<JournalWrite> {
        let open = self.open_mut(h)?;
        if open.journal.text.is_empty() {
            return None;
        }
        open.journal.last_tx = None;
        if let Some(taken) = &mut open.journal.unhashed {
            taken.push_str(&open.journal.text);
        }
        let w = JournalWrite {
            rewrite: mem::take(&mut open.journal.rewrite),
            text: mem::take(&mut open.journal.text),
        };
        assert!(!w.rewrite || w.text.starts_with(JOURNAL_HEADER));
        assert!(open.journal.text.is_empty(), "taken bytes are sent once");
        Some(w)
    }

    /// Does one bounded step of the work commits leave to idle frames (edits spec §3, D39):
    /// `budget` bytes of a disk hash (the active document's ahead of its first commit, or
    /// any journal's whose `base` waits for it), a compaction once garbage outweighs live
    /// data, or `budget` bytes of a first journal's `hist` records. The active document
    /// goes first. Returns the document whose tables were rebuilt (a compaction: its
    /// change log broke, so derived tables rebuild), else `None`.
    ///
    /// Cost: O(`budget`), or O(document) for a compaction (~100 ms at 100k objects).
    ///
    /// # Panics
    ///
    /// Panics if `budget` is 0 (a step that does nothing would never finish the work).
    pub fn idle_step(&mut self, active: Option<DocHandle>, budget: usize) -> Option<DocHandle> {
        assert!(budget > 0, "a step does something");
        let h = self.idle_next(active)?;
        let open = self.open_mut(h).expect("`idle_next` returns live handles");
        if idle_hash(open, Some(h) == active) {
            let DiskHash::Partial { hasher, absorbed } = &mut open.disk_hash else {
                unreachable!("`idle_hash` holds only while the hash is partial")
            };
            let end = (*absorbed + budget).min(open.disk.len());
            assert!(end > *absorbed, "a pending hash has bytes left");
            hasher.update(&open.disk[*absorbed..end]);
            *absorbed = end;
            if end == open.disk.len() {
                disk_hash(open);
                settle(open);
            }
            return None;
        }
        if open.doc.compact_due(1) {
            open.doc.compact();
            return Some(h);
        }
        prelude_step(open, budget);
        None
    }

    /// Returns whether [`Project::idle_step`] has work left, with `active` the document on
    /// screen (only its disk hash is taken ahead of need). Cost: O(open documents).
    #[must_use]
    pub fn idle_pending(&self, active: Option<DocHandle>) -> bool {
        self.idle_next(active).is_some()
    }

    /// Finishes the disk hash a journal's `base` waits for, in this frame, so the next
    /// [`Project::take_journal`] names it: before the app quits. Cost: O(file) once.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: settling leaves no placeholder `base` behind.
    pub fn settle_journal(&mut self, h: DocHandle) {
        if let Some(open) = self.open_mut(h) {
            settle(open);
            assert!(open.journal.unhashed.is_none());
        }
    }

    /// Returns whether document `h`'s disk bytes are not hashed yet (a read-only
    /// document and one without disk bytes have nothing to hash).
    #[must_use]
    pub fn hash_pending(&self, h: DocHandle) -> bool {
        self.open_ref(h).is_some_and(|o| {
            matches!(o.disk_hash, DiskHash::Partial { .. })
                && !o.disk.is_empty()
                && !o.doc.read_only()
        })
    }
}

/// Returns the sha256 hex of the disk bytes, hashing them (the rest of them, after
/// [`Project::idle_step`]) on first use.
pub(crate) fn disk_hash(open: &mut Open) -> String {
    if let DiskHash::Partial { hasher, absorbed } = &mut open.disk_hash {
        assert!(*absorbed <= open.disk.len());
        hasher.update(&open.disk[*absorbed..]);
        let done = sha256::hex(&mem::take(hasher).finish());
        open.disk_hash = DiskHash::Done(done);
    }
    let DiskHash::Done(hash) = &open.disk_hash else {
        unreachable!("hashed above")
    };
    assert_eq!(hash.len(), 64);
    hash.clone()
}

/// Forgets the disk hash (the disk bytes changed).
pub(crate) fn rehash(open: &mut Open) {
    assert!(
        open.journal.unhashed.is_none(),
        "a waiting base is settled first"
    );
    open.disk_hash = DiskHash::unhashed();
}

/// Puts the disk hash into a journal whose `base` names [`UNHASHED`] and sends the
/// journal so far again, whole, as a rewrite. Hashes the rest of the file if a step did
/// not (a save or a quit came first). Cost: O(journal since its start).
pub(crate) fn settle(open: &mut Open) {
    if open.journal.unhashed.is_none() {
        return;
    }
    let hash = disk_hash(open);
    let j = &mut open.journal;
    let Some(mut text) = j.unhashed.take() else {
        unreachable!("checked above")
    };
    let taken = text.len();
    text.push_str(&j.text);
    let at = JOURNAL_HEADER.len() + "base ".len();
    assert_eq!(&text[at..at + UNHASHED.len()], UNHASHED, "base comes first");
    assert_eq!(
        hash.len(),
        UNHASHED.len(),
        "the digest replaces the placeholder in place"
    );
    text.replace_range(at..at + UNHASHED.len(), &hash);
    if let Some(last) = &mut j.last_tx {
        last.offset += taken;
    }
    j.text = text;
    j.rewrite = true;
    assert!(j.unhashed.is_none());
    assert!(j.text.starts_with(JOURNAL_HEADER));
}

pub(crate) fn append_journal<E: Borrow<Entry>>(
    journal: &mut JournalOut,
    recs: &[JournalRecord<E>],
) {
    if !journal.started {
        return; // nothing committed yet: the journal starts with the first commit
    }
    let start = journal.text.len();
    journal::encode_records(&mut journal.text, recs);
    assert!(recs.is_empty() || journal.text.ends_with('\n'));
    journal.txs += recs
        .iter()
        .filter(|r| matches!(r, JournalRecord::Tx(_)))
        .count() as u64;
    journal.bytes += (journal.text.len() - start) as u64;
}

/// Replaces the journal with one describing the current lineage: `base` (the disk
/// bytes), the undo entries up to the saved depth as history, the rest as
/// transactions to replay. Redo history is not kept across a rewrite. Hashes the disk
/// bytes in this frame (a save, reload, merge or recovery: O(file) already).
pub(crate) fn rewrite_journal(open: &mut Open) {
    disk_hash(open);
    rewrite_journal_lazy(open);
}

/// [`rewrite_journal`] in a commit's frame: a `base` whose hash idle steps have not taken
/// yet names [`UNHASHED`] until [`settle`].
pub(crate) fn rewrite_journal_lazy(open: &mut Open) {
    // Every rewrite happens where the disk state is on the undo stack (open, save,
    // reload, merge, recovery), so replaying from `base` reproduces the state.
    let depth = open
        .saved_depth
        .expect("rewrites happen at a reachable disk state");
    assert!(depth <= open.undo.len());
    // The disk hash is taken in idle steps: until then `base` names a placeholder.
    let base = match &open.disk_hash {
        DiskHash::Done(hash) => hash.clone(),
        DiskHash::Partial { .. } => UNHASHED.to_owned(),
    };
    let unhashed = (base == UNHASHED).then(String::new);
    let mut text = JOURNAL_HEADER.to_owned();
    journal::encode_records(&mut text, &[JournalRecord::<&Entry>::Base(base)]);
    // The history below the saved state, as far as idle steps encoded it ahead.
    let n = Some(prelude_len(open)).filter(|&n| n <= depth).unwrap_or(0);
    if n > 0 {
        join(&mut text, &open.journal.prelude.text);
    }
    let mut recs: Vec<JournalRecord<&Entry>> =
        open.undo.range(n..depth).map(JournalRecord::Hist).collect();
    recs.extend(open.undo.range(depth..).map(JournalRecord::Tx));
    assert_eq!(
        n + recs.len(),
        open.undo.len(),
        "every undo entry is journaled once"
    );
    let mut rest = String::new();
    journal::encode_records(&mut rest, &recs);
    join(&mut text, &rest);
    open.journal = JournalOut {
        started: true,
        rewrite: true,
        txs: open.undo.len() as u64,
        bytes: text.len() as u64,
        text,
        last_tx: None,
        unhashed,
        prelude: Prelude::default(),
    };
    open.redo.clear();
    assert!(open.journal.text.starts_with(JOURNAL_HEADER));
}

// ---- private ----

impl Project {
    /// The document [`Project::idle_step`] works on next: the active one first.
    fn idle_next(&self, active: Option<DocHandle>) -> Option<DocHandle> {
        let due = |h: DocHandle| {
            self.open_ref(h)
                .is_some_and(|o| idle_hash(o, Some(h) == active) || idle_other(o))
        };
        active
            .filter(|&h| due(h))
            .or_else(|| self.handles().find(|&h| due(h)))
    }
}

/// Whether an idle step hashes `open`'s disk bytes: a journal waits for them, or the
/// document is on screen and may be edited next.
fn idle_hash(open: &Open, active: bool) -> bool {
    let partial = matches!(open.disk_hash, DiskHash::Partial { .. }) && !open.disk.is_empty();
    partial && (open.journal.unhashed.is_some() || active && !open.doc.read_only())
}

/// Whether an idle step has other work on `open`: a compaction or a first journal's
/// history to encode.
fn idle_other(open: &Open) -> bool {
    let first = !open.journal.started && !open.disk.is_empty() && !open.doc.read_only();
    let hist = open
        .saved_depth
        .is_some_and(|d| d <= open.undo.len() && prelude_len(open) < d);
    open.doc.compact_due(1) || first && hist
}

/// The number of undo entries the journal prelude holds, 0 when it no longer describes
/// the bottom of the undo stack (entries dropped below it, or it was never encoded).
fn prelude_len(open: &Open) -> usize {
    let Prelude {
        entries, last_seq, ..
    } = open.journal.prelude;
    let valid = entries > 0 && entries <= open.undo.len() && open.undo[entries - 1].seq == last_seq;
    let len = if valid { entries } else { 0 };
    assert!(len <= open.undo.len(), "the prelude describes undo entries");
    len
}

/// Encodes about `budget` bytes more of the journal prelude: `hist` records of the undo
/// entries below the saved state, joined as one encoding of them would be.
fn prelude_step(open: &mut Open, budget: usize) {
    assert!(budget > 0);
    let depth = open.saved_depth.expect("idle_other checked a saved state");
    assert!(
        depth <= open.undo.len(),
        "idle_other checked a reachable saved state"
    );
    let mut n = prelude_len(open);
    let text = &mut open.journal.prelude.text;
    if n == 0 {
        text.clear();
    }
    let start = text.len();
    let mut chunk = String::new();
    while n < depth && text.len() - start < budget {
        chunk.clear();
        journal::encode_records(&mut chunk, &[JournalRecord::Hist(&open.undo[n])]);
        join(text, &chunk);
        n += 1;
    }
    assert!(n > 0, "idle_other checked an entry is left to encode");
    assert!(n <= depth);
    open.journal.prelude.entries = n;
    open.journal.prelude.last_seq = open.undo[n - 1].seq;
}

/// Appends separately encoded records to `text` as one encoding writes them: a blank
/// line between records of different words, and every join here is one (`base`,
/// `inverse` ends, `hist` or `tx` starts).
fn join(text: &mut String, chunk: &str) {
    if !text.is_empty() && !chunk.is_empty() {
        text.push('\n');
    }
    text.push_str(chunk);
}
