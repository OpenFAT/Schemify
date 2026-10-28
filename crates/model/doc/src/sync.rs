//! The disk side of an open document: a save → canonical bytes, the app's confirmation →
//! a new merge base; bytes changed outside Schemify → reload, a three-way merge, or
//! conflict blocks to resolve; a journal tail → replayed or discarded.
//! Cites: edits spec §3 (recovery), §4 (external changes, merge3), §5 (saves).
//!
//! Contents:
//! - `pub struct SaveRequest`, `pub enum External`, `pub struct ConflictBlock`,
//!   `pub enum Choice`; `pub(crate) struct PendingSave`, `pub(crate) struct Conflict`
//! - `Project`: `recover`, `discard_recovery`, `save`, `saved`, `external_change`,
//!   `conflicts`, `resolve_conflicts`

use crate::Code;
use crate::Diag;
use crate::diff::diff_ops;
use crate::document::Document;
use crate::edit;
use crate::journal::{Entry, JRec};
use crate::open::{DocHandle, Open, OpenError, dirty, refused};
use crate::project::Project;
use crate::requests::{push_commit, redo_top, undo_top};
use crate::sidecar::{append_journal, disk_hash, rehash, rewrite_journal, settle};
use schemify_format::{ParseError, merge3, parse};

/// Canonical bytes to write for a save. Hand them back to `Project::saved` once they
/// are on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    /// File contents (format spec §2 canonical form).
    pub bytes: String,
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

/// What `save` remembers until `saved`: which undo depth the bytes are.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingSave {
    pub(crate) len: usize,
    pub(crate) depth: usize,
    pub(crate) top_seq: u64,
}

#[derive(Debug)]
pub(crate) struct Conflict {
    /// Text around the blocks: `pieces.len() == blocks.len() + 1`.
    pub(crate) pieces: Vec<String>,
    pub(crate) blocks: Vec<ConflictBlock>,
    pub(crate) theirs: Vec<u8>,
}

/// Journal compaction thresholds (edits spec §3) and what a compaction keeps.
const COMPACT_TXS: u64 = 1 << 16;

const COMPACT_BYTES: u64 = 64 << 20;

const COMPACT_KEEP: usize = 1 << 12;

impl Project {
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

    /// Returns the canonical bytes to write (edits spec §5); `None` when not dirty.
    /// The app writes them atomically, then calls [`Project::saved`].
    ///
    /// # Errors
    ///
    /// `E040` stale handle; `E044` read-only, in conflict or pending recovery; `E025`
    /// when the kind rules fail (the file would not parse).
    pub fn save(&mut self, h: DocHandle) -> Result<Option<SaveRequest>, Code> {
        let open = self.open_mut(h).ok_or(Code::E040)?;
        if refused(open) {
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
        settle(open); // the journal's `base` names the bytes being replaced
        open.disk = written.into_bytes();
        rehash(open);
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
            let rec = JRec::<Entry>::Save(p.top_seq, disk_hash(open));
            append_journal(&mut open.journal, &[rec]);
        } else if open.journal.started {
            // Edits landed between `save` and `saved`: the journal's position no longer
            // matches the saved state, so restate the lineage from the new bytes.
            rewrite_journal(open);
        }
        assert!(open.pending_save.is_none());
        Ok(())
    }

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
            open.journal.unhashed = None; // a started journal is rewritten below
            rehash(open);
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
}

// ---- private ----

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
            let forward = diff_ops(&base, &open.doc);
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
        let mut forward = diff_ops(&lineage, &merged_doc);
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
    open.journal.unhashed = None; // rewritten below
    rehash(open);
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
