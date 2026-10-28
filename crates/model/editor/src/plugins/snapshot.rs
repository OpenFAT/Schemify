//! Snapshot tables for plugins (plugin-abi §4): the documents, nets, cell values and
//! violations, filled in groups and patched per row when only a few rows changed.

use super::{Host, ResultRows, doc_id};
use schemify_doc::{Cursor, DocHandle, Project};
use schemify_elab::{CellState, Name};
use schemify_plugin::Snapshots;
use schemify_plugin::abi::{Table, col};

/// Snapshot groups [`Host::snapshot`] refills independently: Instances; Wires and Labels
/// without their net column; Nets with Violations; Results; the Wires and the Labels net
/// columns (a check pass renumbers nets without moving a wire).
pub(super) const GROUPS: usize = 7;

/// Fills the snapshot groups flagged `stale` ([`GROUPS`]) from document `doc` (snapshot id
/// `id`), its state `cell` and the run's `results`; see [`Host::snapshot`].
pub(super) fn fill_groups(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    cell: Option<&CellState>,
    results: &ResultRows,
    stale: [bool; GROUPS],
) {
    let st = doc.strings();
    let (inst, w, l) = (doc.instances(), doc.wires(), doc.labels());
    // Net ids are elab's component rows; u32::MAX = not elaborated.
    let nets_of = |col: &[u32], n: usize| match cell {
        Some(_) if col.len() == n => col.to_vec(),
        _ => vec![u32::MAX; n],
    };
    if stale[0] {
        let n = inst.id.len();
        s.fill::<u32>(Table::Instances, col::instances::DOC, &vec![id; n]);
        let raw: Vec<u32> = inst.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Instances, col::instances::ID, &raw);
        s.fill::<i32>(Table::Instances, col::instances::X, &inst.x);
        s.fill::<i32>(Table::Instances, col::instances::Y, &inst.y);
        let o: Vec<u8> = inst.orient.iter().map(|&o| o as u8).collect();
        s.fill::<u8>(Table::Instances, col::instances::ORIENT, &o);
        let cells: Vec<&str> = inst.cell.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::CELL, &cells);
        let names: Vec<&str> = inst.name.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::NAME, &names);
    }
    if stale[1] {
        let n = w.id.len();
        s.fill::<u32>(Table::Wires, col::wires::DOC, &vec![id; n]);
        let raw: Vec<u32> = w.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Wires, col::wires::ID, &raw);
        s.fill::<i32>(Table::Wires, col::wires::X1, &w.x1);
        s.fill::<i32>(Table::Wires, col::wires::Y1, &w.y1);
        s.fill::<i32>(Table::Wires, col::wires::X2, &w.x2);
        s.fill::<i32>(Table::Wires, col::wires::Y2, &w.y2);
    }
    if stale[5] {
        let n = w.id.len();
        s.fill::<u32>(
            Table::Wires,
            col::wires::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.wire_net), n),
        );
    }
    if stale[2] {
        let n = l.id.len();
        s.fill::<u32>(Table::Labels, col::labels::DOC, &vec![id; n]);
        let raw: Vec<u32> = l.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Labels, col::labels::ID, &raw);
        s.fill::<i32>(Table::Labels, col::labels::X, &l.x);
        s.fill::<i32>(Table::Labels, col::labels::Y, &l.y);
        let texts: Vec<&str> = l.text.iter().map(|&t| st.get(t)).collect();
        s.fill_strings(Table::Labels, col::labels::TEXT, &texts);
    }
    if stale[6] {
        let n = l.id.len();
        s.fill::<u32>(
            Table::Labels,
            col::labels::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.label_net), n),
        );
    }
    if stale[3] {
        let empty = CellState::default();
        let c = cell.unwrap_or(&empty);
        let nets = &c.nets;
        let names: Vec<&str> = nets.net_name.iter().map(|&n| c.names.get(n)).collect();
        s.fill_strings(Table::Nets, col::nets::NAME, &names);
        s.fill::<u16>(Table::Nets, col::nets::WIDTH, &nets.net_width);
        let global: Vec<u8> = nets.net_global.iter().map(|&g| u8::from(g)).collect();
        s.fill::<u8>(Table::Nets, col::nets::GLOBAL, &global);
        s.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &nets.net_pins);
        let v = &c.violations.rows;
        let codes: Vec<&str> = v.iter().map(|r| r.code.as_str()).collect();
        s.fill_strings(Table::Violations, col::violations::CODE, &codes);
        let sev: Vec<u8> = v.iter().map(|r| r.code.severity() as u8).collect();
        s.fill::<u8>(Table::Violations, col::violations::SEVERITY, &sev);
        let obj: Vec<u32> = v.iter().map(|r| r.object.0).collect();
        s.fill::<u32>(Table::Violations, col::violations::OBJECT, &obj);
        let none = schemify_elab::Name::NONE;
        let param: Vec<&str> = v
            .iter()
            .map(|r| {
                if r.param == none {
                    ""
                } else {
                    c.names.get(r.param)
                }
            })
            .collect();
        s.fill_strings(Table::Violations, col::violations::PARAM, &param);
    }
    if stale[4] {
        let jobs: Vec<u32> = results.iter().map(|r| r.0).collect();
        s.fill::<u32>(Table::Results, col::results::JOB, &jobs);
        let outs: Vec<&str> = results.iter().map(|r| r.1.as_str()).collect();
        s.fill_strings(Table::Results, col::results::OUTPUT, &outs);
        let vals: Vec<&str> = results.iter().map(|r| r.2.as_str()).collect();
        s.fill_strings(Table::Results, col::results::VALUE, &vals);
        let pass: Vec<u8> = results.iter().map(|r| r.3).collect();
        s.fill::<u8>(Table::Results, col::results::PASS, &pass);
    }
    // The caller completes the tables (a document group refilled without its net column
    // waits for `sync_cell_groups`) and checks them.
    debug_assert!(stale.iter().any(|&g| g));
}

/// What the snapshot's document and cell groups were filled from (see [`Host::snapshot`]), so
/// the next fill writes only what changed: the document groups follow the document's change
/// log, the cell groups are compared with what they hold.
#[derive(Debug, Default)]
pub(super) struct Filled {
    /// The document (snapshot id) and the change-log position its groups reflect.
    pub(super) doc: Option<(u32, Cursor)>,
    /// The document and check pass the Nets and Violations tables reflect (pass 0: no cell
    /// state); `None` before the first fill.
    pub(super) pass: Option<(u32, u64)>,
    /// What the Wires and Labels net columns hold (`None`: unknown, refill them).
    pub(super) wire_net: Option<Vec<u32>>,
    pub(super) label_net: Option<Vec<u32>>,
    /// The Nets columns as filled: name, width, global, pin count.
    pub(super) nets: (Vec<Name>, Vec<u16>, Vec<bool>, Vec<u32>),
    /// What the Results table was filled from.
    pub(super) results: Option<u64>,
}

/// Rows of a table changed by more than this share are refilled instead of patched.
pub(super) const PATCH_SHARE: usize = 8;

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
            for &r in rows.iter().filter(|&&r| (r as usize) < kept) {
                let r = r as usize;
                fill_doc_table(s, doc, id, snap, r..r + 1, r..r + 1);
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
        debug_assert_eq!(s.rows(snap) as usize, new);
    }
}

/// Writes document rows `rows` of table `snap` (all but the net column) over snapshot rows
/// `at`.
pub(super) fn fill_doc_table(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    snap: Table,
    at: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
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
pub(super) fn sync_net_column(
    s: &mut Snapshots,
    table: Table,
    column: u32,
    shadow: &mut Option<Vec<u32>>,
    want: &[u32],
) {
    assert_eq!(s.rows(table) as usize, want.len(), "the column is in step");
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
    use std::borrow::Cow;
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
    let (name, width, global, pins) = &f.nets;
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
        f.nets.0.clone_from(&n.net_name);
        f.nets.1.clone_from(&n.net_width);
        f.nets.2.clone_from(&n.net_global);
        f.nets.3.clone_from(&n.net_pins);
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
    debug_assert_eq!(s.rows(Table::Nets) as usize, c.nets.net_name.len());
    debug_assert_eq!(s.rows(Table::Violations) as usize, c.violations.rows.len());
}

/// Writes nets `rows` of `c` over Nets rows `at`.
pub(super) fn fill_nets(
    s: &mut Snapshots,
    c: &CellState,
    at: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
    let n = &c.nets;
    let names: Vec<&str> = n.net_name[rows.clone()]
        .iter()
        .map(|&x| c.names.get(x))
        .collect();
    s.splice_strings(Table::Nets, col::nets::NAME, at.clone(), &names);
    s.splice::<u16>(
        Table::Nets,
        col::nets::WIDTH,
        at.clone(),
        &n.net_width[rows.clone()],
    );
    let global: Vec<u8> = n.net_global[rows.clone()]
        .iter()
        .map(|&g| u8::from(g))
        .collect();
    s.splice::<u8>(Table::Nets, col::nets::GLOBAL, at.clone(), &global);
    s.splice::<u32>(Table::Nets, col::nets::PIN_COUNT, at, &n.net_pins[rows]);
}

/// Writes violations `rows` of `c` over Violations rows `at`.
pub(super) fn fill_violations(
    s: &mut Snapshots,
    c: &CellState,
    at: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
    let v = &c.violations.rows[rows];
    let codes: Vec<&str> = v.iter().map(|r| r.code.as_str()).collect();
    s.splice_strings(Table::Violations, col::violations::CODE, at.clone(), &codes);
    let sev: Vec<u8> = v.iter().map(|r| r.code.severity() as u8).collect();
    s.splice::<u8>(
        Table::Violations,
        col::violations::SEVERITY,
        at.clone(),
        &sev,
    );
    let obj: Vec<u32> = v.iter().map(|r| r.object.0).collect();
    s.splice::<u32>(Table::Violations, col::violations::OBJECT, at.clone(), &obj);
    let none = Name::NONE;
    let param: Vec<&str> = v
        .iter()
        .map(|r| {
            if r.param == none {
                ""
            } else {
                c.names.get(r.param)
            }
        })
        .collect();
    s.splice_strings(Table::Violations, col::violations::PARAM, at, &param);
}

/// A snapshot's debug text without its table epochs (they count fills, not content).
#[cfg(debug_assertions)]
pub(super) fn without_epochs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("epoch: ") {
        out.push_str(&rest[..i]);
        rest = rest[i + 7..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

/// The std hash of `v`.
pub(super) fn hash_of(v: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

impl Host {
    /// Refreshes the snapshot tables plugins read for the active document `h` (Docs,
    /// Instances, Wires, Labels, Selection), its derived state `cell` (net ids of wires and
    /// labels, Nets, Violations; `u32::MAX` nets and empty tables without it) and the last
    /// run's `results` rows. `key` names everything but `selection` (document epochs, the
    /// check pass, the results): with the previous call's key only the Selection table is
    /// refilled. Otherwise the Instances, Wires and Labels rows follow the document's change
    /// log from the last fill (refilled when it no longer reaches it: another document, a
    /// reload, a compaction), the net columns are compared with what they hold and the
    /// differing rows written, and the Nets and Violations tables, when the check pass moved,
    /// are compared with the previous pass's rows (refilled when `cell` does not continue
    /// it). Cost: O(changed rows) for the document tables, one compare pass over the net
    /// columns, nets and violations, plus the rows written (or the selection).
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &mut self,
        project: &Project,
        h: Option<DocHandle>,
        selection: &[u32],
        cell: Option<&CellState>,
        results: &ResultRows,
        key: u64,
    ) {
        let s = &mut self.snapshots;
        let id = h.map_or(0, doc_id);
        s.fill::<u32>(
            Table::Selection,
            col::selection::DOC,
            &vec![id; selection.len()],
        );
        s.fill::<u32>(Table::Selection, col::selection::ID, selection);
        if self.snapshot_key.replace(key) == Some(key) {
            debug_assert_eq!(s.check(), Ok(()));
            return; // a selection change only (a click): the document tables stand
        }
        let handles: Vec<DocHandle> = project.handles().collect();
        let ids: Vec<u32> = handles.iter().map(|&d| doc_id(d)).collect();
        s.fill::<u32>(Table::Docs, col::docs::HANDLE, &ids);
        let kinds: Vec<u8> = handles
            .iter()
            .map(|&d| project.doc(d).map_or(0, |x| x.kind() as u8))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::KIND, &kinds);
        let dirty: Vec<u8> = handles
            .iter()
            .map(|&d| u8::from(project.is_dirty(d)))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::DIRTY, &dirty);
        let empty: Vec<&str> = vec![""; handles.len()];
        s.fill_strings(Table::Docs, col::docs::PATH, &empty);
        s.fill_strings(Table::Docs, col::docs::CELL, &empty);
        let Some(doc) = h.and_then(|h| project.doc(h)) else {
            return;
        };
        let id = doc_id(h.expect("doc implies handle"));
        // The document groups follow the document's change log from the last fill; the
        // elab groups follow the check pass, compared with what was filled.
        let changes = self
            .filled
            .doc
            .filter(|d| d.0 == id)
            .and_then(|d| doc.changes_since(d.1));
        match changes {
            Some(changes) => patch_doc_groups(s, doc, id, changes, &mut self.filled),
            None => {
                let groups = [true, true, true, false, false, false, false];
                fill_groups(s, doc, id, cell, results, groups);
                (self.filled.wire_net, self.filled.label_net) = (None, None);
            }
        }
        self.filled.doc = Some((id, doc.cursor()));
        sync_cell_groups(s, id, cell, &mut self.filled);
        let rk = hash_of(results);
        if self.filled.results.replace(rk) != Some(rk) {
            let mut stale = [false; GROUPS];
            stale[4] = true;
            fill_groups(s, doc, id, cell, results, stale);
        }
        // Oracle (debug builds, small documents): the refilled groups equal a full refill.
        #[cfg(debug_assertions)]
        if doc.instances().id.len() + doc.wires().id.len() + doc.labels().id.len() < 20_000 {
            let mut full = self.snapshots.clone();
            fill_groups(&mut full, doc, id, cell, results, [true; GROUPS]);
            debug_assert_eq!(
                without_epochs(&format!("{:?}", self.snapshots)),
                without_epochs(&format!("{full:?}")),
                "incremental snapshot == full refill"
            );
        }
        debug_assert_eq!(self.snapshots.check(), Ok(()));
    }
}
