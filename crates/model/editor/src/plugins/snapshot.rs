//! Snapshot tables for plugins (plugin-abi §4): the documents, nets, cell values and
//! violations, filled in groups and patched per row when only a few rows changed.
//!
//! Contents:
//! - `pub(super) struct Filled`
//! - `pub(super) fn fill_nets`
//! - `pub(super) fn fill_violations`
//! - `Host::snapshot`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use schemify_doc::{Cursor, DocHandle, Project};
use schemify_elab::{CellState, Name};
use schemify_plugin::Snapshots;
use schemify_plugin::abi::{Table, col};

use super::patch::{patch_doc_groups, sync_cell_groups};
use super::{Host, ResultRows, doc_id};

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
    /// The Nets columns as filled.
    pub(super) nets: NetShadow,
    /// What the Results table was filled from.
    pub(super) results: Option<u64>,
}

/// The Nets snapshot columns as last filled, one row per component.
#[derive(Debug, Default)]
pub(super) struct NetShadow {
    pub(super) name: Vec<Name>,
    pub(super) width: Vec<u16>,
    pub(super) global: Vec<bool>,
    pub(super) pins: Vec<u32>,
}

/// Snapshot groups [`Host::snapshot`] refills independently: Instances; Wires and Labels
/// without their net column; Nets with Violations; Results; the Wires and the Labels net
/// columns (a check pass renumbers nets without moving a wire).
const GROUPS: usize = 7;

/// Writes nets `rows` of `c` over Nets rows `at`.
pub(super) fn fill_nets(s: &mut Snapshots, c: &CellState, at: Range<usize>, rows: Range<usize>) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
    assert!(
        at.end <= s.rows(Table::Nets) as usize,
        "rows {at:?} of the Nets table"
    );
    let n = &c.nets;
    assert!(rows.end <= n.net_name.len(), "nets {rows:?} of the cell");
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
    at: Range<usize>,
    rows: Range<usize>,
) {
    if at.is_empty() && rows.is_empty() {
        return;
    }
    assert!(
        at.end <= s.rows(Table::Violations) as usize,
        "rows {at:?} of Violations"
    );
    assert!(
        rows.end <= c.violations.rows.len(),
        "violations {rows:?} of the cell"
    );
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
    ///
    /// # Panics
    ///
    /// `h` names a document past `doc::DOCS_MAX` ([`doc_id`]).
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
        assert_eq!(
            s.rows(Table::Docs) as usize,
            handles.len(),
            "one Docs row per document"
        );
        let Some((h, doc)) = h.and_then(|h| Some((h, project.doc(h)?))) else {
            return;
        };
        let id = doc_id(h);
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
        assert_eq!(
            s.rows(Table::Wires) as usize,
            doc.wires().id.len(),
            "Wires in step"
        );
        assert_eq!(
            s.rows(Table::Labels) as usize,
            doc.labels().id.len(),
            "Labels in step"
        );
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

// ---- private ----

/// Fills the snapshot groups flagged `stale` ([`GROUPS`]) from document `doc` (snapshot id
/// `id`), its state `cell` and the run's `results`; see [`Host::snapshot`].
fn fill_groups(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    cell: Option<&CellState>,
    results: &ResultRows,
    stale: [bool; GROUPS],
) {
    assert!(
        stale.iter().any(|&g| g),
        "a fill refreshes at least one group"
    );
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
        assert_eq!(s.rows(Table::Results) as usize, results.len());
    }
    // The caller completes the tables (a document group refilled without its net column
    // waits for `sync_cell_groups`) and checks them.
}

/// A snapshot's debug text without its table epochs (they count fills, not content).
#[cfg(debug_assertions)]
fn without_epochs(text: &str) -> String {
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
fn hash_of(v: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}
