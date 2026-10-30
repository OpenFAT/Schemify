//! Validated records → row writes on a [`Document`] (insert, delete, put, reorder), each
//! logged to the change log and the id index; plus the record lookups and the
//! whole-document rules an edit transaction checks at its end.
//! Cites: edits spec §1 (ops, `E040`–`E043`), format spec §4 (kind rules `E025`, keys).
//!
//! Contents (crate methods of `Document`):
//! - `push_row`, `replace_row`, `insert`, `delete`, `put`, `reorder`, `put_header`,
//!   `check`
//! - `locate`, `kind_at`, `key_of`
//! - `kind_rules_ok`, `dangling`, `wire_zero`
//! - `pub(crate) struct Dangling`

use crate::Code;
use crate::changes::Write;
use crate::document::{Document, Loc, TableId};
use crate::pool::{RecView, Scratch};
use crate::record::{self, RecordBuf};
use crate::strings::ObjId;
use crate::tables::{Owned, Table};
use schemify_format::{FileKind, KeyRule, RecordKind};
use std::ops::Range;

/// An in-file reference whose target does not exist (`E042`). Orders by the
/// referencing record, then the referenced text, so a sorted list can be searched.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Dangling {
    /// Kind of the referencing record (`Header` for the header's `default`).
    pub(crate) kind: RecordKind,
    /// Key of the referencing record (empty for the header).
    pub(crate) key: String,
    /// The text that names no record.
    pub(crate) target: String,
}

impl Document {
    /// Appends a validated record to table `t` and indexes its id. Returns its row.
    ///
    /// # Panics
    ///
    /// Panics past the table's capacity (callers that take user input check first).
    pub(crate) fn push_row(&mut self, t: TableId, rec: &RecView<'_>) -> Loc {
        let row = with_table_mut!(self, t, |tab, strings, pool| {
            tab.push(rec, strings, pool);
            tab.len() - 1
        });
        assert!(row < t.capacity() as usize, "{t:?} exceeds its capacity");
        let loc = Loc {
            table: t,
            row: row as u32,
        };
        let id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
        if id != ObjId::NONE {
            // First occurrence wins; `reid_dups` gives later duplicates new ids.
            self.by_id.entry(id).or_insert(loc);
        }
        assert_eq!(self.table_len(t), row + 1, "pushed rows land at the end");
        loc
    }

    /// Replaces a row: push the new one, then swap it into place (only the replaced row
    /// changes position-wise, so file order is kept).
    pub(crate) fn replace_row(&mut self, loc: Loc, rec: &RecordBuf) {
        let (t, row) = (loc.table, loc.row as usize);
        assert!(row < self.table_len(t), "stale row");
        assert_eq!(
            TableId::of(rec.kind),
            Some(t),
            "the record belongs to the row's table"
        );
        let old_id = self.unindex(loc);
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        let len = self.table_len(t);
        let tmp = with_table_mut!(self, t, |tab, strings, pool| {
            tab.push(&scratch.view(rec.kind), strings, pool);
            tab.swap_remove_row(row);
            tab.len()
        });
        assert_eq!(tmp, len);
        self.reindex(t, row..row + 1, Write::Content);
        let new_id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
        if old_id != new_id && old_id != ObjId::NONE {
            self.log.push(t, Write::Deleted, row as u32, old_id);
        }
    }

    /// Inserts a validated record: at row `at` of an ordered table (default: after the
    /// last row of the same kind, else at the end), anywhere in an unordered one.
    ///
    /// # Errors
    ///
    /// `E041` id taken, `E024` name taken, `E043` position out of range, position given
    /// for an unordered table, or table full.
    pub(crate) fn insert(&mut self, rec: &RecordBuf, at: Option<u32>) -> Result<Loc, Code> {
        let t = TableId::of(rec.kind).expect("validated records have a table");
        self.check_key_free(rec, None)?;
        let len = self.table_len(t);
        if len >= t.capacity() as usize {
            return Err(Code::E043);
        }
        let pos = match at {
            Some(_) if !t.ordered() => return Err(Code::E043),
            Some(p) if p as usize > len => return Err(Code::E043),
            Some(p) => p as usize,
            None if t.ordered() => self.default_position(t, rec.kind),
            None => len,
        };
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        let mut loc = self.push_row(t, &scratch.view(rec.kind));
        if pos != len {
            self.move_rows(t, len, pos);
            loc.row = pos as u32;
        }
        let id = with_table!(self, t, |tab| tab.id_at(loc.row as usize, &self.strings));
        self.log.push(t, Write::Content, loc.row, id);
        assert_eq!(self.kind_at(loc), rec.kind);
        assert_eq!(self.table_len(t), len + 1, "one row added");
        Ok(loc)
    }

    /// Deletes the row at `loc`. Ordered tables shift; unordered ones swap the last row in.
    ///
    /// # Panics
    ///
    /// Panics on a stale `loc`.
    pub(crate) fn delete(&mut self, loc: Loc) {
        let (t, row) = (loc.table, loc.row as usize);
        let len = self.table_len(t);
        assert!(row < len, "stale row");
        let id = self.unindex(loc);
        self.log.push(t, Write::Deleted, row as u32, id);
        if t.ordered() {
            with_table_mut!(self, t, |tab, _s, _p| tab.remove_row(row));
            self.reindex(t, row..len - 1, Write::Moved);
        } else {
            with_table_mut!(self, t, |tab, _s, _p| tab.swap_remove_row(row));
            self.reindex(t, row..(row + 1).min(len - 1), Write::Moved);
        }
        assert_eq!(self.table_len(t), len - 1);
    }

    /// Replaces the row at `loc` with a validated record of the same table, keeping its
    /// position. The new key must be free (or be the row's own).
    ///
    /// # Errors
    ///
    /// `E041`/`E024` when the new key belongs to another record; `E043` when the record
    /// belongs to another table.
    pub(crate) fn put(&mut self, loc: Loc, rec: &RecordBuf) -> Result<Loc, Code> {
        if TableId::of(rec.kind) != Some(loc.table) {
            return Err(Code::E043);
        }
        self.check_key_free(rec, Some(loc))?;
        let len = self.table_len(loc.table);
        assert!((loc.row as usize) < len, "stale row");
        self.replace_row(loc, rec);
        assert_eq!(self.kind_at(loc), rec.kind);
        assert_eq!(self.table_len(loc.table), len, "a put keeps the row count");
        Ok(loc)
    }

    /// Moves the row at `loc` to row `to` of its (ordered) table.
    ///
    /// # Errors
    ///
    /// `E043` for an unordered table or `to` out of range.
    pub(crate) fn reorder(&mut self, loc: Loc, to: u32) -> Result<Loc, Code> {
        let len = self.table_len(loc.table);
        if !loc.table.ordered() || to as usize >= len {
            return Err(Code::E043);
        }
        let kind = self.kind_at(loc);
        self.move_rows(loc.table, loc.row as usize, to as usize);
        let moved = Loc {
            table: loc.table,
            row: to,
        };
        assert_eq!(self.table_len(loc.table), len, "a reorder keeps every row");
        assert_eq!(self.kind_at(moved), kind, "the record moved to row `to`");
        Ok(moved)
    }

    /// Replaces the header's attribute lines with those of a validated header record.
    pub(crate) fn put_header(&mut self, rec: &RecordBuf) {
        assert_eq!(rec.kind, RecordKind::Header);
        assert!(
            rec.verbatim.is_empty(),
            "validated: the header has no verbatim lines"
        );
        assert!(
            rec.comments.is_empty(),
            "validated: the header has no comments"
        );
        self.pool.orphan(self.header, 0);
        let mut scratch = Scratch::default();
        scratch.fill_buf(rec);
        self.header =
            self.pool
                .push_attrs(&scratch.view(RecordKind::Header), &[], &mut self.strings);
        assert_eq!(self.header.len as usize, rec.attrs.len());
    }

    /// Validates a record for this document's file kind (see `check_record`).
    pub(crate) fn check(&self, rec: &RecordBuf) -> Result<(), Code> {
        record::check_record(rec, self.kind).map_err(|e| e.code)
    }

    /// Finds a record by kind and key: id text for id kinds, name for named kinds,
    /// draw-order index for graphics. The header is not a table row.
    pub(crate) fn locate(&self, kind: RecordKind, key: &str) -> Option<Loc> {
        let t = TableId::of(kind)?;
        let loc = match kind.key() {
            KeyRule::Id => self
                .find_id(ObjId::find(key, &self.strings)?)
                .filter(|l| l.table == t),
            KeyRule::Name => {
                let name = self.strings.find(key)?;
                let row = with_table!(self, t, |tab| (0..tab.len())
                    .find(|&r| tab.name_at(r) == name && tab.kind_at(r) == kind));
                row.map(|row| Loc {
                    table: t,
                    row: row as u32,
                })
            }
            KeyRule::Position => {
                let row: u32 = key.parse().ok()?;
                (row < self.graphics.len() as u32).then_some(Loc { table: t, row })
            }
            KeyRule::Header | KeyRule::TypeName => None,
        };
        let loc = loc.filter(|l| self.kind_at(*l) == kind);
        assert!(loc.is_none_or(|l| l.table == t));
        loc
    }

    /// Returns the record kind at `loc`.
    pub(crate) fn kind_at(&self, loc: Loc) -> RecordKind {
        assert!((loc.row as usize) < self.table_len(loc.table), "stale row");
        with_table!(self, loc.table, |tab| tab.kind_at(loc.row as usize))
    }

    /// Returns the key text addressing `loc` in op text (id, name, or graphic index).
    pub(crate) fn key_of(&self, loc: Loc) -> String {
        let row = loc.row as usize;
        let rule = self.kind_at(loc).key();
        let key = match rule {
            KeyRule::Id => with_table!(self, loc.table, |tab| tab.id_at(row, &self.strings))
                .text(&self.strings),
            KeyRule::Name => {
                let name = with_table!(self, loc.table, |tab| tab.name_at(row));
                self.strings.get(name).to_owned()
            }
            KeyRule::Position => row.to_string(),
            KeyRule::Header | KeyRule::TypeName => {
                unreachable!("rows are id, name or position keyed")
            }
        };
        assert!(!key.is_empty() || rule == KeyRule::Name);
        key
    }

    /// Spec §4 kind rules (`E025`), the same rules the reader applies, so a document that
    /// passes is a file that parses.
    pub(crate) fn kind_rules_ok(&self) -> bool {
        let impls = self.impls.len();
        let schematic = (0..impls)
            .filter(|&r| self.strings.get(self.impls.fields[r][1]) == "schematic")
            .count();
        assert!(schematic <= impls);
        assert!(
            self.kind != FileKind::Shbench || impls == 0,
            "benches have no impls"
        );
        let ok = match self.kind {
            FileKind::Shprim => self.pins.len() >= 1 && impls >= 1 && schematic == 0,
            FileKind::Shcomp => schematic == 1,
            FileKind::Shbench => true,
            FileKind::Project
            | FileKind::Lock
            | FileKind::Config
            | FileKind::Journal
            | FileKind::Techmap => unreachable!("documents are cell files (`Document::new`)"),
        };
        let cell = matches!(self.kind, FileKind::Shprim | FileKind::Shcomp);
        ok && !(cell && impls > 1 && self.header_value("default").is_none())
    }

    /// In-file references whose target does not exist (`E042`): `waive` → instance id,
    /// constraint `member` → any id, `body` → impl name, `spec` → output name, header
    /// `default` → impl name. Sorted.
    pub(crate) fn dangling(&self) -> Vec<Dangling> {
        let mut out = Vec::new();
        let s = &self.strings;
        let id_row = |text: &str| ObjId::find(text, s).and_then(|id| self.find_id(id));
        for r in 0..self.waivers.len() {
            let target = s.get(self.waivers.fields[r][1]);
            if id_row(target).is_none_or(|l| l.table != TableId::Instances) {
                out.push(Dangling {
                    kind: RecordKind::Waive,
                    key: s.get(self.waivers.fields[r][0]).to_owned(),
                    target: target.to_owned(),
                });
            }
        }
        for r in 0..self.constraints.len() {
            for (k, vals) in self.pool.attrs(self.constraints.attrs[r]) {
                let target = vals.first().map_or("", |v| s.get(*v));
                if s.get(k) == "member" && id_row(target).is_none() {
                    out.push(Dangling {
                        kind: RecordKind::Constraint,
                        key: s.get(self.constraints.fields[r][0]).to_owned(),
                        target: target.to_owned(),
                    });
                }
            }
        }
        for (table, kind, named) in [
            (&self.bodies, RecordKind::Body, RecordKind::Impl),
            (&self.specs, RecordKind::Spec, RecordKind::Output),
        ] {
            for r in 0..table.len() {
                let name = s.get(table.fields[r][0]);
                if self.locate(named, name).is_none() {
                    out.push(Dangling {
                        kind,
                        key: name.to_owned(),
                        target: name.to_owned(),
                    });
                }
            }
        }
        if let Some(d) = self.header_value("default")
            && self.locate(RecordKind::Impl, d).is_none()
        {
            out.push(Dangling {
                kind: RecordKind::Header,
                key: String::new(),
                target: d.to_owned(),
            });
        }
        out.sort();
        let members = self.pool.attr_key.len(); // a loose bound: every attribute line
        let bound = self.waivers.len() + members + self.bodies.len() + self.specs.len() + 1;
        assert!(out.len() <= bound);
        out
    }

    /// Returns whether the wire at `loc` has equal endpoints (`E026`).
    pub(crate) fn wire_zero(&self, loc: Loc) -> bool {
        assert_eq!(loc.table, TableId::Wires);
        let (r, w) = (loc.row as usize, &self.wires);
        w.x1[r] == w.x2[r] && w.y1[r] == w.y2[r]
    }
}

// ---- private ----

impl Document {
    /// Drops the id index entry of the row at `loc` (when it points there) and counts the
    /// row's pool entries as garbage: the first half of replacing or deleting a row.
    /// Returns the row's id.
    fn unindex(&mut self, loc: Loc) -> ObjId {
        let row = loc.row as usize;
        assert!(row < self.table_len(loc.table), "stale row");
        let id = with_table!(self, loc.table, |tab| tab.id_at(row, &self.strings));
        if id != ObjId::NONE && self.by_id.get(&id) == Some(&loc) {
            self.by_id.remove(&id);
        }
        let Owned { attrs, others } = with_table!(self, loc.table, |tab| tab.owned(row));
        self.pool.orphan(attrs, others);
        assert!(self.by_id.get(&id) != Some(&loc));
        id
    }

    fn default_position(&self, t: TableId, kind: RecordKind) -> usize {
        assert!(t.ordered());
        assert_eq!(TableId::of(kind), Some(t));
        let pos = with_table!(self, t, |tab| {
            match (0..tab.len()).rev().find(|&r| tab.kind_at(r) == kind) {
                Some(r) => r + 1,
                None => tab.len(),
            }
        });
        assert!(pos <= self.table_len(t));
        pos
    }

    fn move_rows(&mut self, t: TableId, from: usize, to: usize) {
        let len = self.table_len(t);
        assert!(t.ordered(), "rows move only in ordered tables");
        assert!(from < len, "stale row");
        assert!(to < len, "callers check the destination row");
        if from == to {
            return;
        }
        with_table_mut!(self, t, |tab, _s, _p| tab.move_row(from, to));
        self.reindex(t, from.min(to)..from.max(to) + 1, Write::Moved);
        assert_eq!(self.table_len(t), len);
    }

    /// Re-points `by_id` at the rows in `rows` after they were written (`what`), and logs
    /// each write.
    fn reindex(&mut self, t: TableId, rows: Range<usize>, what: Write) {
        let len = self.table_len(t);
        assert!(rows.start <= rows.end);
        assert!(rows.start <= len);
        for row in rows.start..rows.end.min(len) {
            let id = with_table!(self, t, |tab| tab.id_at(row, &self.strings));
            self.log.push(t, what, row as u32, id);
            if id != ObjId::NONE {
                let loc = Loc {
                    table: t,
                    row: row as u32,
                };
                self.by_id.insert(id, loc);
            }
        }
    }

    /// Fails when `rec`'s id or name is held by a record other than `own`.
    fn check_key_free(&self, rec: &RecordBuf, own: Option<Loc>) -> Result<(), Code> {
        assert!(own.is_none_or(|l| TableId::of(rec.kind) == Some(l.table)));
        assert!(
            rec.kind.key() == KeyRule::Position || !rec.fields.is_empty(),
            "validated"
        );
        let key = rec.fields.first().map_or("", String::as_str);
        let taken = |loc: Option<Loc>| loc.is_some_and(|l| Some(l) != own);
        match rec.kind.key() {
            KeyRule::Id
                if taken(ObjId::find(key, &self.strings).and_then(|id| self.find_id(id))) =>
            {
                Err(Code::E041)
            }
            KeyRule::Name if taken(self.locate(rec.kind, key)) => Err(Code::E024),
            _ => Ok(()),
        }
    }
}
