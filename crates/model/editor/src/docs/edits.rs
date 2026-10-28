//! The page's `doc` edits: a var's expression, a prose block's text, added, moved and
//! deleted blocks. One transaction per edit.

use super::{kind_of, prose_text};
use schemify_doc::{Document, Loc, NEW_ID, Op, RecordBuf, RecordKind, TableId, Target};

pub(super) fn target(doc: &Document, row: u32) -> Option<Target> {
    let b = doc.doc_blocks();
    let kind = *b.kind.get(row as usize)?;
    let key = doc.strings().get(b.fields[row as usize][0]);
    Some(Target::new(kind, key))
}

/// Ops setting var `row`'s expression to `expr` (D20: a stale `cached` value goes with the
/// old expression). Empty when nothing changes or `row` is not a var.
pub fn var_ops(doc: &Document, row: u32, expr: &str) -> Vec<Op> {
    let expr = expr.trim();
    let b = doc.doc_blocks();
    if kind_of(doc, row) != Some(RecordKind::Var) || expr.is_empty() {
        return Vec::new();
    }
    let s = doc.strings();
    if s.get(b.fields[row as usize][1]) == expr {
        return Vec::new();
    }
    let t = target(doc, row).expect("row checked");
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
    ops
}

/// Ops replacing prose `row`'s text (an all-blank text deletes the block).
pub fn prose_ops(doc: &Document, row: u32, text: &str) -> Vec<Op> {
    if kind_of(doc, row) != Some(RecordKind::Prose) || prose_text(doc, row) == text {
        return Vec::new();
    }
    let t = target(doc, row).expect("row checked");
    if text.trim().is_empty() {
        return vec![Op::Delete(t)];
    }
    let mut rec = doc.record(Loc {
        table: TableId::DocBlocks,
        row,
    });
    rec.verbatim = text.lines().map(|l| l.trim_end().to_owned()).collect();
    vec![Op::Put { target: t, rec }]
}

/// Row a new block goes to: after the selection, else at the end.
pub fn insert_row(doc: &Document, sel: Option<u32>) -> u32 {
    let n = doc.doc_blocks().kind.len() as u32;
    sel.map_or(n, |s| (s + 1).min(n))
}

/// Op inserting a new prose block at `row`.
pub fn add_prose(row: u32) -> Vec<Op> {
    let rec = RecordBuf::new(RecordKind::Prose)
        .field(NEW_ID)
        .verbatim("New text: markdown with $inline$ and $$display$$ math.");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op inserting a new var `v<k>` (the first free name) with value 0 at `row`.
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
        .expect("a free name among n + 1 candidates");
    let rec = RecordBuf::new(RecordKind::Var).field(&name).field("0");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op moving block `row` by one (`up`), or nothing at the edge.
pub fn move_block(doc: &Document, row: u32, up: bool) -> Vec<Op> {
    let n = doc.doc_blocks().kind.len() as u32;
    let to = if up {
        row.checked_sub(1)
    } else {
        Some(row + 1)
    };
    match (target(doc, row), to.filter(|&to| to < n)) {
        (Some(t), Some(to)) => vec![Op::Reorder { target: t, to }],
        _ => Vec::new(),
    }
}

/// Op deleting block `row`.
pub fn delete_block(doc: &Document, row: u32) -> Vec<Op> {
    target(doc, row).map(Op::Delete).into_iter().collect()
}
