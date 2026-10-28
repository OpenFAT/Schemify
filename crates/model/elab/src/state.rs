//! The editor's derived state of one open document ([`CellState`]): the document and the
//! ops `doc` just applied in, its connectivity, its expression graph evaluated in the
//! default context, and its context-free violations out (D13, D19, D39: 1k dependents
//! within 8 ms). Value edits re-evaluate only the affected nodes (`revalue`), geometry
//! edits patch only the nets they touch (`live`) and post only the findings they change
//! (`findings`); everything else rebuilds what it touched.
//!
//! Six questions: in, one document and its ops; out, the tables below, owned here and
//! read by the editor. Instance rows ≤ 2^22 (`u32`); interface handles `u32`. Access: per
//! edit, the touched rows only. Lifetime: the open document. Serial: one document's edit.
//!
//! Contents:
//! - `pub enum NetChange`
//! - `pub struct Recomputed`
//! - `pub struct CellState`: `build`, `recompute`
//! - `pub(crate) fn subject(op: &Op) -> (RecordKind, &str, Option<&str>)`

mod findings;
mod revalue;

use crate::cells;
use crate::connect::{self, Cx};
use crate::eval::Work;
use crate::graph::Graph;
use crate::iface::{Held, Iface};
use crate::live::{Ctx, Delta, Live};
use crate::names::{Name, Names};
use crate::nets::{self, CellNets, Seed};
use crate::values::{Funcs, NONE};
use crate::violations::{self, Code, Violation, Violations};
use schemify_doc::{DocHandle, Document, ObjId, Op, Project};
use schemify_format::RecordKind;
use std::collections::{BTreeMap, HashMap};

/// What [`CellState::recompute`] did to the connectivity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetChange {
    /// No op touched it.
    #[default]
    Unchanged,
    /// Patched per touched net: untouched nets kept their handles, and the check pass
    /// posted only the findings that changed.
    Patched,
    /// Rebuilt: every net handle may have changed.
    Rebuilt,
}

/// What [`CellState::recompute`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recomputed {
    /// The connectivity's fate.
    pub nets: NetChange,
    /// The graph was rebuilt and fully evaluated (independent of `nets`: a var edit can
    /// rebuild the graph beside a net patch).
    pub graph: bool,
    /// Nodes re-evaluated incrementally (0 when the graph was rebuilt).
    pub evaluated: usize,
}

/// Derived data of one open document, in its default context (own params at their
/// defaults). Interfaces of instantiated cells are read when the state is built; an
/// edit to another document's interface needs [`CellState::build`] again.
#[derive(Debug, Default)]
pub struct CellState {
    /// Every name below.
    pub names: Names,
    /// Interfaces: index 0 is this document's own; the rest are instantiated cells.
    pub ifaces: Vec<Iface>,
    /// Interface of each instance row (`u32::MAX`: unresolved, `E001`).
    pub inst_iface: Vec<u32>,
    /// Connectivity.
    pub nets: CellNets,
    /// Expression graph with current values.
    pub graph: Graph,
    /// Context-free findings of this document, sorted.
    pub violations: Violations,
    /// What the last check pass changed in `violations`: (rows removed, rows added), each in
    /// key order. Applying it to pass `prev_pass`'s rows gives pass `pass`'s.
    pub changes: (Vec<Violation>, Vec<Violation>),
    /// Process-unique id of the check pass `violations` comes from.
    pub pass: u64,
    /// The pass `changes` starts from; 0 after [`CellState::build`] (no delta: take all).
    pub prev_pass: u64,
    /// The value delta of the last [`CellState::recompute`]: instance rows whose evaluated
    /// params (a value or its `unverified` flag) it changed, sorted. Empty after a build
    /// or a graph rebuild ([`Recomputed::graph`]), where every value may have changed.
    pub revalued: Vec<u32>,
    /// Why each unresolved instance row did not resolve (`None`: resolved).
    unresolved: Vec<Option<String>>,
    /// Connectivity findings by key, duplicate keys kept in key-run order (the table keeps
    /// the first row of each key): what a patch updates the table's connectivity rows from.
    /// Keyed by the raw handles, which are equal exactly when the keys are (one arena interns
    /// each text once), so a patch moves no other row.
    net_rows: BTreeMap<RowKey, Vec<Violation>>,
    live: Live,
    waivers: Vec<(ObjId, Name)>,
    ws: Work,
}

/// A finding's key as raw handles: (code, cell, object, path, param).
type RowKey = (Code, u32, u32, u32, u32);

/// What the ops of one [`CellState::recompute`] touched.
#[derive(Default)]
struct Touched<'o> {
    /// Rebuild the connectivity (`inherit`, `ignore`).
    nets: bool,
    /// Patch the connectivity (geometry moved).
    geom: bool,
    /// Rebuild the graph (a var added or removed, a Virtuoso-derived `cached`).
    graph: bool,
    /// Instances whose attributes changed (an override added, removed or changed).
    insts: Vec<ObjId>,
    /// A var's expression changed.
    vars: bool,
    /// D20: own vars and params whose `cached` stand-in changed (the app's write-back).
    cached_vars: Vec<&'o str>,
    cached_params: Vec<&'o str>,
    /// A waiver changed: the whole check pass reruns.
    waived: bool,
    /// Instances whose `name` changed: their element names and `E009` rows are patched.
    renamed: Vec<ObjId>,
}

impl CellState {
    /// Builds the state of document `h`. `None` for a stale handle.
    ///
    /// Cost: connectivity O(n log n) in objects, graph O(nodes + edges), one full
    /// evaluation and the checks.
    ///
    /// # Panics
    ///
    /// Panics only on a broken internal invariant: a cell row the project calls readable
    /// without an indexed interface.
    pub fn build<F: Funcs>(project: &Project, h: DocHandle, funcs: &mut F) -> Option<CellState> {
        let doc = project.doc(h)?;
        let mut st = CellState::default();
        let (cell, stem) = cells::row_of(project, doc)
            .map_or(("top".to_owned(), "top".to_owned()), |r| {
                cells::display(project, r)
            });
        let own = Iface::from_doc(doc, &cell, &stem, NONE, Held::Open, &mut st.names);
        st.ifaces.push(own);
        let s = doc.strings();
        // One interface per instantiated cell row, in first-use order.
        let mut by_row: HashMap<usize, u32> = HashMap::new();
        for i in 0..doc.instances().id.len() {
            let text = s.get(doc.instances().cell[i]);
            match cells::readable(project, project.resolve(h, text).ok(), text) {
                Ok(row) => {
                    let next = st.ifaces.len() as u32;
                    let at = *by_row.entry(row).or_insert(next);
                    if at == next {
                        let iface_doc = project.interface(row).expect("readable");
                        let (cell, stem) = cells::display(project, row);
                        let held = if project.open_handle(row).is_some() {
                            Held::Open
                        } else {
                            Held::Indexed
                        };
                        let row32 = row as u32;
                        let f =
                            Iface::from_doc(iface_doc, &cell, &stem, row32, held, &mut st.names);
                        st.ifaces.push(f);
                    }
                    st.inst_iface.push(at);
                    st.unresolved.push(None);
                }
                Err(why) => {
                    st.inst_iface.push(NONE);
                    st.unresolved.push(Some(why));
                }
            }
        }
        st.rebuild_nets(doc);
        st.rebuild_graph(doc, funcs);
        st.check(doc, funcs);
        assert_eq!(st.inst_iface.len(), doc.instances().id.len());
        assert_eq!(st.unresolved.len(), st.inst_iface.len());
        Some(st)
    }

    /// Brings the state up to date after `ops` were applied to document `h` (the frame
    /// spine's `elab::recompute`): the ops `Project::applied` lends for a commit, an undo
    /// (its inverse ops) or a redo. Value edits (an override set, changed or removed, a
    /// var's expression) re-evaluate only their dependents, report the instances whose
    /// values changed ([`CellState::revalued`]) and recheck only those and the edited ones;
    /// geometry edits patch the touched nets; an instance rename that keeps its element
    /// count patches its element names and duplicate-name rows; structural edits
    /// (instances, params, vars added or removed, a dependency on a later node, a bus
    /// rename) rebuild. Ops of other documents are not seen: rebuild with
    /// [`CellState::build`] when an instantiated cell's interface changes. The result
    /// always equals a fresh [`CellState::build`] (the incremental oracle).
    ///
    /// # Panics
    ///
    /// Panics if `h` is stale.
    pub fn recompute<F: Funcs>(
        &mut self,
        project: &Project,
        h: DocHandle,
        ops: &[Op],
        funcs: &mut F,
    ) -> Recomputed {
        let doc = project.doc(h).expect("recompute needs the edited document");
        // Row-indexed state is stale when a rejected transaction permuted the instances.
        if self.live.instances_moved(doc) {
            return self.rebuild(project, h, funcs);
        }
        let Some(mut t) = Touched::of(ops, doc) else {
            return self.rebuild(project, h, funcs); // rows or interfaces move
        };
        // A rejected transaction may have permuted the geometry tables' rows without ops:
        // the patch moves the row outputs.
        t.geom = t.geom || self.live.rows_moved(doc);

        // ---- connectivity: patch, or rebuild when the patch gives up
        let mut delta: Option<Delta> = None;
        if !t.nets && t.geom {
            let cx = Ctx::new(doc, &self.ifaces, &self.inst_iface);
            delta = self.live.update(ops, &cx, &mut self.nets, &mut self.names);
            t.nets = delta.is_none();
        }
        if !t.nets && !t.renamed.is_empty() {
            let cx = Ctx::new(doc, &self.ifaces, &self.inst_iface);
            match self
                .live
                .rename(&t.renamed, &cx, &mut self.nets, &mut self.names)
            {
                Some(renamed) => {
                    let d = delta.get_or_insert_default();
                    d.gone.extend(renamed.gone);
                    d.added.extend(renamed.added);
                }
                None => t.nets = true, // an element count changed
            }
        }
        if t.nets {
            delta = None;
            self.rebuild_nets(doc);
        }
        assert!(
            t.nets || delta.is_some() || !t.geom,
            "geometry is patched or rebuilt"
        );
        let mut done = Recomputed {
            nets: match (t.nets, delta.is_some()) {
                (true, _) => NetChange::Rebuilt,
                (false, true) => NetChange::Patched,
                (false, false) => NetChange::Unchanged,
            },
            ..Recomputed::default()
        };

        // ---- values: recompile the edited expressions, or rebuild the graph
        let mut dirty: Vec<u32> = Vec::new();
        done.graph = t.graph
            || !self.diff_values(doc, &t.insts, t.vars, &mut dirty)
            || !self.refresh_cached(doc, &t.cached_vars, &t.cached_params, &mut dirty);
        let mut changed: Vec<u32> = Vec::new();
        if done.graph {
            self.rebuild_graph(doc, funcs);
        } else if !dirty.is_empty() {
            done.evaluated =
                self.graph
                    .recompute(&dirty, &self.ifaces, funcs, &mut self.ws, &mut changed);
        }
        // The value delta: instance rows owning a node whose value changed.
        let owner = &self.graph.owner;
        let own = changed.iter().any(|&n| owner[n as usize] == NONE);
        self.revalued.clear();
        let revalued = changed.iter().map(|&n| owner[n as usize]);
        self.revalued.extend(revalued.filter(|&o| o != NONE));
        self.revalued.sort_unstable();
        self.revalued.dedup();

        // ---- findings: post the patch's and the touched instances', or rerun the pass
        // Rows whose findings may change: the revalued ones and those whose attributes an
        // op touched (an override added, removed or changed).
        let mut rows: Vec<u32> = t
            .insts
            .iter()
            .filter_map(|&id| doc.find_id(id))
            .map(|l| l.row)
            .collect();
        rows.extend_from_slice(&self.revalued);
        rows.sort_unstable();
        rows.dedup();
        let changes = if t.nets || done.graph || t.waived {
            None
        } else {
            let net = delta.map(|d| self.apply_net_delta(d));
            self.recheck(doc, &rows, own, funcs).map(|mut rechecked| {
                if let Some(net) = net {
                    rechecked.gone.extend(net.gone);
                    rechecked.added.extend(net.added);
                }
                let names = &self.names;
                rechecked
                    .gone
                    .sort_by(|a, b| violations::cmp_key(a, b, names));
                rechecked
                    .added
                    .sort_by(|a, b| violations::cmp_key(a, b, names));
                rechecked
            })
        };
        match changes {
            Some(c) => self.next_pass(c.gone, c.added),
            None => self.check(doc, funcs),
        }
        if done.graph {
            self.revalued.clear(); // every instance: consumers take a rebuild
        }
        if done.graph {
            assert_eq!(
                done.evaluated, 0,
                "a rebuilt graph evaluates fully, not incrementally"
            );
        }
        assert!(self.revalued.len() <= self.inst_iface.len());
        assert_eq!(
            self.graph.vals.len(),
            self.graph.len(),
            "every node evaluated"
        );
        done
    }
}

/// The record an op is about: (kind, key, attribute key for attribute ops).
pub(crate) fn subject(op: &Op) -> (RecordKind, &str, Option<&str>) {
    let (kind, key) = op.subject();
    let attr: Option<&str> = match op {
        Op::SetAttr { key, .. } | Op::RemoveAttr { key, .. } => Some(key.as_str()),
        _ => None,
    };
    (kind, key, attr)
}

// ---- private ----

impl<'o> Touched<'o> {
    /// Classifies `ops` (applied to `doc`); `None` when one moves rows or interfaces
    /// (instances inserted, deleted or re-pointed; params, pins, impls, the header).
    fn of(ops: &'o [Op], doc: &Document) -> Option<Touched<'o>> {
        let mut t = Touched::default();
        for op in ops {
            let (kind, key, attr) = subject(op);
            let attr_op = matches!(op, Op::SetAttr { .. } | Op::RemoveAttr { .. });
            match kind {
                RecordKind::Wire | RecordKind::Label | RecordKind::Port | RecordKind::Noconn => {
                    t.geom = true
                }
                RecordKind::Inst if !attr_op => return None,
                RecordKind::Inst => match attr {
                    Some("at") => t.geom = true,
                    Some("name") => t.renamed.extend(ObjId::find(key, doc.strings())),
                    Some("inherit" | "ignore") => t.nets = true,
                    // A Virtuoso-derived value and its digest (E039): rare, from an import;
                    // the values and their checks are rebuilt.
                    Some("cached") => t.graph = true,
                    _ => t.insts.extend(ObjId::find(key, doc.strings())),
                },
                RecordKind::Var => match op {
                    Op::SetField { index: 1, .. } => t.vars = true,
                    _ if attr_op && attr == Some("cached") => t.cached_vars.push(key),
                    _ => t.graph = true,
                },
                RecordKind::Waive => t.waived = true,
                RecordKind::Param if attr_op && attr == Some("cached") => t.cached_params.push(key),
                RecordKind::Param | RecordKind::Pin | RecordKind::Impl | RecordKind::Header => {
                    return None;
                }
                _ => {}
            }
        }
        assert!(t.insts.len() <= ops.len());
        assert!(t.renamed.len() <= ops.len());
        Some(t)
    }
}

impl CellState {
    /// Replaces the state by a fresh build (what every edit `recompute` cannot patch does).
    fn rebuild<F: Funcs>(&mut self, project: &Project, h: DocHandle, funcs: &mut F) -> Recomputed {
        *self = CellState::build(project, h, funcs).expect("recompute's handle is live");
        assert_eq!(self.prev_pass, 0, "a fresh build has no delta");
        Recomputed {
            nets: NetChange::Rebuilt,
            graph: true,
            evaluated: 0,
        }
    }

    /// Rebuilds the connectivity and reseeds the live patcher from it.
    fn rebuild_nets(&mut self, doc: &Document) {
        assert!(!self.ifaces.is_empty(), "the own interface is built first");
        let mut globals = Vec::new();
        nets::global_labels(doc, &mut self.names, &mut globals);
        globals.sort_unstable();
        globals.dedup();
        let (mut seed, mut found) = (Seed::default(), Vec::new());
        let connect_cx = Cx {
            doc,
            own: &self.ifaces[0],
            ifaces: &self.ifaces,
            inst_iface: &self.inst_iface,
            globals: &globals,
        };
        self.nets =
            connect::connect_seeded(&connect_cx, &mut self.names, &mut found, Some(&mut seed));
        let cx = Ctx::new(doc, &self.ifaces, &self.inst_iface);
        self.live = Live::seed(seed, found, globals, &self.nets, &cx, &mut self.names);
        assert_eq!(self.nets.elem_start.len(), self.inst_iface.len() + 1);
    }

    /// Rebuilds the own interface (own vars may have changed) and the graph, and evaluates
    /// every node.
    fn rebuild_graph<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        let (cell, stem) = (
            self.names.get(self.ifaces[0].cell).to_owned(),
            self.names.get(self.ifaces[0].stem).to_owned(),
        );
        self.ifaces[0] = Iface::from_doc(doc, &cell, &stem, NONE, Held::Open, &mut self.names);
        self.graph = Graph::build(doc, 0, &self.ifaces, &self.inst_iface, &[], &self.names);
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        self.graph
            .eval_all(None, &mut vals, &mut unv, &self.ifaces, funcs, &mut self.ws);
        self.graph.vals = vals;
        self.graph.unverified = unv;
        assert_eq!(self.graph.vals.len(), self.graph.len());
        assert_eq!(self.graph.unverified.len(), self.graph.len());
    }
}
