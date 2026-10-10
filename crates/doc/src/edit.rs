//! Edit ops and the all-or-nothing transaction engine (edits spec §1).
//!
//! Spine of [`apply`]: note which whole-document rules the ops can affect → apply each
//! op (resolving fresh ids, snapping, building its exact inverse) → end-of-transaction
//! checks (`E026`, `E025`, `E042`) → on any error, replay the inverses and return every
//! (op index, code); otherwise return the inverse transaction.
//!
//! Every modification of an existing record is "read the record, change the owned
//! copy, validate it, put it back", so the inverse of `set_field`/`set_attr`/
//! `remove_attr`/`put` is one `put` of the prior record. That is exact even when the
//! table normalizes (wire endpoint order) or the value was snapped.

use crate::Code;
use crate::document::{Document, Loc, TableId};
use crate::record::{Attr, RecordBuf, is_word};
use crate::strings::ObjId;
use crate::tables::Instances;
use schemify_format::{IdRng, KeyRule, RecordKind, fresh_id, is_number};
use std::collections::BTreeMap;

/// The id field value that asks `doc` to draw a fresh id on insert.
pub const NEW_ID: &str = "-";
/// Most ops one transaction may carry (a paste of a whole 2^22-object cell).
pub const OPS_MAX: usize = 1 << 22;

/// Addresses one record: its kind plus its key. The key is the id for id-keyed kinds,
/// the name for named kinds, the draw-order index for graphics, and ignored (`"0"`)
/// for the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Record kind (`Header` for the header).
    pub kind: RecordKind,
    /// Id, name or graphic index.
    pub key: String,
}

impl Target {
    /// Addresses a record.
    pub fn new(kind: RecordKind, key: &str) -> Target {
        Target {
            kind,
            key: key.to_owned(),
        }
    }

    /// Addresses the header.
    pub fn header() -> Target {
        Target::new(RecordKind::Header, "0")
    }
}

/// One edit op (edits spec §1; `put` and positional `insert` are doc amendments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Adds a record. An id field of [`NEW_ID`] is replaced by a fresh id. `at` is the
    /// row in an ordered table (ranks 1, 2, 4, 5); `None` puts it after the last record
    /// of its kind. Inverse: `delete`.
    Insert {
        /// The record.
        rec: RecordBuf,
        /// Row in an ordered table.
        at: Option<u32>,
    },
    /// Removes a record. Inverse: `insert` of the prior record at its prior row.
    Delete(Target),
    /// Replaces a record wholesale, keeping its row (ids are immutable). Inverse: `put`
    /// of the prior record.
    Put {
        /// The record to replace.
        target: Target,
        /// Its new content.
        rec: RecordBuf,
    },
    /// Sets positional field `index` (0-based, after the word). Field 0 of an id-keyed
    /// record is immutable (`E043`). Inverse: `put` of the prior record.
    SetField {
        /// The record.
        target: Target,
        /// Field index.
        index: u32,
        /// New value.
        value: String,
    },
    /// Sets an attribute line: replaces the line with `key` (for repeatable attributes,
    /// the one whose first value matches), else appends. Inverse: `put` of the prior.
    SetAttr {
        /// The record.
        target: Target,
        /// Attribute word.
        key: String,
        /// Values.
        values: Vec<String>,
    },
    /// Removes the lines with `key` (only those whose first value is `first`, if given).
    /// Removing nothing is `E040`. Inverse: `put` of the prior.
    RemoveAttr {
        /// The record.
        target: Target,
        /// Attribute word.
        key: String,
        /// First value selecting repeatable lines.
        first: Option<String>,
    },
    /// Moves a record of an ordered table to row `to`. Inverse: `reorder` back.
    Reorder {
        /// The record.
        target: Target,
        /// New row.
        to: u32,
    },
}

impl Op {
    /// Returns the record this op is about: (kind, key text). Inserts report the
    /// payload's first field.
    pub(crate) fn subject(&self) -> (RecordKind, &str) {
        match self {
            Op::Insert { rec, .. } => (rec.kind, rec.fields.first().map_or("", String::as_str)),
            Op::Delete(t)
            | Op::Put { target: t, .. }
            | Op::SetField { target: t, .. }
            | Op::SetAttr { target: t, .. }
            | Op::RemoveAttr { target: t, .. }
            | Op::Reorder { target: t, .. } => (t.kind, t.key.as_str()),
        }
    }
}

/// Whether a transaction is applied now or held for review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Validate, apply, push the inverse on the undo stack, journal it.
    Commit,
    /// Validate, then store in `staged_edits` without applying.
    Stage,
}

/// An ordered list of ops on one document with an undo-menu label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    /// Label for undo menus and the journal.
    pub label: String,
    /// Commit or stage.
    pub mode: Mode,
    /// The ops, applied in order.
    pub ops: Vec<Op>,
}

/// Why one op of a rejected transaction failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpError {
    /// Index of the op in its transaction.
    pub op: u32,
    /// What went wrong.
    pub code: Code,
}

/// What edits need to know about one parameter of an instantiated cell: whether it is
/// computed (`E030`) and its plain-number snapping rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParamFacts {
    pub(crate) name: String,
    pub(crate) computed: bool,
    pub(crate) grid: Option<String>,
    pub(crate) step: Option<String>,
    pub(crate) min: Option<String>,
}

impl ParamFacts {
    /// Returns the snapped value when `value` is a plain number and the param has a
    /// plain-number `grid` (multiples of it) or `step` (from `min` when that is a plain
    /// number, else from 0); `None` when nothing changes (expressions are elab's).
    pub(crate) fn snap(&self, value: &str) -> Option<String> {
        if !is_number(value) {
            return None;
        }
        // A `grid` that is an expression wins over `step`: elab evaluates and gates it.
        let snapped = match (self.grid.as_deref(), self.step.as_deref()) {
            (Some(g), _) => snap(value, g, "0"),
            (None, Some(st)) => snap(value, st, self.min.as_deref().unwrap_or("0")),
            (None, None) => None,
        };
        assert!(
            snapped.as_deref().is_none_or(is_number),
            "snapping yields plain numbers"
        );
        snapped.filter(|s| s != value)
    }
}

/// Per cell reference text (as written in this document): the cell's parameters, or
/// `None` when the reference does not resolve (then nothing is snapped or checked;
/// elab reports `E001`).
pub(crate) type ParamMap = BTreeMap<String, Option<Vec<ParamFacts>>>;

// ------------------------------------------------------------------ the engine

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
    apply_with(doc, ops, params, rng, true)
}

/// Re-applies ops that once validated onto possibly different content (journal
/// recovery onto a changed file, local edits rebased onto external bytes): every check
/// runs, no parameter semantics, no id draws. Returns their inverse.
///
/// # Errors
///
/// As [`apply`]: the content no longer admits the ops.
pub(crate) fn replay(doc: &mut Document, ops: &mut [Op]) -> Result<Vec<Op>, Vec<OpError>> {
    apply_with(doc, ops, &ParamMap::new(), &mut IdRng::new(0), true)
}

/// Applies ops that restore a state that existed (undo, redo, a stage rollback, the
/// step from a replay to a merge result): records are still validated, but not the
/// end-of-transaction rules, which guard new user edits, not history.
///
/// # Errors
///
/// Record-level errors only; one means the document changed under the history.
pub(crate) fn restore(doc: &mut Document, ops: &mut [Op]) -> Result<Vec<Op>, Vec<OpError>> {
    apply_with(doc, ops, &ParamMap::new(), &mut IdRng::new(0), false)
}

fn apply_with(
    doc: &mut Document,
    ops: &mut [Op],
    params: &ParamMap,
    rng: &mut IdRng,
    end_rules: bool,
) -> Result<Vec<Op>, Vec<OpError>> {
    assert!(
        !doc.read_only(),
        "callers refuse read-only documents (E044)"
    );
    assert!(ops.len() <= OPS_MAX, "transaction exceeds {OPS_MAX} ops");
    let rules = end_rules && ops.iter().any(|op| touches_kind_rules(op.subject().0));
    let refs = end_rules && ops.iter().any(touches_refs);
    let rules_before = rules && doc.kind_rules_ok();
    let dangling_before = if refs { doc.dangling() } else { Vec::new() };
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
        end_checks(doc, ops, rules_before, &dangling_before, refs, &mut errors);
    }
    if !errors.is_empty() {
        undo_partial(doc, inverse);
        errors.sort_by_key(|e| e.op);
        return Err(errors);
    }
    inverse.reverse();
    assert!(inverse.len() == ops.len());
    Ok(inverse)
}

fn undo_partial(doc: &mut Document, inverse: Vec<Op>) {
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
    dangling_before: &[(RecordKind, String, String)],
    refs: bool,
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
    if refs {
        for d in doc.dangling() {
            if dangling_before.binary_search(&d).is_ok() {
                continue;
            }
            let (kind, key, target) = (d.0, d.1.as_str(), d.2.as_str());
            let op = last(ops, |k, x| (k == kind && x == key) || x == target);
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
            Ok(Op::Delete(target_at(doc, loc)))
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
                target: target_at(doc, loc),
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
            if !is_word(key) {
                return Err(Code::E028);
            }
            let (loc, prior) = read(doc, target)?;
            let mut rec = prior.clone();
            set_attr(&mut rec, key, values);
            let Some(loc) = loc else {
                return put_header(doc, &mut rec);
            };
            doc.check(&rec)?;
            inst_params(&mut rec, params, Scope::Key(key))?;
            // Journal the snapped value: it is what was stored (edits spec §1).
            if let Some(a) = find_set_line(&rec, key, values) {
                values.clone_from(&a.values);
            }
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
                target: target_at(doc, new),
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
        target: target_at(doc, loc),
        rec: prior,
    };
    assert!(matches!(inv, Op::Put { .. }));
    Ok(inv)
}

/// Reads the target (the header has no `Loc`).
fn read(doc: &Document, t: &Target) -> Result<(Option<Loc>, RecordBuf), Code> {
    if t.kind == RecordKind::Header {
        return Ok((None, doc.header_record()));
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
    doc.locate(t.kind, &t.key).ok_or(Code::E040)
}

fn target_at(doc: &Document, loc: Loc) -> Target {
    let t = Target {
        kind: doc.kind_at(loc),
        key: doc.key_of(loc),
    };
    assert!(
        doc.locate(t.kind, &t.key) == Some(loc),
        "the target addresses the row back"
    );
    t
}

fn assign_id(doc: &Document, rec: &mut RecordBuf, rng: &mut IdRng) {
    if rec.kind.key() != KeyRule::Id || rec.fields.first().map(String::as_str) != Some(NEW_ID) {
        return;
    }
    let id = fresh_id(rng, |t| {
        ObjId::find(t, doc.strings())
            .and_then(|i| doc.find_id(i))
            .is_some()
    });
    rec.fields[0] = id.as_str().to_owned();
    assert_ne!(rec.fields[0], NEW_ID);
}

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

/// Instance parameter semantics on a validated record: `E030` for an override of a
/// computed param, snapping of plain-number overrides in `scope` (D13).
fn inst_params(rec: &mut RecordBuf, params: &ParamMap, scope: Scope<'_>) -> Result<(), Code> {
    if rec.kind != RecordKind::Inst {
        return Ok(());
    }
    assert!(rec.fields.len() == 2, "validated instances have two fields");
    let Some(Some(facts)) = params.get(&rec.fields[1]) else {
        return Ok(());
    };
    for a in rec
        .attrs
        .iter_mut()
        .filter(|a| Instances::is_override(&a.key))
    {
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

/// Sets an attribute line on an owned record (see [`Op::SetAttr`]).
fn set_attr(rec: &mut RecordBuf, key: &str, values: &[String]) {
    let repeat = rec.kind.schema().attr(key).is_some_and(|(_, a)| a.repeat);
    let pos = rec
        .attrs
        .iter()
        .position(|a| a.key == key && (!repeat || a.values.first() == values.first()));
    match pos {
        Some(i) => rec.attrs[i].values = values.to_vec(),
        None => rec.attrs.push(Attr {
            key: key.to_owned(),
            values: values.to_vec(),
        }),
    }
    assert!(rec.attrs.iter().any(|a| a.key == key));
}

fn find_set_line<'r>(rec: &'r RecordBuf, key: &str, values: &[String]) -> Option<&'r Attr> {
    let repeat = rec.kind.schema().attr(key).is_some_and(|(_, a)| a.repeat);
    rec.attrs
        .iter()
        .find(|a| a.key == key && (!repeat || a.values.first() == values.first()))
}

// ------------------------------------------------------------------ snapping

/// A decimal `m / 10^s`.
#[derive(Clone, Copy)]
struct Dec {
    m: i128,
    s: u32,
}

/// Digits a snapped operand may have; beyond it the value is left for elab.
const DIGITS_MAX: usize = 30;

fn dec(text: &str) -> Option<Dec> {
    if !is_number(text) || text.len() > DIGITS_MAX {
        return None;
    }
    let (neg, body) = text.strip_prefix('-').map_or((false, text), |b| (true, b));
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    let mut m: i128 = 0;
    for b in whole.bytes().chain(frac.bytes()) {
        m = m * 10 + i128::from(b - b'0');
    }
    assert!(
        m >= 0 && m < 10i128.pow(DIGITS_MAX as u32),
        "at most 30 digits"
    );
    Some(Dec {
        m: if neg { -m } else { m },
        s: frac.len() as u32,
    })
}

/// Snaps `value` to `origin + k * grid` for the nearest integer `k` (ties away from
/// zero), in exact decimal arithmetic. Returns the shortest decimal text, or `None` when
/// an operand is not a plain number, the grid is not positive, or digits overflow.
pub(crate) fn snap(value: &str, grid: &str, origin: &str) -> Option<String> {
    let (v, g, o) = (dec(value)?, dec(grid)?, dec(origin)?);
    let s = v.s.max(g.s).max(o.s);
    let scale = |d: Dec| d.m.checked_mul(10i128.checked_pow(s - d.s)?);
    let (v, g, o) = (scale(v)?, scale(g)?, scale(o)?);
    if g <= 0 {
        return None;
    }
    let diff = v.checked_sub(o)?;
    let (q, r) = (diff / g, diff % g);
    let k = if 2 * r.abs() >= g {
        q + diff.signum()
    } else {
        q
    };
    let out = o.checked_add(k.checked_mul(g)?)?;
    assert!(
        (out - o) % g == 0 && 2 * (out - v).abs() <= g,
        "on grid, nearest"
    );
    Some(format_dec(out, s))
}

fn format_dec(m: i128, s: u32) -> String {
    let neg = m < 0;
    let digits = m.unsigned_abs().to_string();
    let s = s as usize;
    let padded = if digits.len() <= s {
        format!("{}{digits}", "0".repeat(s + 1 - digits.len()))
    } else {
        digits
    };
    let (whole, frac) = padded.split_at(padded.len() - s);
    let frac = frac.trim_end_matches('0');
    let mut out = String::new();
    if neg && (whole != "0" || !frac.is_empty()) {
        out.push('-');
    }
    out.push_str(whole);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    assert!(is_number(&out));
    out
}

// ------------------------------------------------------------------ diff → ops

/// Returns ops that turn `from` into `to` (same file kind; canonical text equal after
/// applying). The header is one `put`; an ordered table that differs is rewritten whole
/// (deletes by descending row, inserts by ascending row); unordered tables go by key
/// (delete, put, insert). All deletes come first, then puts, then inserts, so a key may
/// move between kinds. Cost: O(n log n) in records, allocating one `RecordBuf` each.
pub(crate) fn diff_ops(from: &Document, to: &Document) -> Vec<Op> {
    assert_eq!(from.kind(), to.kind(), "diff across file kinds");
    let (mut deletes, mut puts, mut inserts) = (Vec::new(), Vec::new(), Vec::new());
    let header = to.header_record();
    if from.header_record() != header {
        puts.push(Op::Put {
            target: Target::header(),
            rec: header,
        });
    }
    for t in TableId::ALL {
        let rows = |d: &Document| -> Vec<(RecordBuf, Target)> {
            (0..d.table_len(t) as u32)
                .map(|row| {
                    let loc = Loc { table: t, row };
                    (d.record(loc), target_at(d, loc))
                })
                .collect()
        };
        let (a, b) = (rows(from), rows(to));
        if t.ordered() {
            if a.iter().map(|x| &x.0).eq(b.iter().map(|x| &x.0)) {
                continue;
            }
            deletes.extend(a.into_iter().rev().map(|(_, tg)| Op::Delete(tg)));
            for (row, (rec, _)) in b.into_iter().enumerate() {
                inserts.push(Op::Insert {
                    rec,
                    at: Some(row as u32),
                });
            }
            continue;
        }
        let key = |(r, tg): &(RecordBuf, Target)| ((r.kind, tg.key.clone()), r.clone());
        let a: BTreeMap<_, _> = a.iter().map(key).collect();
        let b: BTreeMap<_, _> = b.iter().map(key).collect();
        for ((kind, k), rec) in &a {
            match b.get(&(*kind, k.clone())) {
                None => deletes.push(Op::Delete(Target::new(*kind, k))),
                Some(new) if new != rec => puts.push(Op::Put {
                    target: Target::new(*kind, k),
                    rec: new.clone(),
                }),
                Some(_) => {}
            }
        }
        for ((kind, k), rec) in &b {
            if !a.contains_key(&(*kind, k.clone())) {
                inserts.push(Op::Insert {
                    rec: rec.clone(),
                    at: None,
                });
            }
        }
    }
    let rows: usize = TableId::ALL
        .iter()
        .map(|t| from.table_len(*t) + to.table_len(*t))
        .sum();
    deletes.extend(puts);
    deletes.extend(inserts);
    assert!(
        deletes.len() <= rows + 1,
        "at most one op per record plus the header"
    );
    deletes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_is_exact_decimal() {
        assert_eq!(snap("0.153", "0.005", "0").as_deref(), Some("0.155"));
        assert_eq!(snap("0.152", "0.005", "0").as_deref(), Some("0.15"));
        assert_eq!(
            snap("0.1525", "0.005", "0").as_deref(),
            Some("0.155"),
            "ties away from zero"
        );
        assert_eq!(snap("-0.1525", "0.005", "0").as_deref(), Some("-0.155"));
        assert_eq!(snap("7", "2", "0").as_deref(), Some("8"));
        assert_eq!(snap("0.4", "1", "0").as_deref(), Some("0"));
        assert_eq!(snap("-0.4", "1", "0").as_deref(), Some("0"));
        assert_eq!(snap("1.07", "0.1", "0.15").as_deref(), Some("1.05"));
        assert_eq!(snap("1u", "0.1", "0"), None, "expressions are elab's");
        assert_eq!(snap("1", "0", "0"), None);
        assert_eq!(snap("1", "-1", "0"), None);
        assert_eq!(snap("1", "w", "0"), None);
    }

    #[test]
    fn param_facts_choose_grid_then_step() {
        let mut p = ParamFacts {
            name: "L".into(),
            computed: false,
            grid: Some("0.005".into()),
            step: Some("1".into()),
            min: None,
        };
        assert_eq!(p.snap("0.151").as_deref(), Some("0.15"));
        assert_eq!(p.snap("0.15"), None, "already on grid");
        p.grid = Some("lambda".into());
        assert_eq!(
            p.snap("2.6"),
            None,
            "an expression grid is elab's, even with a step"
        );
        p.grid = None;
        p.min = Some("0.5".into());
        assert_eq!(p.snap("2.6").as_deref(), Some("2.5"));
        p.min = Some("lmin".into());
        assert_eq!(p.snap("2.6"), None, "step from an expression min is elab's");
    }
}
