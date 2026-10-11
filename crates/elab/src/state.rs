//! The editor's derived state of one open document ([`CellState`]): its connectivity,
//! its expression graph evaluated in the default context, and its context-free
//! violations. The frame spine calls [`CellState::recompute`] with the ops `doc` just
//! applied; value edits re-evaluate only the affected nodes (D19, D39: 1k dependents
//! within 8 ms), geometry edits patch only the nets they touch (`live`) and post only the
//! findings they change; everything else rebuilds what it touched.

use crate::cells::{display, row_of};
use crate::check::{
    CellCtx, Violation, Violations, check_overrides, check_values, cmp_key, waivers,
};
use crate::connect::{CellNets, Seed, connect_seeded};
use crate::graph::{Graph, OWN, Work, override_of};
use crate::iface::{Iface, own_ref};
use crate::live::{Ctx, Delta, Live};
use crate::names::{Name, Names};
use crate::values::{Funcs, NONE, Syntax, VAR};
use schemify_doc::{DocHandle, Document, ObjId, Op, Project, Sym};
use schemify_format::RecordKind;
use std::collections::HashMap;

/// What [`CellState::recompute`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recomputed {
    /// Connectivity changed (rebuilt, or patched).
    pub nets: bool,
    /// Connectivity was patched per touched net instead of rebuilt: untouched nets kept
    /// their handles, and the check pass posted only the findings that changed.
    pub patched: bool,
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
    /// What the last check pass changed in `violations`: (rows removed, rows added), each in
    /// key order. Applying it to pass `prev_pass`'s rows gives pass `pass`'s.
    pub changes: (Vec<Violation>, Vec<Violation>),
    /// Process-unique id of the check pass `violations` comes from.
    pub pass: u64,
    /// The pass `changes` starts from; 0 after [`CellState::build`] (no delta: take all).
    pub prev_pass: u64,
    unresolved: Vec<Option<String>>,
    /// Connectivity findings sorted by key, duplicate keys kept (the table keeps one row
    /// per key): what a patch updates the table's connectivity rows from.
    net_rows: Vec<Violation>,
    live: Live,
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
        if self.live.instances_moved(doc) {
            // Row-indexed state is stale: a rejected transaction permuted the instances.
            *self = CellState::build(project, h, funcs).expect("live handle");
            return Recomputed {
                nets: true,
                patched: false,
                graph: true,
                evaluated: 0,
            };
        }
        let mut done = Recomputed::default();
        // `nets`: rebuild the connectivity; `geom`: patch it (`live`).
        let (mut nets, mut geom) = (false, false);
        let mut rebuild = false;
        let mut touched_insts: Vec<ObjId> = Vec::new();
        let mut touched_vars = false;
        // D20: own vars and params whose `cached` stand-in changed (the app's write-back).
        let mut cached_vars: Vec<&str> = Vec::new();
        let mut cached_params: Vec<&str> = Vec::new();
        let mut waived = false;
        for op in ops {
            let (kind, key, attr) = subject(op);
            match kind {
                RecordKind::Wire | RecordKind::Label | RecordKind::Port | RecordKind::Noconn => {
                    geom = true
                }
                RecordKind::Inst => match op {
                    Op::SetAttr { .. } | Op::RemoveAttr { .. } => match attr {
                        Some("at") => geom = true,
                        Some("name" | "inherit" | "ignore") => nets = true,
                        _ => touched_insts.extend(ObjId::find(key, doc.strings())),
                    },
                    _ => {
                        // Insert, delete, put, cell reference: rows and interfaces move.
                        *self = CellState::build(project, h, funcs).expect("live handle");
                        return Recomputed {
                            nets: true,
                            patched: false,
                            graph: true,
                            evaluated: 0,
                        };
                    }
                },
                RecordKind::Var => match op {
                    Op::SetField { index: 1, .. } => touched_vars = true,
                    Op::SetAttr { .. } | Op::RemoveAttr { .. } if attr == Some("cached") => {
                        cached_vars.push(key);
                    }
                    _ => rebuild = true,
                },
                RecordKind::Waive => waived = true,
                RecordKind::Param
                    if matches!(op, Op::SetAttr { .. } | Op::RemoveAttr { .. })
                        && attr == Some("cached") =>
                {
                    cached_params.push(key);
                }
                RecordKind::Param | RecordKind::Pin | RecordKind::Impl | RecordKind::Header => {
                    *self = CellState::build(project, h, funcs).expect("live handle");
                    return Recomputed {
                        nets: true,
                        patched: false,
                        graph: true,
                        evaluated: 0,
                    };
                }
                _ => {}
            }
        }
        let mut delta: Option<Delta> = None;
        if !nets && geom {
            let cx = Ctx {
                doc,
                own: &self.ifaces[0],
                ifaces: &self.ifaces,
                inst_iface: &self.inst_iface,
            };
            delta = self.live.update(ops, &cx, &mut self.nets, &mut self.names);
            nets = delta.is_none(); // the patch gave up: rebuild
            done.patched = delta.is_some();
        }
        if nets {
            self.rebuild_nets(doc);
        }
        done.nets = nets || geom;
        let mut dirty: Vec<u32> = Vec::new();
        if !rebuild {
            rebuild = !self.diff_values(doc, &touched_insts, touched_vars, &mut dirty);
        }
        if !rebuild {
            rebuild = !self.refresh_cached(doc, &cached_vars, &cached_params, &mut dirty);
        }
        if rebuild {
            self.rebuild_graph(doc, funcs);
            done.graph = true;
        } else if !dirty.is_empty() {
            done.evaluated = self
                .graph
                .recompute(&dirty, &self.ifaces, funcs, &mut self.ws);
        }
        // A patch posts its findings unless values changed too (then one full pass).
        let values = done.graph || waived || !touched_insts.is_empty() || !dirty.is_empty();
        match delta {
            Some(d) if !values => self.apply_net_delta(d),
            _ if done.nets || values => self.check(doc, funcs),
            _ => self.check_values_only(doc, funcs),
        }
        assert!(!(done.graph && done.evaluated > 0));
        assert!(!(done.patched && nets), "patched or rebuilt, not both");
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

    /// Points the programs of own vars `vars` and own params `params` at their current
    /// `cached` attribute (D20), marking dirty the ones whose value rests on it. A cache write
    /// never changes a value that a loaded plugin computed, so it costs no rebuild. Returns
    /// `false` when a name is not an own var / param (the caller rebuilds).
    fn refresh_cached(
        &mut self,
        doc: &Document,
        vars: &[&str],
        params: &[&str],
        dirty: &mut Vec<u32>,
    ) -> bool {
        let s = doc.strings();
        let (blocks, prows) = (doc.doc_blocks(), doc.params());
        let g = &mut self.graph;
        let names = vars.iter().map(|&v| (false, v));
        for (is_param, name) in names.chain(params.iter().map(|&p| (true, p))) {
            let Some(n) = self.names.find(name) else {
                return false;
            };
            let (table, kind, slot) = if is_param {
                let own = &self.ifaces[0].param_name;
                (prows, RecordKind::Param, own.iter().position(|&x| x == n))
            } else {
                let v = g.var_name.iter().position(|&x| x == n);
                (blocks, RecordKind::Var, v.map(|v| g.params as usize + v))
            };
            let Some(node) = slot.map(|k| k as u32) else {
                return false;
            };
            let Some(r) = (0..table.kind.len())
                .find(|&r| table.kind[r] == kind && s.get(table.fields[r][0]) == name)
            else {
                return false;
            };
            let cached = crate::iface::cached(table, r, s, doc);
            let prog = g.prog[node as usize] as usize;
            let progs = if g.table[node as usize] == OWN {
                &mut g.progs
            } else {
                assert_eq!(
                    g.table[node as usize], g.own,
                    "own var or param in the own interface"
                );
                &mut self.ifaces[g.own as usize].progs
            };
            progs.cached[prog] = cached;
            if g.unverified[node as usize] || g.vals[node as usize].is_err() {
                dirty.push(node);
            }
        }
        debug_assert!(dirty.iter().all(|&d| (d as usize) < g.len()));
        true
    }

    fn rebuild_nets(&mut self, doc: &Document) {
        let mut globals = Vec::new();
        crate::connect::global_labels(doc, &mut self.names, &mut globals);
        globals.sort_unstable();
        globals.dedup();
        let (mut seed, mut found) = (Seed::default(), Vec::new());
        self.nets = connect_seeded(
            doc,
            &self.ifaces[0],
            &self.ifaces,
            &self.inst_iface,
            &globals,
            &mut self.names,
            &mut found,
            Some(&mut seed),
        );
        let cx = Ctx {
            doc,
            own: &self.ifaces[0],
            ifaces: &self.ifaces,
            inst_iface: &self.inst_iface,
        };
        self.live = Live::seed(seed, found, globals, &self.nets, &cx, &mut self.names);
    }

    /// Posts a connectivity patch's findings: `gone` leave the table and `new` enter it
    /// (rows on both sides cancel), as the delta of a new pass. The table keeps one row per
    /// key, so a key leaves only with its last connectivity row.
    ///
    /// Cost: O((gone + new) · log rows) compares plus a memmove per changed row.
    fn apply_net_delta(&mut self, (mut gone, mut new): Delta) {
        let names = &self.names;
        let order = |a: &Violation, b: &Violation| {
            cmp_key(a, b, names).then_with(|| a.detail.cmp(&b.detail))
        };
        gone.sort_by(order);
        new.sort_by(order);
        let (mut g, mut n) = (Vec::new(), Vec::new());
        let (mut gi, mut ni) = (gone.into_iter().peekable(), new.into_iter().peekable());
        loop {
            match (gi.peek(), ni.peek()) {
                (Some(a), Some(b)) => match order(a, b) {
                    std::cmp::Ordering::Equal => {
                        gi.next();
                        ni.next();
                    }
                    std::cmp::Ordering::Less => g.extend(gi.next()),
                    std::cmp::Ordering::Greater => n.extend(ni.next()),
                },
                (Some(_), None) => g.extend(gi.next()),
                (None, Some(_)) => n.extend(ni.next()),
                (None, None) => break,
            }
        }
        let (rows, net) = (&mut self.violations.rows, &mut self.net_rows);
        let mut changes: (Vec<Violation>, Vec<Violation>) = (Vec::new(), Vec::new());
        for r in g {
            let lo = net.partition_point(|x| cmp_key(x, &r, names).is_lt());
            let at = (lo..net.len())
                .take_while(|&k| cmp_key(&net[k], &r, names).is_eq())
                .find(|&k| net[k] == r)
                .expect("a gone finding is a connectivity row");
            net.remove(at);
            let t = rows
                .binary_search_by(|x| cmp_key(x, &r, names))
                .expect("its key is in the table");
            let kept = net.get(lo).filter(|x| cmp_key(x, &r, names).is_eq());
            match kept {
                None => changes.0.push(rows.remove(t)),
                Some(k) if rows[t] == r => {
                    changes.0.push(std::mem::replace(&mut rows[t], k.clone()));
                    changes.1.push(k.clone());
                }
                Some(_) => {}
            }
        }
        for r in n {
            let at = net.partition_point(|x| cmp_key(x, &r, names).is_le());
            net.insert(at, r.clone());
            if let Err(t) = rows.binary_search_by(|x| cmp_key(x, &r, names)) {
                rows.insert(t, r.clone());
                changes.1.push(r);
            }
        }
        changes.0.sort_by(|a, b| cmp_key(a, b, names));
        changes.1.sort_by(|a, b| cmp_key(a, b, names));
        debug_assert!(
            rows.windows(2)
                .all(|w| cmp_key(&w[0], &w[1], names).is_lt())
        );
        self.next_pass(changes);
    }

    fn rebuild_graph<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        // Own vars may have changed: refresh the own interface first.
        let (cell, stem) = (
            self.names.get(self.ifaces[0].cell).to_owned(),
            self.names.get(self.ifaces[0].stem).to_owned(),
        );
        self.ifaces[0] = Iface::from_doc(doc, &cell, &stem, NONE, true, &mut self.names);
        self.graph = Graph::build(doc, 0, &self.ifaces, &self.inst_iface, &[], &self.names);
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        self.graph
            .eval_all(None, &mut vals, &mut unv, &self.ifaces, funcs, &mut self.ws);
        self.graph.vals = vals;
        self.graph.unverified = unv;
    }

    /// Recomputes every finding.
    fn check<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        self.waivers = waivers(doc, &mut self.names);
        let mut net = self.live.rows(&self.nets, &self.ifaces[0], &self.names);
        net.sort_by(|a, b| cmp_key(a, b, &self.names));
        self.net_rows = net;
        let mut rows = self.net_rows.clone();
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
        let fresh = Violations::from_rows(rows, &self.names);
        // The first pass has no predecessor: consumers take every row (`prev_pass == 0`).
        let changes = if self.pass == 0 {
            (Vec::new(), Vec::new())
        } else {
            let (gone, new) = fresh.diff(&self.violations, &self.names);
            (
                gone.into_iter().cloned().collect(),
                new.into_iter().cloned().collect(),
            )
        };
        self.violations = fresh;
        self.next_pass(changes);
    }

    /// Starts a new check pass whose delta from the current one is `changes`.
    fn next_pass(&mut self, changes: (Vec<Violation>, Vec<Violation>)) {
        static PASSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.prev_pass = self.pass;
        self.pass = PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.changes = changes;
        assert!(self.pass > self.prev_pass);
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
        let old = self.violations.replace_values(fresh, &self.names);
        // The delta: old value rows against the fresh ones (both sorted, value codes only).
        let fresh = Violations {
            rows: self
                .violations
                .rows
                .iter()
                .filter(|v| crate::check::value_code(v.code))
                .cloned()
                .collect(),
        };
        let (gone, new) = fresh.diff(&old, &self.names);
        let changes = (
            gone.into_iter().cloned().collect(),
            new.into_iter().cloned().collect(),
        );
        self.next_pass(changes);
    }
}

/// The record an op is about: (kind, key, attribute key for attribute ops).
pub(crate) fn subject(op: &Op) -> (RecordKind, &str, Option<&str>) {
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
