//! A transaction's ops on one [`Document`] → applied all-or-nothing, each resolved in
//! place (fresh ids drawn, values snapped) and paired with its exact inverse.
//! Cites: edits spec §1 (transactions, `E025`, `E026`, `E030`, `E040`–`E043`); D13
//! (snapping).
//!
//! Spine of `apply`: note which whole-document rules the ops can affect → apply each
//! op (resolving fresh ids, snapping, building its exact inverse) → end-of-transaction
//! checks (`E026`, `E025`, `E042`) → on any error, replay the inverses and return every
//! (op index, code); otherwise return the inverse transaction.
//!
//! Every modification of an existing record is "read the record, change the owned
//! copy, validate it, put it back", so the inverse of `set_field`/`set_attr`/
//! `remove_attr`/`put` is one `put` of the prior record. That is exact even when the
//! table normalizes (wire endpoint order) or the value was snapped.
//!
//! Contents:
//! - `pub(crate) fn apply(doc, ops, params, rng) -> Result<Vec<Op>, Vec<OpError>>`
//! - `pub(crate) fn replay(doc, ops) -> Result<Vec<Op>, Vec<OpError>>`
//! - `pub(crate) fn restore(doc, ops) -> Result<Vec<Op>, Vec<OpError>>`

use crate::Code;
use crate::document::{Document, Loc};
use crate::ops::{NEW_ID, OPS_MAX, Op, OpError, Target};
use crate::record::{Attr, RecordBuf};
use crate::rows::Dangling;
use crate::schematic::Instances;
use crate::snap::ParamMap;
use crate::strings::ObjId;
use schemify_format::{IdRng, KeyRule, RecordKind};

/// Which override values of an instance an op writes, so only those are snapped:
/// replaying the journal (which has no parameter facts) must reproduce the state.
#[derive(Clone, Copy)]
enum Scope<'k> {
    /// Insert/put: every override is new.
    All,
    /// `set_attr`: only this key.
    Key(&'k str),
    /// The cell reference changed: re-check `E030`, change nothing.
    CheckOnly,
}

/// Which rules a transaction is held to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rules {
    /// Record rules and the end-of-transaction rules (`E025`, `E026`, `E042`): new edits.
    EndOfTransaction,
    /// Record rules only: history being restored.
    RecordsOnly,
}

/// Applies `ops` to `doc` all-or-nothing. Ops are resolved in place (fresh ids drawn,
/// values snapped), so the caller journals exactly what was applied.
///
/// Returns the inverse ops, already in application order (undo applies them as is).
///
/// # Errors
///
/// Every (op index, code) found; `doc` is then unchanged. End-of-transaction checks
/// are attributed to the last op touching the record involved.
pub(crate) fn apply(
    doc: &mut Document,
    ops: &mut [Op],
    params: &ParamMap,
    rng: &mut IdRng,
) -> Result<Vec<Op>, Vec<OpError>> {
    apply_with(doc, ops, params, rng, Rules::EndOfTransaction)
}

/// Re-applies ops that once validated onto possibly different content (journal
/// recovery onto a changed file, local edits rebased onto external bytes): every check
/// runs, no parameter semantics, no id draws. Returns their inverse.
///
/// # Errors
///
/// As [`apply`]: the content no longer admits the ops.
pub(crate) fn replay(doc: &mut Document, ops: &mut [Op]) -> Result<Vec<Op>, Vec<OpError>> {
    apply_with(
        doc,
        ops,
        &ParamMap::new(),
        &mut IdRng::new(0),
        Rules::EndOfTransaction,
    )
}

/// Applies ops that restore a state that existed (undo, redo, a stage rollback, the
/// step from a replay to a merge result): records are still validated, but not the
/// end-of-transaction rules, which guard new user edits, not history.
///
/// # Errors
///
/// Record-level errors only; one means the document changed under the history.
pub(crate) fn restore(doc: &mut Document, ops: &mut [Op]) -> Result<Vec<Op>, Vec<OpError>> {
    apply_with(
        doc,
        ops,
        &ParamMap::new(),
        &mut IdRng::new(0),
        Rules::RecordsOnly,
    )
}

// ---- private ----

fn apply_with(
    doc: &mut Document,
    ops: &mut [Op],
    params: &ParamMap,
    rng: &mut IdRng,
    rules: Rules,
) -> Result<Vec<Op>, Vec<OpError>> {
    assert!(
        !doc.read_only(),
        "callers refuse read-only documents (E044)"
    );
    assert!(ops.len() <= OPS_MAX, "transaction exceeds {OPS_MAX} ops");
    let end_rules = rules == Rules::EndOfTransaction;
    let kind_rules = end_rules && ops.iter().any(|op| touches_kind_rules(op.subject().0));
    let refs = end_rules && ops.iter().any(touches_refs);
    let rules_before = kind_rules && doc.kind_rules_ok();
    // `None` when no op touches a reference: the transaction cannot add a dangling one.
    let dangling_before = refs.then(|| doc.dangling());
    let mut inverse = Vec::with_capacity(ops.len());
    let mut errors = Vec::new();
    for (i, op) in ops.iter_mut().enumerate() {
        match apply_one(doc, op, params, rng) {
            Ok(inv) => {
                // Every inverse addresses the record its op touched.
                assert_eq!(inv.subject().0, op.subject().0, "{op:?} -> {inv:?}");
                inverse.push(inv);
            }
            Err(code) => errors.push(OpError { op: i as u32, code }),
        }
    }
    if end_rules {
        end_checks(
            doc,
            ops,
            rules_before,
            dangling_before.as_deref(),
            &mut errors,
        );
    }
    if !errors.is_empty() {
        undo_partial(doc, inverse);
        errors.sort_by_key(|e| e.op);
        return Err(errors);
    }
    inverse.reverse();
    assert_eq!(inverse.len(), ops.len(), "one inverse per applied op");
    Ok(inverse)
}

fn undo_partial(doc: &mut Document, inverse: Vec<Op>) {
    assert!(!doc.read_only());
    let empty = ParamMap::new();
    let mut rng = IdRng::new(0);
    for mut inv in inverse.into_iter().rev() {
        let r = apply_one(doc, &mut inv, &empty, &mut rng);
        assert!(
            r.is_ok(),
            "the inverse of an applied op always applies: {r:?}"
        );
    }
}

fn touches_kind_rules(kind: RecordKind) -> bool {
    matches!(
        kind,
        RecordKind::Pin | RecordKind::Impl | RecordKind::Header
    )
}

fn touches_refs(op: &Op) -> bool {
    let (kind, _) = op.subject();
    let referencing = matches!(
        kind,
        RecordKind::Waive
            | RecordKind::Constraint
            | RecordKind::Body
            | RecordKind::Spec
            | RecordKind::Impl
            | RecordKind::Output
            | RecordKind::Header
    );
    referencing || matches!(op, Op::Delete(t) if t.kind.key() == KeyRule::Id)
}

fn end_checks(
    doc: &Document,
    ops: &[Op],
    rules_before: bool,
    dangling_before: Option<&[Dangling]>,
    errors: &mut Vec<OpError>,
) {
    fn last(ops: &[Op], pred: impl Fn(RecordKind, &str) -> bool) -> u32 {
        let i = ops.iter().rposition(|op| {
            let (k, key) = op.subject();
            pred(k, key)
        });
        i.unwrap_or(ops.len().saturating_sub(1)) as u32
    }
    let errors_before = errors.len();
    let mut wires: Vec<&str> = ops
        .iter()
        .map(Op::subject)
        .filter(|(k, _)| *k == RecordKind::Wire)
        .map(|(_, key)| key)
        .collect();
    wires.sort_unstable();
    wires.dedup();
    for w in wires {
        if doc
            .locate(RecordKind::Wire, w)
            .is_some_and(|l| doc.wire_zero(l))
        {
            let op = last(ops, |k, key| k == RecordKind::Wire && key == w);
            errors.push(OpError {
                op,
                code: Code::E026,
            });
        }
    }
    if rules_before && !doc.kind_rules_ok() {
        let op = last(ops, |k, _| touches_kind_rules(k));
        errors.push(OpError {
            op,
            code: Code::E025,
        });
    }
    if let Some(before) = dangling_before {
        assert!(before.is_sorted(), "`Document::dangling` sorts");
        for d in doc.dangling() {
            if before.binary_search(&d).is_ok() {
                continue;
            }
            let op = last(ops, |k, x| (k == d.kind && x == d.key) || x == d.target);
            errors.push(OpError {
                op,
                code: Code::E042,
            });
        }
    }
    assert!(
        errors[errors_before..]
            .iter()
            .all(|e| (e.op as usize) < ops.len().max(1)),
        "end-of-transaction errors name an op of the transaction"
    );
}

/// Applies one op; returns its inverse. On error `doc` is unchanged.
fn apply_one(
    doc: &mut Document,
    op: &mut Op,
    params: &ParamMap,
    rng: &mut IdRng,
) -> Result<Op, Code> {
    match op {
        Op::Insert { rec, at } => {
            if rec.kind == RecordKind::Header {
                return Err(Code::E043);
            }
            assign_id(doc, rec, rng);
            doc.check(rec)?;
            inst_params(rec, params, Scope::All)?;
            let loc = doc.insert(rec, *at)?;
            Ok(Op::Delete(Target::at(doc, loc)))
        }
        Op::Delete(t) => {
            let loc = locate(doc, t)?;
            let prior = doc.record(loc);
            let at = loc.table.ordered().then_some(loc.row);
            doc.delete(loc);
            let positional = t.kind.key() == KeyRule::Position;
            assert!(
                positional || doc.locate(t.kind, &t.key).is_none(),
                "the id or name is gone"
            );
            Ok(Op::Insert { rec: prior, at })
        }
        Op::Put { target, rec } => {
            if target.kind == RecordKind::Header {
                return put_header(doc, rec);
            }
            let loc = locate(doc, target)?;
            if rec.kind.key() == KeyRule::Id && rec.fields.first() != Some(&target.key) {
                return Err(Code::E043);
            }
            doc.check(rec)?;
            inst_params(rec, params, Scope::All)?;
            let prior = doc.record(loc);
            let loc = doc.put(loc, rec)?;
            Ok(Op::Put {
                target: Target::at(doc, loc),
                rec: prior,
            })
        }
        Op::SetField {
            target,
            index,
            value,
        } => {
            if target.kind == RecordKind::Header {
                return Err(Code::E043);
            }
            let loc = locate(doc, target)?;
            let prior = doc.record(loc);
            let i = *index as usize;
            if i >= prior.fields.len() || (i == 0 && prior.kind.key() == KeyRule::Id) {
                return Err(Code::E043);
            }
            let mut rec = prior.clone();
            rec.fields[i] = value.clone();
            doc.check(&rec)?;
            // A new cell reference re-checks the overrides against the new cell (E030).
            inst_params(&mut rec, params, Scope::CheckOnly)?;
            modify(doc, loc, prior, rec)
        }
        Op::SetAttr {
            target,
            key,
            values,
        } => {
            if !schemify_format::is_word(key) {
                return Err(Code::E028);
            }
            let (loc, prior) = read(doc, target)?;
            let mut rec = prior.clone();
            let line = set_attr(&mut rec, key, values);
            let Some(loc) = loc else {
                return put_header(doc, &mut rec);
            };
            doc.check(&rec)?;
            inst_params(&mut rec, params, Scope::Key(key))?;
            // Journal the snapped value: it is what was stored (edits spec §1).
            values.clone_from(&rec.attrs[line].values);
            modify(doc, loc, prior, rec)
        }
        Op::RemoveAttr { target, key, first } => {
            let (loc, prior) = read(doc, target)?;
            let mut rec = prior.clone();
            let matches = |a: &Attr| {
                a.key == *key && first.as_ref().is_none_or(|f| a.values.first() == Some(f))
            };
            rec.attrs.retain(|a| !matches(a));
            if rec.attrs.len() == prior.attrs.len() {
                return Err(Code::E040);
            }
            match loc {
                None => put_header(doc, &mut rec),
                Some(loc) => {
                    doc.check(&rec)?;
                    modify(doc, loc, prior, rec)
                }
            }
        }
        Op::Reorder { target, to } => {
            let loc = locate(doc, target)?;
            let from = loc.row;
            let new = doc.reorder(loc, *to)?;
            assert_eq!(new.row, *to);
            Ok(Op::Reorder {
                target: Target::at(doc, new),
                to: from,
            })
        }
    }
}

/// Puts the validated, modified copy `rec` of the record at `loc` back.
fn modify(doc: &mut Document, loc: Loc, prior: RecordBuf, rec: RecordBuf) -> Result<Op, Code> {
    assert_eq!(prior.kind, rec.kind);
    let loc = doc.put(loc, &rec)?;
    let inv = Op::Put {
        target: Target::at(doc, loc),
        rec: prior,
    };
    assert!(matches!(inv, Op::Put { .. }));
    Ok(inv)
}

/// Reads the target (the header has no `Loc`).
fn read(doc: &Document, t: &Target) -> Result<(Option<Loc>, RecordBuf), Code> {
    if t.kind == RecordKind::Header {
        let header = doc.header_record();
        assert!(header.fields.is_empty(), "the header has no fields");
        return Ok((None, header));
    }
    let loc = locate(doc, t)?;
    let rec = doc.record(loc);
    assert_eq!(rec.kind, t.kind);
    Ok((Some(loc), rec))
}

fn put_header(doc: &mut Document, rec: &mut RecordBuf) -> Result<Op, Code> {
    if rec.kind != RecordKind::Header {
        return Err(Code::E043);
    }
    rec.fields.clear();
    doc.check(rec)?;
    let prior = doc.header_record();
    doc.put_header(rec);
    assert_eq!(
        doc.header_record(),
        *rec,
        "the header holds exactly the put attributes"
    );
    Ok(Op::Put {
        target: Target::header(),
        rec: prior,
    })
}

fn locate(doc: &Document, t: &Target) -> Result<Loc, Code> {
    if t.kind == RecordKind::Header {
        return Err(Code::E043);
    }
    let loc = doc.locate(t.kind, &t.key).ok_or(Code::E040)?;
    assert!((loc.row as usize) < doc.table_len(loc.table));
    Ok(loc)
}

fn assign_id(doc: &Document, rec: &mut RecordBuf, rng: &mut IdRng) {
    if rec.kind.key() != KeyRule::Id || rec.fields.first().map(String::as_str) != Some(NEW_ID) {
        return;
    }
    let id = schemify_format::fresh_id(rng, |t| {
        ObjId::find(t, doc.strings())
            .and_then(|i| doc.find_id(i))
            .is_some()
    });
    rec.fields[0] = id.as_str().to_owned();
    assert_ne!(rec.fields[0], NEW_ID);
    let taken = ObjId::find(&rec.fields[0], doc.strings()).and_then(|i| doc.find_id(i));
    assert!(taken.is_none(), "a fresh id names no record yet");
}

/// Instance parameter semantics on a validated record: `E030` for an override of a
/// computed param, snapping of plain-number overrides in `scope` (D13).
fn inst_params(rec: &mut RecordBuf, params: &ParamMap, scope: Scope<'_>) -> Result<(), Code> {
    if rec.kind != RecordKind::Inst {
        return Ok(());
    }
    assert_eq!(rec.fields.len(), 2, "validated instances have two fields");
    let Some(Some(facts)) = params.get(&rec.fields[1]) else {
        return Ok(());
    };
    let overrides = rec
        .attrs
        .iter_mut()
        .filter(|a| Instances::is_override(&a.key));
    for a in overrides {
        let Some(p) = facts.iter().find(|p| p.name == a.key) else {
            continue; // override of an unknown param: elab's W007
        };
        if p.computed {
            return Err(Code::E030);
        }
        let snap_this = match scope {
            Scope::All => true,
            Scope::Key(k) => k == a.key,
            Scope::CheckOnly => false,
        };
        if snap_this
            && let Some(v) = a.values.first_mut()
            && let Some(s) = p.snap(v)
        {
            *v = s;
        }
    }
    Ok(())
}

/// Sets an attribute line on an owned record (see [`Op::SetAttr`]); returns its index.
fn set_attr(rec: &mut RecordBuf, key: &str, values: &[String]) -> usize {
    assert!(
        schemify_format::is_word(key),
        "the caller refuses a key that is not a word (E028)"
    );
    let lines = rec.attrs.len();
    let repeat = rec.kind.schema().attr(key).is_some_and(|(_, a)| a.repeat);
    let pos = rec
        .attrs
        .iter()
        .position(|a| a.key == key && (!repeat || a.values.first() == values.first()));
    let i = match pos {
        Some(i) => {
            rec.attrs[i].values = values.to_vec();
            i
        }
        None => {
            rec.attrs.push(Attr {
                key: key.to_owned(),
                values: values.to_vec(),
            });
            rec.attrs.len() - 1
        }
    };
    assert_eq!(rec.attrs[i].key, key);
    assert!(
        rec.attrs.len() <= lines + 1,
        "replaces a line or appends one"
    );
    i
}
