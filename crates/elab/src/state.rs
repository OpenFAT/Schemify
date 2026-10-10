//! The editor's derived state of one open document ([`CellState`]): its connectivity,
//! its expression graph evaluated in the default context, and its context-free
//! violations. The frame spine calls [`CellState::recompute`] with the ops `doc` just
//! applied; value edits re-evaluate only the affected nodes (D19, D39: 1k dependents
//! within 8 ms), everything else rebuilds what it touched.

use crate::cells::{display, row_of};
use crate::check::{CellCtx, Violation, Violations, check_overrides, check_values, waivers};
use crate::connect::{CellNets, connect};
use crate::graph::{Graph, OWN, Work, override_of};
use crate::iface::{Iface, own_ref};
use crate::names::{Name, Names};
use crate::values::{Funcs, NONE, Syntax, VAR};
use schemify_doc::{DocHandle, Document, ObjId, Op, Project, Sym};
use schemify_format::RecordKind;
use std::collections::HashMap;

/// What [`CellState::recompute`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recomputed {
    /// Connectivity was rebuilt.
    pub nets: bool,
    /// The graph was rebuilt and fully evaluated.
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
    unresolved: Vec<Option<String>>,
    net_findings: Vec<Violation>,
    waivers: Vec<(ObjId, Name)>,
    ws: Work,
}

impl CellState {
    /// Builds the state of document `h`. `None` for a stale handle.
    ///
    /// Cost: connectivity O(n log n) in objects, graph O(nodes + edges), one full
    /// evaluation and the checks.
    pub fn build<F: Funcs>(project: &Project, h: DocHandle, funcs: &mut F) -> Option<CellState> {
        let doc = project.doc(h)?;
        let mut st = CellState::default();
        let (cell, stem) = row_of(project, doc).map_or(("top".to_owned(), "top".to_owned()), |r| {
            display(project, r)
        });
        let own = Iface::from_doc(doc, &cell, &stem, NONE, true, &mut st.names);
        st.ifaces.push(own);
        let s = doc.strings();
        let mut by_row: HashMap<usize, u32> = HashMap::new();
        for i in 0..doc.instances().id.len() {
            let text = s.get(doc.instances().cell[i]);
            match project.resolve(h, text) {
                Ok(row) => {
                    let next = st.ifaces.len() as u32;
                    let at = *by_row.entry(row).or_insert(next);
                    if at == next {
                        let iface_doc = project.interface(row).expect("resolve checked the row");
                        let (cell, stem) = display(project, row);
                        let open = crate::cells::is_open(project, row);
                        let f = Iface::from_doc(
                            iface_doc,
                            &cell,
                            &stem,
                            row as u32,
                            open,
                            &mut st.names,
                        );
                        st.ifaces.push(f);
                    }
                    st.inst_iface.push(at);
                    st.unresolved.push(None);
                }
                Err(_) => {
                    st.inst_iface.push(NONE);
                    st.unresolved.push(Some(format!("cannot resolve `{text}`")));
                }
            }
        }
        st.rebuild_nets(doc);
        st.rebuild_graph(doc, funcs);
        st.check(doc, funcs);
        assert_eq!(st.inst_iface.len(), doc.instances().id.len());
        Some(st)
    }

    /// Brings the state up to date after `ops` were committed to document `h` (the
    /// frame spine's `elab::recompute`). Value edits (an existing override's or var's
    /// expression) re-evaluate only their dependents; geometry edits rebuild the
    /// connectivity; structural edits (instances, params, vars added or removed,
    /// overrides added or removed, changed dependencies) rebuild the graph. Ops of
    /// other documents are not seen: rebuild with [`CellState::build`] when an
    /// instantiated cell's interface changes.
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
        let mut done = Recomputed::default();
        let mut nets = false;
        let mut rebuild = false;
        let mut touched_insts: Vec<ObjId> = Vec::new();
        let mut touched_vars = false;
        let mut waived = false;
        for op in ops {
            let (kind, key, attr) = subject(op);
            match kind {
                RecordKind::Wire | RecordKind::Label | RecordKind::Port | RecordKind::Noconn => {
                    nets = true
                }
                RecordKind::Inst => match op {
                    Op::SetAttr { .. } | Op::RemoveAttr { .. } => match attr {
                        Some("at" | "name" | "inherit" | "ignore") => nets = true,
                        _ => touched_insts.extend(ObjId::find(key, doc.strings())),
                    },
                    _ => {
                        // Insert, delete, put, cell reference: rows and interfaces move.
                        *self = CellState::build(project, h, funcs).expect("live handle");
                        return Recomputed {
                            nets: true,
                            graph: true,
                            evaluated: 0,
                        };
                    }
                },
                RecordKind::Var => match op {
                    Op::SetField { index: 1, .. } => touched_vars = true,
                    _ => rebuild = true,
                },
                RecordKind::Waive => waived = true,
                RecordKind::Param | RecordKind::Pin | RecordKind::Impl | RecordKind::Header => {
                    *self = CellState::build(project, h, funcs).expect("live handle");
                    return Recomputed {
                        nets: true,
                        graph: true,
                        evaluated: 0,
                    };
                }
                _ => {}
            }
        }
        if nets {
            self.rebuild_nets(doc);
            done.nets = true;
        }
        let mut dirty: Vec<u32> = Vec::new();
        if !rebuild {
            rebuild = !self.diff_values(doc, &touched_insts, touched_vars, &mut dirty);
        }
        if rebuild {
            self.rebuild_graph(doc, funcs);
            done.graph = true;
        } else if !dirty.is_empty() {
            done.evaluated = self
                .graph
                .recompute(&dirty, &self.ifaces, funcs, &mut self.ws);
        }
        if done.nets || done.graph || waived || !touched_insts.is_empty() {
            self.check(doc, funcs);
        } else {
            self.check_values_only(doc, funcs);
        }
        assert!(!(done.graph && done.evaluated > 0));
        done
    }

    /// Recompiles the expressions whose text changed. Returns `false` when the change
    /// is structural (an override added or removed, a dependency changed).
    fn diff_values(
        &mut self,
        doc: &Document,
        insts: &[ObjId],
        vars: bool,
        dirty: &mut Vec<u32>,
    ) -> bool {
        let s = doc.strings();
        let g = &mut self.graph;
        if !g.cyclic.is_empty() {
            // Cycle nodes had their edges cut; any edit may break or move the cycle.
            return false;
        }
        let mut nodes: Vec<(u32, Sym, Syntax)> = Vec::new();
        for id in insts {
            let Some(loc) = doc.find_id(*id) else {
                return false;
            };
            let i = loc.row as usize;
            let Some(c) = self.ifaces.get(self.inst_iface[i] as usize) else {
                continue;
            };
            let first = g.inst_first[i];
            for k in 0..c.param_name.len() {
                let n = first + k as u32;
                let now = if c.param_computed[k] {
                    None
                } else {
                    override_of(doc, i, self.names.get(c.param_name[k]))
                };
                let was = (g.table[n as usize] == OWN).then_some(g.src[n as usize]);
                match (was, now) {
                    (None, None) => {}
                    (Some(a), Some(b)) if a == b => {}
                    (Some(_), Some(b)) => nodes.push((n, b, c.param_kind[k].syntax())),
                    _ => return false,
                }
            }
            // Override keys the cell lacks change nothing but W007 (re-checked below).
        }
        if vars {
            let blocks = doc.doc_blocks();
            let rows: Vec<usize> = (0..blocks.kind.len())
                .filter(|&r| blocks.kind[r] == RecordKind::Var)
                .collect();
            let own_vars = self.ifaces[0].var_name.len();
            if rows.len() != own_vars {
                return false;
            }
            for (v, r) in rows.into_iter().enumerate() {
                let n = g.params + v as u32;
                if g.src[n as usize] != blocks.fields[r][1] {
                    nodes.push((n, blocks.fields[r][1], Syntax::Expr));
                }
            }
        }
        let me = &self.ifaces[0];
        for (n, sym, syntax) in nodes {
            let names = &self.names;
            let lookup = |name: &str| own_ref(names, &me.param_name, &g.var_name, name);
            let prog = g.progs.push(s.get(sym), syntax, None, lookup);
            let new_deps: Vec<u32> = g
                .progs
                .refs(prog)
                .iter()
                .map(|&r| {
                    if r & VAR == 0 {
                        r
                    } else {
                        g.params + (r & !VAR)
                    }
                })
                .collect();
            if new_deps != g.node_deps(n) {
                return false;
            }
            g.table[n as usize] = OWN;
            g.prog[n as usize] = prog;
            g.src[n as usize] = sym;
            dirty.push(n);
        }
        true
    }

    fn rebuild_nets(&mut self, doc: &Document) {
        self.net_findings.clear();
        let mut globals = Vec::new();
        crate::connect::global_labels(doc, &mut self.names, &mut globals);
        globals.sort_unstable();
        globals.dedup();
        self.nets = connect(
            doc,
            &self.ifaces[0],
            &self.ifaces,
            &self.inst_iface,
            &globals,
            &mut self.names,
            &mut self.net_findings,
        );
    }

    fn rebuild_graph<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        // Own vars may have changed: refresh the own interface first.
        let (cell, stem) = (
            self.names.get(self.ifaces[0].cell).to_owned(),
            self.names.get(self.ifaces[0].stem).to_owned(),
        );
        self.ifaces[0] = Iface::from_doc(doc, &cell, &stem, NONE, true, &mut self.names);
        self.graph = Graph::build(doc, 0, &self.ifaces, &self.inst_iface, &[], &mut self.names);
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        self.graph
            .eval_all(None, &mut vals, &mut unv, &self.ifaces, funcs, &mut self.ws);
        self.graph.vals = vals;
        self.graph.unverified = unv;
    }

    /// Recomputes every finding.
    fn check<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        self.waivers = waivers(doc, &mut self.names);
        let mut rows = self.net_findings.clone();
        let cx = CellCtx {
            doc,
            ifaces: &self.ifaces,
            own: 0,
            inst_iface: &self.inst_iface,
            graph: &self.graph,
            waivers: &self.waivers,
            path: Name::NONE,
        };
        check_overrides(&cx, &self.unresolved, &mut self.names, &mut rows);
        check_values(
            &cx,
            &self.graph.vals,
            &self.names,
            funcs,
            &mut self.ws,
            &mut rows,
        );
        self.violations = Violations::from_rows(rows, &self.names);
    }

    /// Replaces only the findings that depend on values (`E008`, gating): after a value
    /// edit nothing else can change, and the other rows (often thousands of `W004`)
    /// keep their sorted place.
    fn check_values_only<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        let mut rows = Vec::new();
        let cx = CellCtx {
            doc,
            ifaces: &self.ifaces,
            own: 0,
            inst_iface: &self.inst_iface,
            graph: &self.graph,
            waivers: &self.waivers,
            path: Name::NONE,
        };
        check_values(
            &cx,
            &self.graph.vals,
            &self.names,
            funcs,
            &mut self.ws,
            &mut rows,
        );
        let fresh = Violations::from_rows(rows, &self.names);
        self.violations.replace_values(fresh, &self.names);
    }
}

/// The record an op is about: (kind, key, attribute key for attribute ops).
fn subject(op: &Op) -> (RecordKind, &str, Option<&str>) {
    match op {
        Op::Insert { rec, .. } => (
            rec.kind,
            rec.fields.first().map_or("", String::as_str),
            None,
        ),
        Op::Delete(t)
        | Op::Put { target: t, .. }
        | Op::SetField { target: t, .. }
        | Op::Reorder { target: t, .. } => (t.kind, t.key.as_str(), None),
        Op::SetAttr { target, key, .. } | Op::RemoveAttr { target, key, .. } => {
            (target.kind, target.key.as_str(), Some(key.as_str()))
        }
    }
}
