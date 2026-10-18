//! The change log of one [`Document`](crate::Document): every row write the edit
//! primitives make (insert, delete and the row it swaps in, put, reorder), in order, so a
//! derived table (the editor's scene, its plugin snapshot) follows an edit in O(change)
//! instead of diffing every row (D39, `docs/dev/perf.md` "Edit frames pass 2026-10").
//!
//! In: the primitives' row writes. Out: [`Change`] rows since a consumer's [`Cursor`], or
//! `None` when the log no longer reaches it (the consumer rebuilds from the tables).
//!
//! Six questions: 1. row writes in, a sequence of changes out; 2. a handful per edit, at
//! most [`LOG_MAX`] kept; 3. table `u8`, row `u32` (tables hold < 2^22 rows), id `u32`;
//! 4. appended once, read sequentially by each consumer once per frame; 5. lives with the
//! document (a reload or merge makes a new document, so a new log); 6. not parallel.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::document::TableId;
use crate::strings::ObjId;

/// Changes the log keeps; past it the older half is dropped (a consumer that far behind
/// rebuilds).
pub const LOG_MAX: usize = 1 << 16;

/// Source of log origins: two logs never share one, so a cursor of one never reads another.
static ORIGINS: AtomicU64 = AtomicU64::new(1);

/// What a row write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Write {
    /// The row now holds a new record (an insert, or a put replacing it): `id` is that record.
    Content,
    /// Another record moved into the row (the swap after a delete, an ordered table's shift
    /// or reorder); `id` is that record, whose content did not change.
    Moved,
    /// Record `id` left the table; `row` was where it was (now another record, after a
    /// [`Write::Moved`] change, or past the end).
    Deleted,
}

/// One row write of table `table`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    /// The table written.
    pub table: TableId,
    /// What the write did.
    pub what: Write,
    /// The row written.
    pub row: u32,
    /// The record written, moved or deleted (`ObjId::NONE` for a table without ids).
    pub id: ObjId,
}

/// A consumer's position in a document's log ([`crate::Document::cursor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursor {
    origin: u64,
    at: u64,
}

/// The log itself (owned by its document; only `doc` writes it).
#[derive(Debug)]
pub(crate) struct ChangeLog {
    origin: u64,
    /// Sequence number of `rows[0]`.
    start: u64,
    rows: Vec<Change>,
}

impl Default for ChangeLog {
    fn default() -> ChangeLog {
        ChangeLog {
            origin: ORIGINS.fetch_add(1, Ordering::Relaxed),
            start: 0,
            rows: Vec::new(),
        }
    }
}

/// A copy of a document is another document: its log starts afresh, so a cursor of the
/// original never reads the copy's (diverging) changes.
impl Clone for ChangeLog {
    fn clone(&self) -> ChangeLog {
        ChangeLog::default()
    }
}

impl ChangeLog {
    fn seq(&self) -> u64 {
        self.start + self.rows.len() as u64
    }

    /// Appends one change, dropping the older half when full.
    pub(crate) fn push(&mut self, table: TableId, what: Write, row: u32, id: ObjId) {
        if self.rows.len() >= LOG_MAX {
            let half = LOG_MAX / 2;
            self.rows.drain(..half);
            self.start += half as u64;
        }
        self.rows.push(Change {
            table,
            what,
            row,
            id,
        });
        assert!(self.rows.len() <= LOG_MAX);
    }

    /// Every cursor taken so far stops reading through: the tables changed in a way the
    /// log does not describe (a compaction renumbered strings and rows).
    /// The sequence skips one number, so even a cursor taken right before is behind it.
    pub(crate) fn break_here(&mut self) {
        let next = self.seq() + 1;
        self.rows.clear();
        self.start = next;
        assert_eq!(self.seq(), next);
    }

    pub(crate) fn cursor(&self) -> Cursor {
        Cursor {
            origin: self.origin,
            at: self.seq(),
        }
    }

    pub(crate) fn since(&self, c: Cursor) -> Option<&[Change]> {
        if c.origin != self.origin || c.at < self.start || c.at > self.seq() {
            return None;
        }
        let from = (c.at - self.start) as usize;
        assert!(from <= self.rows.len());
        Some(&self.rows[from..])
    }
}
