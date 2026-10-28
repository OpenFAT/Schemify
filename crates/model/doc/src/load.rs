//! A parsed format model → a [`Document`]'s tables (load), the tables → a format model
//! (save), and the rebuild that does both to drop garbage (compaction).
//! Cites: format spec §4 (ranks), §5 (newer versions read-only), §7 (`W001`, `E024`,
//! `E042`); D39 (compaction off the edit frame).
//!
//! Load spine: header attributes → one `push_row` per record into its table → `reid_dups`
//! (`W001`) → `load_diags` (`E024` duplicate overrides, `E042` dangling references).
//! Save spine: header → every table in rank order → end-of-file comments; the format
//! writer then sorts ranks 3 and 6.
//!
//! Contents (crate methods of `Document`):
//! - `from_format(src: &FormatDoc) -> Loaded`, `to_format(&self) -> FormatDoc<'static>`
//! - `compact_due(&self, k: usize) -> bool`, `compact(&mut self)`

use crate::changes::ChangeLog;
use crate::document::{Document, Loaded, Loc, TableId};
use crate::pool::{FormatSink, Pool, RecView, Scratch, Span, emit_attrs};
use crate::strings::{ObjId, Strings};
use crate::tables::Table;
use crate::{Code, Diag};
use schemify_format::{Document as FormatDoc, IdRng, RecordKind, fresh_id};

impl Document {
    /// Loads tables from a parsed format model, re-iding `W001` duplicates and reporting
    /// what the format layer cannot see (see module docs). Never fails: format errors
    /// were caught by the parser; semantic findings are diagnostics.
    pub(crate) fn from_format(src: &FormatDoc<'_>) -> Loaded {
        let kind = src.kind();
        assert!(src.record_count() >= 1);
        let mut doc = Document::new(kind);
        doc.version = src.version();
        doc.by_id.reserve(src.record_count() as usize);
        let mut scratch = Scratch::default();
        scratch.fill_format(src, 0);
        doc.header = push_header(
            &mut doc.pool,
            &mut doc.strings,
            &scratch.view(RecordKind::Header),
        );
        let mut locs = Vec::with_capacity(src.record_count() as usize);
        locs.push(None);
        for r in 1..src.record_count() {
            let rk = src.record_kind(r);
            let Some(t) = TableId::of(rk) else {
                assert_eq!(rk, RecordKind::Unknown, "cell files hold only cell kinds");
                doc.unknown_records += 1;
                locs.push(None);
                continue;
            };
            scratch.fill_format(src, r);
            locs.push(Some(doc.push_row(t, &scratch.view(rk))));
        }
        for c in src.eof_comments() {
            let s = doc.strings.intern(c);
            doc.eof_comments.push(s);
        }
        let mut diags = doc.reid_dups(src, &locs);
        diags.extend(doc.load_diags());
        doc.log = ChangeLog::default(); // a loaded document starts its log empty
        doc.strings.settle();
        assert!(doc.unknown_records == 0 || doc.read_only());
        Loaded { doc, diags }
    }

    /// Builds the format model of this document: header, then every table in rank
    /// order (instances write `at` then `name` first), then end-of-file comments.
    ///
    /// # Panics
    ///
    /// Panics on a read-only document (it lost the records it cannot model).
    pub(crate) fn to_format(&self) -> FormatDoc<'static> {
        assert!(!self.read_only(), "a read-only document is never written");
        let mut sink = FormatSink::new(FormatDoc::new(self.kind));
        emit_attrs(self.header, &self.strings, &self.pool, &mut sink);
        for t in TableId::ALL {
            with_table!(self, t, |tab| {
                for row in 0..tab.len() {
                    tab.emit(row, &self.strings, &self.pool, &mut sink);
                }
            });
        }
        let mut out = sink.doc;
        for &c in &self.eof_comments {
            out.push_eof_comment(self.strings.get(c));
        }
        assert_eq!(out.kind(), self.kind);
        out
    }

    /// Returns whether garbage (pool entries of replaced rows, strings edits interned) is
    /// at least `k` times the live data, and at least `k`·2^16 entries: [`Document::compact`]
    /// is due. `Project` compacts at `k = 1` in an idle step and at `k = 3` inside a
    /// commit, for a caller that never idles (the CLI, a script). Cost: O(1).
    pub(crate) fn compact_due(&self, k: usize) -> bool {
        assert!(k >= 1);
        let dead = self.pool.dead as usize;
        let size = self.pool.size();
        assert!(dead <= size, "garbage is part of the pool");
        let pool = dead >= k << 16 && dead * (k + 1) >= size * k;
        pool || self.strings.outgrown(k as u32)
    }

    /// Rebuilds the arena and pool from the live rows, so a long session of edits stays
    /// bounded. Row order and ids are kept; `Sym` and `Span` values change and the change
    /// log breaks (a reader rebuilds, as after a reload). Cost: O(document), ~100 ms at
    /// 100k objects, so `Project` runs it in an idle step.
    pub(crate) fn compact(&mut self) {
        let rebuilt = Document::from_format(&self.to_format()).doc;
        assert_eq!(rebuilt.instances.len(), self.instances.len());
        // Strings and rows are renumbered: the log goes on, but no earlier cursor reads on.
        let mut log = std::mem::take(&mut self.log);
        log.break_here();
        *self = rebuilt;
        self.log = log;
        assert_eq!(self.pool.dead, 0);
        assert!(!self.compact_due(1));
    }
}

// ---- private ----

fn push_header(pool: &mut Pool, strings: &mut Strings, rec: &RecView<'_>) -> Span {
    assert!(rec.verbatim.is_empty());
    assert!(rec.comments.is_empty());
    let span = pool.push_attrs(rec, &[], strings);
    assert_eq!(span.len as usize, rec.attrs.len());
    span
}

impl Document {
    /// Gives every `W001` duplicate a fresh id, deterministically: the generator is
    /// seeded from the old id text and the duplicate's ordinal (`IdRng::for_import`), so
    /// reloading the same bytes always yields the same ids. References (`waive`
    /// targets, constraint `member`s) keep pointing at the first occurrence.
    fn reid_dups(&mut self, src: &FormatDoc<'_>, locs: &[Option<Loc>]) -> Vec<Diag> {
        let mut diags = Vec::new();
        for (ordinal, w) in src.warnings().iter().enumerate() {
            assert_eq!(w.code, schemify_format::Code::W001);
            let Some(loc) = locs[w.record as usize] else {
                continue;
            };
            let mut rec = self.record(loc);
            let mut rng = IdRng::for_import(&rec.fields[0], ordinal as u32);
            let (by_id, strings) = (&self.by_id, &self.strings);
            let new = fresh_id(&mut rng, |t| {
                ObjId::find(t, strings).is_some_and(|id| by_id.contains_key(&id))
            });
            rec.fields[0] = new.as_str().to_owned();
            self.replace_row(loc, &rec);
            diags.push(Diag {
                code: Code::W001,
                line: w.line,
                detail: "duplicate id re-assigned",
            });
        }
        assert_eq!(diags.len(), src.warnings().len());
        diags
    }

    /// Findings on a loaded document the format layer does not report: duplicate
    /// override keys on one instance (`E024`) and dangling in-file references (`E042`).
    fn load_diags(&self) -> Vec<Diag> {
        let mut diags = Vec::new();
        let inherit = self.strings.find("inherit");
        let mut keys = Vec::new();
        for &span in &self.instances.attrs {
            // `inherit <pin> <net>` is repeatable, one line per pin (format spec §4),
            // so it is keyed by (inherit, pin); every other key must be unique.
            keys.clear();
            keys.extend(self.pool.attrs(span).map(|(k, vals)| {
                let pin = (Some(k) == inherit)
                    .then(|| vals.first().copied())
                    .flatten();
                (k, pin)
            }));
            if (1..keys.len()).any(|i| keys[..i].contains(&keys[i])) {
                diags.push(Diag {
                    code: Code::E024,
                    line: 0,
                    detail: "instance overrides the same key twice",
                });
            }
        }
        for _ in self.dangling() {
            diags.push(Diag {
                code: Code::E042,
                line: 0,
                detail: "reference to a record that does not exist",
            });
        }
        assert!(diags.iter().all(|d| !d.code.is_warning()));
        diags
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordBuf;
    use schemify_format::FileKind;

    fn poly(dx: i32) -> RecordBuf {
        let mut r = RecordBuf::new(RecordKind::Shape)
            .field("0abcde")
            .field("poly");
        for i in 0..256 {
            r = r.field(&(i + dx).to_string());
        }
        r
    }

    #[test]
    fn replaced_coordinates_are_garbage_and_compact_away() {
        let mut doc = Document::new(FileKind::Shcomp);
        let loc = doc.insert(&poly(0), None).unwrap();
        let dead = doc.pool.dead;
        doc.put(loc, &poly(1)).unwrap();
        assert_eq!(doc.pool.dead - dead, 256, "the replaced row's coordinates");
        for k in 2..1000 {
            doc.put(loc, &poly(k)).unwrap();
            if doc.compact_due(1) {
                doc.compact();
            }
        }
        assert!(
            doc.pool.coords.len() <= 2 * (1 << 16) + 512,
            "{} coordinates for 256 live ones",
            doc.pool.coords.len()
        );
        assert_eq!(doc.record(loc), poly(999));
    }

    #[test]
    fn replaced_strings_are_garbage_and_compact_away() {
        let mut doc = Document::new(FileKind::Shcomp);
        let prose = |k: usize| {
            RecordBuf::new(RecordKind::Prose)
                .field("0abcde")
                .verbatim(&format!("{k} {}", "x".repeat(1 << 16)))
        };
        let loc = doc.insert(&prose(0), None).unwrap();
        for k in 1..200 {
            doc.put(loc, &prose(k)).unwrap();
            if doc.compact_due(1) {
                doc.compact();
            }
        }
        assert!(
            doc.strings().len() <= 32,
            "{} strings for 2 live ones",
            doc.strings().len()
        );
        assert_eq!(doc.record(loc), prose(199));
    }
}
