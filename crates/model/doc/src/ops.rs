//! Edit requests as data: [`Target`] addresses a record, [`Op`] is one edit op, a
//! [`Transaction`] an ordered list of them, [`OpError`] why one failed.
//! Cites: edits spec §1 (ops, modes, op text keys); `put` and positional `insert` are
//! doc amendments.
//!
//! Contents:
//! - `pub const NEW_ID`, `pub(crate) const OPS_MAX`
//! - `pub struct Target`: `new`, `header`; crate: `at`
//! - `pub enum Op`: `subject`
//! - `pub enum Mode`, `pub struct Transaction`, `pub struct OpError`

use crate::Code;
use crate::document::{Document, Loc};
use crate::record::RecordBuf;
use schemify_format::RecordKind;

/// The id field value that asks `doc` to draw a fresh id on insert.
pub const NEW_ID: &str = "-";
/// Most ops one transaction may carry (a paste of a whole 2^22-object cell).
pub(crate) const OPS_MAX: usize = 1 << 22;

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

    /// Addresses the record at `loc` of `doc` by its key (id, name or graphic index).
    pub(crate) fn at(doc: &Document, loc: Loc) -> Target {
        let t = Target {
            kind: doc.kind_at(loc),
            key: doc.key_of(loc),
        };
        assert_eq!(
            doc.locate(t.kind, &t.key),
            Some(loc),
            "the target addresses the row back"
        );
        t
    }
}

impl Op {
    /// Returns the record this op changes: (kind, key text), the key as in [`Target`].
    /// Inserts report the payload's first field (a graphic's draw-order index is its
    /// insert row instead, which an insert without `at` does not know).
    pub fn subject(&self) -> (RecordKind, &str) {
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
