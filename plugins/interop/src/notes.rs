//! The interop report (spec `interop.md` §12): one row per thing a conversion did not carry
//! one-to-one, written as `interop-report.md` and `interop-report.txt`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Rows a report keeps; further rows are counted, not stored (a bound on memory).
pub const NOTES_MAX: usize = 1 << 20;

/// One report row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Note {
    /// `needs_review:<reason>`, `dropped:<reason>`, `rounding`, `off-grid`, `remapped`,
    /// `clamped` or `updated` (§12).
    pub class: String,
    /// The cell (`lib/cell` or a project path) the row is about.
    pub cell: String,
    /// What and where, free text.
    pub detail: String,
}

/// The rows of one conversion, in the order they were added.
#[derive(Clone, Debug, Default)]
pub struct Notes {
    /// Stored rows.
    pub rows: Vec<Note>,
    /// Rows beyond [`NOTES_MAX`].
    pub overflow: u64,
}

impl Notes {
    /// Adds a row.
    pub fn add(&mut self, class: &str, cell: &str, detail: impl Into<String>) {
        if self.rows.len() >= NOTES_MAX {
            self.overflow += 1;
            return;
        }
        self.rows.push(Note {
            class: class.to_owned(),
            cell: cell.to_owned(),
            detail: detail.into(),
        });
    }

    /// `needs_review:<reason>`.
    pub fn review(&mut self, reason: &str, cell: &str, detail: impl Into<String>) {
        self.add(&format!("needs_review:{reason}"), cell, detail);
    }

    /// `dropped:<reason>`.
    pub fn dropped(&mut self, reason: &str, cell: &str, detail: impl Into<String>) {
        self.add(&format!("dropped:{reason}"), cell, detail);
    }

    /// Rows per class, sorted.
    pub fn counts(&self) -> BTreeMap<&str, usize> {
        let mut m = BTreeMap::new();
        for r in &self.rows {
            *m.entry(r.class.as_str()).or_insert(0) += 1;
        }
        m
    }

    /// Tab-separated `class cell detail` lines, sorted, for diffs and tests.
    pub fn text(&self) -> String {
        let mut rows: Vec<&Note> = self.rows.iter().collect();
        rows.sort();
        let mut out = String::new();
        for r in rows {
            writeln!(
                out,
                "{}\t{}\t{}",
                r.class,
                r.cell,
                r.detail.replace(['\t', '\n'], " ")
            )
            .unwrap();
        }
        if self.overflow > 0 {
            writeln!(out, "overflow\t-\t{} more rows", self.overflow).unwrap();
        }
        out
    }

    /// A Markdown summary: counts per class, then the rows grouped by class.
    pub fn markdown(&self, title: &str) -> String {
        let mut out = format!("# {title}\n\n| Class | Rows |\n|---|---|\n");
        for (c, n) in self.counts() {
            writeln!(out, "| `{c}` | {n} |").unwrap();
        }
        let mut rows: Vec<&Note> = self.rows.iter().collect();
        rows.sort();
        let mut last = "";
        for r in rows {
            if r.class != last {
                write!(out, "\n## `{}`\n\n", r.class).unwrap();
                last = &r.class;
            }
            writeln!(out, "- `{}`: {}", r.cell, r.detail.replace('\n', " ")).unwrap();
        }
        out
    }
}
