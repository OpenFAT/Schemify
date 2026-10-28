//! Patching snapshot groups in place: a document table refilled only on the rows that
//! differ when few do ([`PATCH_SHARE`]), the net columns and the per-cell groups synced.
//!
//! Contents:
//! - `pub(super) fn differing`
//! - `pub(super) fn patch_doc_groups`
//! - `pub(super) fn sync_cell_groups`

use std::borrow::Cow;
use std::ops::Range;

use schemify_elab::CellState;
use schemify_plugin::Snapshots;
use schemify_plugin::abi::{Table, col};

use super::snapshot::{Filled, NetShadow, fill_nets, fill_violations};

/// Rows of a table changed by more than this share are refilled instead of patched.
const PATCH_SHARE: usize = 8;
const _: () = assert!(PATCH_SHARE > 0);

/// The rows `0..n` for which `differs` holds, or `None` once they pass `n / PATCH_SHARE`
/// (the caller refills instead): the scan stops there, so a refill costs no index list.
pub(super) fn differing(n: usize, differs: impl Fn(usize) -> bool) -> Option<Vec<usize>> {
    let limit = n.max(64) / PATCH_SHARE;
    let mut out = Vec::new();
    for i in 0..n {
        if differs(i) {
            if out.len() == limit {
                return None;
            }
            out.push(i);
        }
    }
    assert!(out.len() <= limit);
    Some(out)
}

/// Brings the Instances, Wires and Labels tables (net columns in step, as `u32::MAX` for new
/// rows until [`sync_cell_groups`]) up to date with the document rows written since the
/// last fill (`changes`, the document's change log since then).
///
/// Cost: O(changed rows · columns) plus the moves of the string columns' tails.
pub(super) fn patch_doc_groups(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    changes: &[schemify_doc::Change],
    f: &mut Filled,
) {
    use schemify_doc::TableId as T;
    for table in [T::Instances, T::Wires, T::Labels] {
        let mut rows: Vec<u32> = changes
            .iter()
            .filter(|c| c.table == table)
            .map(|c| c.row)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        let snap = match table {
            T::Instances => Table::Instances,
            T::Wires => Table::Wires,
            _ => Table::Labels,
        };
        let (old, new) = (s.rows(snap) as usize, doc.table_len(table));
        if rows.is_empty() && old == new {
            continue;
        }
        let kept = old.min(new);
        if rows.len() * PATCH_SHARE > kept.max(64) {
            fill_doc_table(s, doc, id, snap, 0..old, 0..new);
        } else {
            for &r in &rows {
                let r = r as usize;
                if r < kept {
                    fill_doc_table(s, doc, id, snap, r..r + 1, r..r + 1);
                }
            }
            fill_doc_table(s, doc, id, snap, kept..old, kept..new);
        }
        // The net column keeps the table's row count; its values come from the cell.
        let (net, shadow) = match snap {
            Table::Wires => (col::wires::NET, &mut f.wire_net),
            Table::Labels => (col::labels::NET, &mut f.label_net),
            _ => continue,
        };
        let pad = vec![u32::MAX; new - kept];
        s.splice::<u32>(snap, net, kept..old, &pad);
        if let Some(v) = shadow.as_mut() {
            v.truncate(kept);
            v.extend_from_slice(&pad);
        }
        assert_eq!(s.rows(snap) as usize, new, "the table follows the document");
    }
}

/// Brings the net columns, the Nets and the Violations tables up to date with `cell` (the
/// document's state, `None`: none, so `u32::MAX` nets and empty tables): the net columns by
/// comparison, the Nets and Violations rows, when the check pass moved, by comparison with
/// the previous pass's when `cell` continues it (its names are then the same arena), else
/// refilled.
///
/// Cost: one compare pass over the net columns, the nets and the violations, plus the rows
/// written.
pub(super) fn sync_cell_groups(
    s: &mut Snapshots,
    id: u32,
    cell: Option<&CellState>,
    f: &mut Filled,
) {
    let (nw, nl) = (
        s.rows(Table::Wires) as usize,
        s.rows(Table::Labels) as usize,
    );
    // The cell's net ids of the rows, or `u32::MAX` (not elaborated) while it does not
    // describe them (the frame of an edit, before its check pass).
    fn nets_of<'a>(cell: Option<&CellState>, col: &'a [u32], n: usize) -> Cow<'a, [u32]> {
        match cell {
            Some(_) if col.len() == n => Cow::Borrowed(col),
            _ => Cow::Owned(vec![u32::MAX; n]),
        }
    }
    let want = nets_of(cell, cell.map_or(&[][..], |c| &c.nets.wire_net), nw);
    sync_net_column(s, Table::Wires, col::wires::NET, &mut f.wire_net, &want);
    let want = nets_of(cell, cell.map_or(&[][..], |c| &c.nets.label_net), nl);
    sync_net_column(s, Table::Labels, col::labels::NET, &mut f.label_net, &want);
    let key = (id, cell.map_or(0, |c| c.pass));
    if f.pass == Some(key) {
        return;
    }
    let empty = CellState::default();
    let c = cell.unwrap_or(&empty);
    let continues = cell.is_some_and(|c| c.prev_pass != 0 && f.pass == Some((id, c.prev_pass)));
    f.pass = Some(key);
    let n = &c.nets;
    let NetShadow {
        name,
        width,
        global,
        pins,
    } = &f.nets;
    assert_eq!(name.len(), width.len(), "shadow columns in step");
    assert_eq!(name.len(), pins.len(), "shadow columns in step");
    let same = continues
        && *name == n.net_name
        && *width == n.net_width
        && *global == n.net_global
        && *pins == n.net_pins;
    // Nets: rows are component handles (stable across a patch): write the rows that differ.
    match continues {
        _ if same => {}
        true => {
            let kept = name.len().min(n.net_name.len());
            let differ = differing(kept, |i| {
                name[i] != n.net_name[i]
                    || width[i] != n.net_width[i]
                    || global[i] != n.net_global[i]
                    || pins[i] != n.net_pins[i]
            });
            match differ {
                Some(differ) => {
                    for &i in &differ {
                        fill_nets(s, c, i..i + 1, i..i + 1);
                    }
                    fill_nets(s, c, kept..name.len(), kept..n.net_name.len());
                }
                None => fill_nets(s, c, 0..name.len(), 0..n.net_name.len()),
            }
        }
        false => {
            let rows = s.rows(Table::Nets) as usize;
            fill_nets(s, c, 0..rows, 0..n.net_name.len());
        }
    }
    if !same {
        // In place: the shadow keeps its allocation from pass to pass.
        f.nets.name.clone_from(&n.net_name);
        f.nets.width.clone_from(&n.net_width);
        f.nets.global.clone_from(&n.net_global);
        f.nets.pins.clone_from(&n.net_pins);
    }
    // Violations: the pass's own delta from the previous pass (`changes`), placed by key.
    let nv = s.rows(Table::Violations) as usize;
    let (gone, added) = &c.changes;
    if !continues || gone.len() + added.len() > 64 {
        fill_violations(s, c, 0..nv, 0..c.violations.rows.len());
    } else {
        let rows = &c.violations.rows;
        let rank = |list: &[schemify_elab::Violation], v: &schemify_elab::Violation| {
            list.partition_point(|x| x.key_cmp(v, &c.names).is_lt())
        };
        // Where each gone row was: the rows before it in the old order are the kept rows
        // before it (the new rows before it, less the added) and the gone rows before it.
        let mut at: Vec<usize> = gone
            .iter()
            .map(|g| rank(rows, g) - rank(added, g) + rank(gone, g))
            .collect();
        at.sort_unstable_by(|a, b| b.cmp(a));
        for &i in &at {
            fill_violations(s, c, i..i + 1, 0..0);
        }
        let mut to: Vec<usize> = added.iter().map(|a| rank(rows, a)).collect();
        to.sort_unstable();
        for &j in &to {
            fill_violations(s, c, j..j, j..j + 1);
        }
    }
    assert_eq!(
        s.rows(Table::Nets) as usize,
        c.nets.net_name.len(),
        "Nets follow the cell"
    );
    assert_eq!(
        s.rows(Table::Violations) as usize,
        c.violations.rows.len(),
        "Violations follow the cell"
    );
}

// ---- private ----

/// Writes document rows `rows` of table `snap` (all but the net column) over snapshot rows
/// `at`.
fn fill_doc_table(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    snap: Table,
    at: Range<usize>,
    rows: Range<usize>,
) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
    assert!(at.end <= s.rows(snap) as usize, "rows {at:?} of {snap:?}");
    let st = doc.strings();
    let n = rows.len();
    match snap {
        Table::Instances => {
            let t = doc.instances();
            s.splice::<u32>(snap, col::instances::DOC, at.clone(), &vec![id; n]);
            let raw: Vec<u32> = t.id[rows.clone()].iter().map(|i| i.0).collect();
            s.splice::<u32>(snap, col::instances::ID, at.clone(), &raw);
            s.splice::<i32>(snap, col::instances::X, at.clone(), &t.x[rows.clone()]);
            s.splice::<i32>(snap, col::instances::Y, at.clone(), &t.y[rows.clone()]);
            let o: Vec<u8> = t.orient[rows.clone()].iter().map(|&o| o as u8).collect();
            s.splice::<u8>(snap, col::instances::ORIENT, at.clone(), &o);
            let cells: Vec<&str> = t.cell[rows.clone()].iter().map(|&c| st.get(c)).collect();
            s.splice_strings(snap, col::instances::CELL, at.clone(), &cells);
            let names: Vec<&str> = t.name[rows].iter().map(|&c| st.get(c)).collect();
            s.splice_strings(snap, col::instances::NAME, at, &names);
        }
        Table::Wires => {
            let t = doc.wires();
            s.splice::<u32>(snap, col::wires::DOC, at.clone(), &vec![id; n]);
            let raw: Vec<u32> = t.id[rows.clone()].iter().map(|i| i.0).collect();
            s.splice::<u32>(snap, col::wires::ID, at.clone(), &raw);
            s.splice::<i32>(snap, col::wires::X1, at.clone(), &t.x1[rows.clone()]);
            s.splice::<i32>(snap, col::wires::Y1, at.clone(), &t.y1[rows.clone()]);
            s.splice::<i32>(snap, col::wires::X2, at.clone(), &t.x2[rows.clone()]);
            s.splice::<i32>(snap, col::wires::Y2, at, &t.y2[rows]);
        }
        _ => {
            let t = doc.labels();
            s.splice::<u32>(snap, col::labels::DOC, at.clone(), &vec![id; n]);
            let raw: Vec<u32> = t.id[rows.clone()].iter().map(|i| i.0).collect();
            s.splice::<u32>(snap, col::labels::ID, at.clone(), &raw);
            s.splice::<i32>(snap, col::labels::X, at.clone(), &t.x[rows.clone()]);
            s.splice::<i32>(snap, col::labels::Y, at.clone(), &t.y[rows.clone()]);
            let texts: Vec<&str> = t.text[rows].iter().map(|&x| st.get(x)).collect();
            s.splice_strings(snap, col::labels::TEXT, at, &texts);
        }
    }
}

/// Makes net column `column` of `table` hold `want`, writing only the rows that differ from
/// what it holds (`shadow`; unknown or of another length: refilled).
///
/// Cost: one compare pass over the column, plus the rows written.
fn sync_net_column(
    s: &mut Snapshots,
    table: Table,
    column: u32,
    shadow: &mut Option<Vec<u32>>,
    want: &[u32],
) {
    assert_eq!(s.rows(table) as usize, want.len(), "the column is in step");
    assert!(
        matches!(table, Table::Wires | Table::Labels),
        "a net column's table"
    );
    match shadow.as_mut().filter(|v| v.len() == want.len()) {
        Some(old) if old[..] == *want => {}
        Some(old) => {
            match differing(want.len(), |i| old[i] != want[i]) {
                Some(differ) => {
                    for &i in &differ {
                        s.splice::<u32>(table, column, i..i + 1, &want[i..=i]);
                    }
                }
                None => s.fill::<u32>(table, column, want),
            }
            old.copy_from_slice(want);
        }
        None => {
            s.fill::<u32>(table, column, want);
            match shadow.as_mut() {
                Some(v) => {
                    v.clear();
                    v.extend_from_slice(want);
                }
                None => *shadow = Some(want.to_vec()),
            }
        }
    }
    assert!(
        shadow.as_ref().is_some_and(|v| v[..] == *want),
        "the shadow holds the column"
    );
}
