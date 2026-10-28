//! The page's `doc` edits: a var's expression, a prose block's text, added, moved and
//! deleted blocks. One transaction per edit.
//!
//! Contents:
//! - `pub(super) fn target`
//! - `pub fn var_ops`
//! - `pub fn prose_ops`
//! - `pub fn insert_row`
//! - `pub fn add_prose`
//! - `pub fn add_var`
//! - `pub fn move_block`
//! - `pub fn delete_block`

use schemify_doc::{Document, Loc, NEW_ID, Op, RecordBuf, RecordKind, TableId, Target};

use crate::edit::Toward;

use super::{kind_of, prose_text};

pub(super) fn target(doc: &Document, row: u32) -> Option<Target> {
    let b = doc.doc_blocks();
    let kind = *b.kind.get(row as usize)?;
    assert_eq!(b.fields.len(), b.kind.len(), "block columns in step");
    let key = doc.strings().get(b.fields[row as usize][0]);
    assert!(!key.is_empty(), "every block has its id or name");
    Some(Target::new(kind, key))
}

/// Ops setting var `row`'s expression to `expr` (D20: a stale `cached` value goes with the
/// old expression). Empty when nothing changes or `row` is not a var.
///
/// # Panics
///
/// Never: the assert checks the op count (the expression, then its stale cache).
#[must_use]
pub fn var_ops(doc: &Document, row: u32, expr: &str) -> Vec<Op> {
    let expr = expr.trim();
    let b = doc.doc_blocks();
    let Some(t) = target(doc, row).filter(|t| t.kind == RecordKind::Var) else {
        return Vec::new();
    };
    let s = doc.strings();
    if expr.is_empty() || s.get(b.fields[row as usize][1]) == expr {
        return Vec::new();
    }
    let mut ops = vec![Op::SetField {
        target: t.clone(),
        index: 1,
        value: expr.to_owned(),
    }];
    if doc
        .pool()
        .find_attr(s, b.attrs[row as usize], "cached")
        .is_some()
    {
        ops.push(Op::RemoveAttr {
            target: t,
            key: "cached".into(),
            first: None,
        });
    }
    assert!(
        ops.len() <= 2,
        "the expression, then at most its stale cache"
    );
    ops
}

/// Ops replacing prose `row`'s text (an all-blank text deletes the block). Empty when
/// nothing changes or `row` is not prose.
///
/// # Panics
///
/// Never: the assert checks that a non-blank text gives a line.
#[must_use]
pub fn prose_ops(doc: &Document, row: u32, text: &str) -> Vec<Op> {
    let Some(t) = target(doc, row).filter(|t| t.kind == RecordKind::Prose) else {
        return Vec::new();
    };
    if prose_text(doc, row) == text {
        return Vec::new();
    }
    if text.trim().is_empty() {
        return vec![Op::Delete(t)];
    }
    let mut rec = doc.record(Loc {
        table: TableId::DocBlocks,
        row,
    });
    rec.verbatim = text.lines().map(|l| l.trim_end().to_owned()).collect();
    assert!(!rec.verbatim.is_empty(), "a non-blank text has a line");
    vec![Op::Put { target: t, rec }]
}

/// Row a new block goes to: after the selection, else at the end.
///
/// # Panics
///
/// Never: the row is clamped to one past the last block.
#[must_use]
pub fn insert_row(doc: &Document, sel: Option<u32>) -> u32 {
    let n = block_count(doc);
    let row = sel.map_or(n, |s| s.saturating_add(1).min(n));
    assert!(row <= n, "a new block goes at most one past the end");
    row
}

/// Op inserting a new prose block at `row`.
#[must_use]
pub fn add_prose(row: u32) -> Vec<Op> {
    let rec = RecordBuf::new(RecordKind::Prose)
        .field(NEW_ID)
        .verbatim("New text: markdown with $inline$ and $$display$$ math.");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op inserting a new var `v<k>` (the first free name) with value 0 at `row`.
///
/// # Panics
///
/// If `row` is more than one past the last block.
#[must_use]
pub fn add_var(doc: &Document, row: u32) -> Vec<Op> {
    let b = doc.doc_blocks();
    let s = doc.strings();
    let taken = |name: &str| {
        (0..b.kind.len()).any(|r| b.kind[r] == RecordKind::Var && s.get(b.fields[r][0]) == name)
    };
    // Bounded by the block capacity: at most n names are taken.
    let name = (1..=b.kind.len() + 1)
        .map(|k| format!("v{k}"))
        .find(|n| !taken(n))
        .expect("n blocks leave one of v1..=v(n + 1) free");
    assert!(row <= block_count(doc), "insert at most one past the end");
    let rec = RecordBuf::new(RecordKind::Var).field(&name).field("0");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op moving block `row` one place, or nothing at the edge or for a stale row.
///
/// # Panics
///
/// Never: the assert checks that the block moves to another row.
#[must_use]
pub fn move_block(doc: &Document, row: u32, toward: Toward) -> Vec<Op> {
    let n = block_count(doc);
    let to = match toward {
        Toward::Earlier => row.checked_sub(1),
        Toward::Later => row.checked_add(1),
    };
    match (target(doc, row), to.filter(|&to| to < n)) {
        (Some(t), Some(to)) => {
            assert_ne!(to, row);
            vec![Op::Reorder { target: t, to }]
        }
        _ => Vec::new(),
    }
}

/// Op deleting block `row`; none for a stale row.
///
/// # Panics
///
/// Never: the assert checks that only a stale `row` gives no ops.
pub fn delete_block(doc: &Document, row: u32) -> Vec<Op> {
    let ops: Vec<Op> = target(doc, row).map(Op::Delete).into_iter().collect();
    assert_eq!(ops.is_empty(), kind_of(doc, row).is_none());
    ops
}

// ---- private ----

fn block_count(doc: &Document) -> u32 {
    u32::try_from(doc.doc_blocks().kind.len()).expect("doc blocks < 2^12")
}
