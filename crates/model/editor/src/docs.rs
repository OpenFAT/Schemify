//! `docs`: the docs page of the active level (D19, D21, `docs/spec/docs-page.md`): its doc
//! blocks projected as UI nodes into the plugin-declared `docs.page` scroll pane, beside the
//! canvas, and the `doc` edit transactions the page makes (one per edit, so one undo). Also
//! the D20 cache write-back: plugin-computed param and var values copied into `cached`
//! attributes, and the rule that drops a cache whose expression changed.
//!
//! Data design:
//! 1. In: one document's `doc_blocks` rows and its [`CellState`] values. Out: state
//!    contribution records (UI nodes) and `doc` ops.
//! 2. How many: ≤ 2^12 blocks per cell (doc capacity); a page shows them all (not windowed;
//!    the note at [`contribute`] says why).
//! 3. Widths: block rows `u32` (< 2^12); var values are elab's [`Val`]s.
//! 4. Access: every block once per frame while the page is shown — the contribution text is
//!    compared and reparsed only when it changed (`Shell::set_state`).
//! 5. Lifetime: [`Page`] lives with the editor and resets when the active document changes.
//! 6. Parallel: no (one writer, a few hundred rows).
//!
//! Contents:
//! - `pub struct Page`, `Page::follow`
//! - `pub const IMAGES_FAILED_MAX`
//! - `pub const IMAGE_BYTES_MAX`
//! - `pub const IMAGE_SIDE_MAX`
//! - `pub fn row_of`
//! - `pub fn editor_id`
//! - `pub fn prose_text`
//! - `pub fn kind_of`
//! - `pub fn plot_exprs`
//! - `pub fn var_value`
//! - `pub fn value_text`
//! - `pub(crate) fn node`
//! - `pub(crate) fn attr`
//! - `pub fn quote`

mod cache;
mod edits;
mod images;
mod page;

use std::fmt::Write as _;

use schemify_doc::{DocHandle, Document, Loc, RecordKind, TableId};
use schemify_elab::{CellState, Val};
use schemify_ui::prose;
use schemify_ui::prose::Segment;

pub use cache::{cache_ops, drop_stale_caches, param_value};
pub use edits::{add_prose, add_var, delete_block, insert_row, move_block, prose_ops, var_ops};
pub use images::{image_path, load_image, load_images};
pub use page::{Extras, contribute};

/// The page's interaction state (owner: the editor).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    /// Document the state belongs to; another active document resets it.
    pub doc: Option<DocHandle>,
    /// Selected block row: target of insert (after it), move and delete.
    pub sel: Option<u32>,
    /// Prose block open in the text editor: its row and the text typed so far.
    pub editing: Option<(u32, String)>,
    /// The editor field has had keyboard focus (so losing it means "done").
    pub focused: bool,
    /// Prose image paths that failed to load (they show their alt text); retried when the
    /// page is shown for another document and back. At most [`IMAGES_FAILED_MAX`].
    pub images_failed: Vec<String>,
}

/// Failed image paths a page remembers; past it, the oldest is retried.
pub const IMAGES_FAILED_MAX: usize = 64;

/// Largest image file read for the docs page.
pub const IMAGE_BYTES_MAX: usize = 16 << 20;

/// Longest image side, in pixels.
pub const IMAGE_SIDE_MAX: u32 = 4096;

impl Page {
    /// Resets the state when the shown document changes.
    ///
    /// # Panics
    ///
    /// Never: the asserts check the state just reset.
    pub fn follow(&mut self, doc: DocHandle) {
        if self.doc != Some(doc) {
            *self = Page {
                doc: Some(doc),
                ..Page::default()
            };
        }
        assert_eq!(self.doc, Some(doc));
        assert!(self.images_failed.len() <= IMAGES_FAILED_MAX);
    }
}

/// Block row named by a page node id (`docs.b.<row>…`).
#[must_use]
pub fn row_of(node: &str) -> Option<u32> {
    let rest = node.strip_prefix("docs.b.")?;
    rest.split('.').next()?.parse().ok()
}

/// Node id of the prose editor of block `row`.
#[must_use]
pub fn editor_id(row: u32) -> String {
    let id = format!("docs.b.{row}.edit");
    debug_assert_eq!(
        row_of(&id),
        Some(row),
        "page ids round-trip through `row_of`"
    );
    id
}

/// The verbatim text of prose block `row` (lines joined by `\n`).
///
/// # Panics
///
/// `row` is not a prose block of `doc`.
#[must_use]
pub fn prose_text(doc: &Document, row: u32) -> String {
    assert_eq!(
        kind_of(doc, row),
        Some(RecordKind::Prose),
        "block {row} is not a prose block"
    );
    let rec = doc.record(Loc {
        table: TableId::DocBlocks,
        row,
    });
    assert_eq!(rec.kind, RecordKind::Prose);
    rec.verbatim.join("\n")
}

/// Kind of block `row`, if it exists.
#[must_use]
pub fn kind_of(doc: &Document, row: u32) -> Option<RecordKind> {
    doc.doc_blocks().kind.get(row as usize).copied()
}

/// The calculator expressions of every ```` ```plot ```` fence of `doc`'s prose blocks, in
/// page order (the slots the run fills, `docs-page.md` §5).
///
/// # Panics
///
/// Never: a document holds fewer than 2^12 blocks.
#[must_use]
pub fn plot_exprs(doc: &Document) -> Vec<String> {
    let b = doc.doc_blocks();
    let mut out = Vec::new();
    let rows = u32::try_from(b.kind.len()).expect("doc blocks < 2^12");
    for r in 0..rows {
        if b.kind[r as usize] != RecordKind::Prose {
            continue;
        }
        for part in prose::split_plots(&prose_text(doc, r)) {
            if let Segment::Plot(e) = part {
                out.push(e.trim().to_owned());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------- projection

/// Value and D20 flag of own var `v` (`name` guards against a stale state).
#[must_use]
pub fn var_value<'c>(cell: &'c CellState, name: &str, v: usize) -> Option<(&'c Val, bool)> {
    let g = &cell.graph;
    debug_assert_eq!(g.vals.len(), g.unverified.len(), "value columns in step");
    let n = *g.var_name.get(v)?;
    if cell.names.get(n) != name {
        return None;
    }
    let node = g.params as usize + v;
    Some((g.vals.get(node)?, *g.unverified.get(node)?))
}

/// Engineering text with the unit glued on (`5.2µm`, `20µA`, `15`).
#[must_use]
pub fn value_text(val: &Val, unit: &str) -> String {
    match val.as_f64() {
        Some(x) if matches!(val, Val::Num(_)) => {
            format!("{}{unit}", schemify_wave::format_si(x, 4))
        }
        _ => {
            let mut s = String::new();
            val.write_text(&mut s);
            if !unit.is_empty() {
                s.push(' ');
                s.push_str(unit);
            }
            s
        }
    }
}

/// Writes a `node` record header: `id` of `kind` under `parent`.
pub(crate) fn node(t: &mut String, id: &str, kind: &str, parent: &str) {
    assert!(!id.is_empty());
    assert!(
        !id.contains(char::is_whitespace),
        "a node id is one word: `{id}`"
    );
    assert!(!kind.is_empty());
    assert!(!parent.is_empty(), "node {id} has a parent");
    let _ = writeln!(t, "node {id} {kind}");
    let _ = writeln!(t, "  parent {parent}");
}

/// Writes one quoted attribute line of the current node.
pub(crate) fn attr(t: &mut String, key: &str, value: &str) {
    assert!(!key.is_empty());
    assert!(
        !key.contains(char::is_whitespace),
        "an attribute key is one word: `{key}`"
    );
    assert!(t.ends_with('\n'), "an attribute follows its node line");
    let _ = writeln!(t, "  {key} {}", quote(value));
}

/// Quotes a record value in the format §2 writer's canonical form, so any text
/// (multi-line prose, a status message with a newline) reads back unchanged.
/// The one quoting helper for doc pages, UI records and plugin arguments.
///
/// # Panics
///
/// Never: the assert checks that quoting only grows the text.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            // CR, the other C0 controls and DEL; C1 controls are written raw.
            c if c < ' ' || c == '\x7f' => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    assert!(out.len() >= s.len() + 2, "escapes only grow the text");
    out
}

// ---------------------------------------------------------------------- edits

// ---------------------------------------------------------------------- D20 cache

#[cfg(test)]
mod tests {
    use super::images::png_size;
    use super::*;
    use std::path::Path;

    #[test]
    fn rows_parse_from_node_ids_and_quotes_escape_lines() {
        assert_eq!(row_of("docs.b.12.expr"), Some(12));
        assert_eq!(row_of("docs.b.3"), Some(3));
        assert_eq!(row_of("docs.page"), None);
        assert_eq!(row_of("docs.b.x"), None);
        assert_eq!(quote("a\"b\\\nc\t"), "\"a\\\"b\\\\\\nc\\t\"");
        // Format §2: CR, other C0 and DEL as `\u{hex}`; C1 and the rest raw.
        assert_eq!(quote("\r\x01\x7f\u{85}é"), "\"\\u{d}\\u{1}\\u{7f}\u{85}é\"");
    }

    /// Prose names images inside the project only (the plugin ABI's rule); every way out is
    /// refused before any file is touched.
    #[test]
    fn image_paths_stay_inside_the_project() {
        let root = Path::new("proj");
        assert_eq!(image_path(root, "img/a.png"), Ok(root.join("img/a.png")));
        for bad in [
            "",
            "../a.png",
            "img/../../a.png",
            "/etc/passwd",
            "./a.png",
            "img//a.png",
            "img\\a.png",
            "C:/a.png",
            "a\0.png",
        ] {
            assert!(image_path(root, bad).is_err(), "{bad:?}");
        }
    }

    /// The size check reads the PNG header only, so a huge declared image is refused before
    /// its pixels are allocated; anything else is not a PNG.
    #[test]
    fn png_size_reads_the_header() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&100_000u32.to_be_bytes());
        b.extend_from_slice(&7u32.to_be_bytes());
        assert_eq!(png_size(&b), Some((100_000, 7)));
        assert_eq!(png_size(&b[..23]), None, "truncated");
        b[1] = b'J';
        assert_eq!(png_size(&b), None, "not the signature");
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn values_read_in_engineering_units() {
        assert_eq!(value_text(&Val::Num(5.2e-6), "m"), "5.2µm");
        assert_eq!(value_text(&Val::Int(15), ""), "15");
        assert_eq!(value_text(&Val::Bool(true), ""), "true");
    }
}
