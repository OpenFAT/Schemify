//! Edit requests (the frame spine's `edit_requests` table) → applied transactions,
//! stages, undo and redo on open documents, with the outcome of each and the undo
//! entries that record them.
//! Cites: edits spec §1 (transactions, modes, extend), §2 (undo/redo, stages); D13
//! (parameter semantics of instance edits), D20 (cache write-back via extend).
//!
//! Contents:
//! - `pub const UNDO_MAX`, `pub(crate) const STAGES_MAX`
//! - `pub struct Stage`, `pub enum Edit`, `pub struct EditRequest`, `pub enum Outcome`
//! - `pub fn apply_edits(project: &mut Project, requests: &[EditRequest]) -> Vec<Outcome>`
//! - `Project`: `applied`, `stages`, `preview`
//! - `pub(crate) fn push_commit`, `pub(crate) fn undo_top`, `pub(crate) fn redo_top`

use crate::Code;
use crate::document::Document;
use crate::edit;
use crate::journal::{Entry, JournalRecord};
use crate::open::{self, DocHandle, Open};
use crate::ops::{Mode, Op, OpError, Transaction};
use crate::project::Project;
use crate::sidecar::{self, COMPACT_OVERDUE, LastTx};
use crate::snap::{ParamFacts, ParamMap};
use schemify_format::{FileKind, RecordKind};

/// Undo (and redo) stack capacity per document; the oldest entries drop first.
pub const UNDO_MAX: usize = 1 << 16;

/// Open stages per document.
pub(crate) const STAGES_MAX: usize = 1 << 8;

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

/// A document after [`Project::apply_ops`], with the ops as applied and their inverse.
struct Applied<'project> {
    open: &'project mut Open,
    /// The ops as applied (fresh ids drawn, values snapped).
    ops: Vec<Op>,
    /// Ops that undo `ops`, in application order.
    inverse: Vec<Op>,
}

/// Applies edit requests in order (the frame spine's `doc::apply_edits`): transactions
/// (commit or stage), undo, redo, accept and reject. Each request is all-or-nothing;
/// a failing request does not stop the ones after it. Journal output accumulates per
/// document for [`Project::take_journal`]; [`Project::applied`] reads back the ops a
/// request applied, without copying them.
///
/// # Panics
///
/// Only on a bug in this crate: one outcome per request, and a committed stage's document
/// is still open.
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
                            let before = open.stages.len();
                            open.stages.retain(|s| s.id != *id);
                            assert_eq!(open.stages.len() + 1, before, "stage ids are unique");
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
                Some(open) if open::refused(open) => Outcome::Refused(Code::E044),
                Some(open) if req.edit == Edit::Undo => match undo_top(open) {
                    Ok(Some(seq)) => {
                        let undone = [JournalRecord::<Entry>::Undo(seq)];
                        sidecar::append_journal(&mut open.journal, &undone);
                        Outcome::Undone { seq }
                    }
                    Ok(None) => Outcome::Nothing,
                    Err(e) => Outcome::Rejected(e),
                },
                Some(open) => match redo_top(open) {
                    Ok(Some(seq)) => {
                        let redone = [JournalRecord::<Entry>::Redo(seq)];
                        sidecar::append_journal(&mut open.journal, &redone);
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

impl Project {
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
    #[must_use]
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

    /// Returns the open stages (the `staged_edits` table) of a document.
    #[must_use]
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
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: a replay returns one inverse per op.
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
}

/// The stack side of a commit: clears redo, pushes undo.
pub(crate) fn push_commit(open: &mut Open, entry: Entry) {
    let seq = entry.seq;
    if open.saved_depth.is_some_and(|d| d > open.undo.len()) {
        open.saved_depth = None; // the saved state was in redo history, now gone
    }
    open.redo.clear();
    push_undo(open, entry);
    assert!(open.undo.back().is_some_and(|e| e.seq == seq));
    assert!(open.redo.is_empty(), "a commit clears redo");
}

pub(crate) fn undo_top(open: &mut Open) -> Result<Option<u64>, Vec<OpError>> {
    let Some(mut entry) = open.undo.pop_back() else {
        return Ok(None);
    };
    let mut ops = entry.inverse.clone();
    if let Err(e) = edit::restore(&mut open.doc, &mut ops) {
        open.undo.push_back(entry);
        return Err(e);
    }
    let seq = entry.seq;
    assert_eq!(
        ops.len(),
        entry.forward.len(),
        "one op restored per op undone"
    );
    entry.inverse = ops;
    open.redo.push(entry);
    open.journal.prelude.entries = 0; // a redo brings the entry back with a new inverse
    // Entries only move between the stacks, and a commit clears redo: their sum is
    // bounded by the undo cap, so redo needs no cap of its own.
    assert!(open.undo.len() + open.redo.len() <= UNDO_MAX);
    assert!(open.redo.last().is_some_and(|e| e.seq == seq));
    Ok(Some(seq))
}

pub(crate) fn redo_top(open: &mut Open) -> Result<Option<u64>, Vec<OpError>> {
    let Some(mut entry) = open.redo.pop() else {
        return Ok(None);
    };
    let mut ops = entry.forward.clone();
    match edit::restore(&mut open.doc, &mut ops) {
        Ok(inv) => {
            let seq = entry.seq;
            assert_eq!(inv.len(), entry.forward.len(), "one inverse per op redone");
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

// ---- private ----

impl Project {
    fn tx(&mut self, h: DocHandle, tx: &Transaction, unix_ms: u64) -> Outcome {
        let Some(open) = self.open_ref(h) else {
            return Outcome::Refused(Code::E040);
        };
        if open::refused(open) {
            return Outcome::Refused(Code::E044);
        }
        let Applied { open, ops, inverse } = match self.apply_ops(h, &tx.ops) {
            Ok(applied) => applied,
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

    /// Applies `ops` to live, unrefused document `h` with the parameter facts of the cells
    /// they instantiate. Returns the document, the ops as applied and their inverse.
    fn apply_ops(&mut self, h: DocHandle, ops: &[Op]) -> Result<Applied<'_>, Vec<OpError>> {
        let open = self.open_ref(h).expect("checked live");
        assert!(!open::refused(open), "checked by the caller");
        let params = self.param_map(open, ops);
        let mut ops = ops.to_vec();
        let Project { slots, rng, .. } = self;
        let open = slots[h.index as usize]
            .open
            .as_deref_mut()
            .expect("checked live");
        let inverse = edit::apply(&mut open.doc, &mut ops, &params, rng)?;
        assert_eq!(inverse.len(), ops.len());
        Ok(Applied { open, ops, inverse })
    }

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
        let pending = open.journal.last_tx.is_some_and(|last| last.seq == seq);
        let rewritable = pending || !open.journal.started || open.saved_depth.is_some();
        let saved = open.saved_depth.is_some_and(|d| d >= open.undo.len());
        if open::refused(open) || saved || !rewritable {
            return Outcome::Refused(Code::E044);
        }
        let Applied {
            open,
            ops,
            mut inverse,
        } = match self.apply_ops(h, ops) {
            Ok(applied) => applied,
            Err(errors) => return Outcome::Rejected(errors),
        };
        let entry = open.undo.back_mut().expect("checked above");
        entry.forward.extend(ops);
        // Undo reverts the extension first: it was applied last.
        inverse.append(&mut entry.inverse);
        entry.inverse = inverse;
        match open.journal.last_tx {
            Some(last) if last.seq == seq => {
                let j = &mut open.journal;
                assert!(
                    last.offset <= j.text.len(),
                    "the record is still in the pending text"
                );
                j.bytes -= (j.text.len() - last.offset) as u64;
                j.txs -= 1;
                j.text.truncate(last.offset);
                let entry = open.undo.back().expect("checked above");
                sidecar::append_journal(&mut open.journal, &[JournalRecord::Tx(entry)]);
                open.journal.last_tx = Some(last);
            }
            _ if open.journal.started => sidecar::rewrite_journal_lazy(open),
            _ => {}
        }
        if open.doc.compact_due(COMPACT_OVERDUE) {
            open.doc.compact();
        }
        assert!(open.undo.back().is_some_and(|e| e.seq == seq));
        assert!(open.redo.is_empty(), "extend requires an empty redo stack");
        Outcome::Committed { seq }
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
}

fn param_facts(interface: &Document) -> Vec<ParamFacts> {
    assert!(
        matches!(interface.kind(), FileKind::Shprim | FileKind::Shcomp),
        "benches have no params"
    );
    let s = interface.strings();
    let p = interface.params();
    let pool = interface.pool();
    let first = |row: usize, key: &str| {
        pool.find_attr(s, p.attrs[row], key)
            .and_then(|v| v.first())
            .map(|v| s.get(*v).to_owned())
    };
    let facts: Vec<ParamFacts> = (0..p.kind.len())
        .map(|row| ParamFacts {
            name: s.get(p.fields[row][0]).to_owned(),
            computed: pool.find_attr(s, p.attrs[row], "computed").is_some(),
            grid: first(row, "grid"),
            step: first(row, "step"),
            min: first(row, "min"),
        })
        .collect();
    assert_eq!(facts.len(), p.kind.len(), "one fact row per param");
    facts
}

fn stage(open: &mut Open, label: &str, ops: Vec<Op>) -> Outcome {
    if open.stages.len() >= STAGES_MAX {
        return Outcome::Refused(Code::E043);
    }
    // Bounded: fewer than `STAGES_MAX` ids are taken, so a free one is that close.
    let mut id = open.next_stage;
    for _ in 0..STAGES_MAX {
        if !open.stages.iter().any(|s| s.id == id) {
            break;
        }
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
    assert_eq!(forward.len(), inverse.len(), "one inverse per op");
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
    // (`ponytail:` a never-saved document has no journal until its first save), at a
    // reachable saved state: below it (undone past the save) this commit drops the redo
    // entry the save sat on, and the journal waits for the next save.
    let reachable = open.saved_depth.is_some_and(|d| d <= open.undo.len());
    if !open.journal.started && !open.disk.is_empty() && reachable {
        sidecar::rewrite_journal_lazy(open);
    }
    let offset = open.journal.text.len();
    sidecar::append_journal(&mut open.journal, &[JournalRecord::Tx(&entry)]);
    open.journal.last_tx = open.journal.started.then_some(LastTx { seq, offset });
    push_commit(open, entry);
    // Idle steps compact (`Project::idle_step`); a caller that never idles still bounds
    // the garbage here.
    if open.doc.compact_due(COMPACT_OVERDUE) {
        open.doc.compact();
    }
    assert!(open.redo.is_empty());
    seq
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
