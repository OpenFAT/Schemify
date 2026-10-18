//! The expression graph of one cell (D19, expr spec §4): one node per value the cell
//! defines or instantiates, edges "depends on", evaluated in topological order, and
//! re-evaluated incrementally over the affected set after an edit.
//!
//! Node layout (by index):
//! - `[0, P)`: the cell's own params (default or computed expression), own scope;
//! - `[P, P + V)`: the cell's own vars, own scope (a bench's corner `set` and request
//!   overrides replace a var's expression, or add a var);
//! - per instance with a resolved interface, one node per param of the instantiated
//!   cell: the override (own scope) when present, else the cell's default or computed
//!   expression in the child scope;
//! - child vars an instance's child-scope expressions reference, created on demand.
//!
//! Own scope = own params, then own vars; child scope = that instance's param nodes,
//! then its child var nodes (expr spec §4, netlist spec §6).
//!
//! Six questions: in, a document + interfaces; out, `vals`/`unverified` columns. Nodes
//! ≤ instances × 2^8 params (2^30 bound, `u32`). Eval walks `order` reading each
//! node's deps (a `[start, end)` range per node: built as CSR, an edit that changes a
//! node's dependencies writes them in place or appends them, [`Graph::set_deps`]).
//! Lifetime: until a structural edit (the editor) or one elaboration. Parallel: one graph
//! per cell, independent.

use crate::iface::{GRID, Iface, MAX, MIN, STEP, VALID};
use crate::names::{Name, Names};
use crate::values::{Code as PCode, Funcs, NONE, Progs, Ref, Syntax, VAR, Val};
use schemify_doc::{Document, Instances, Sym};
use schemify_expr::{Deps, Error as XError, Scratch, Value, reverse, topo_order, topo_order_all};
use std::collections::HashMap;

/// Program table of a node: the graph's own table (overrides, substituted vars), or the
/// interface at this index.
pub(crate) const OWN: u32 = u32::MAX - 1;

/// One cell's dependency graph and its current values.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    /// Instance row owning the node, or `u32::MAX` for own params/vars.
    pub owner: Vec<u32>,
    /// Param index (own or child), or `VAR | var index` for var nodes.
    pub(crate) slot: Vec<u32>,
    pub(crate) table: Vec<u32>,
    pub(crate) prog: Vec<u32>,
    /// Doc symbol of the node's expression text in the cell's own document (overrides,
    /// own params and vars), for change detection; [`Sym::NONE`] otherwise.
    pub(crate) src: Vec<Sym>,
    /// Each node's dependencies are `deps[dep_start[n]..dep_end[n]]`.
    pub(crate) dep_start: Vec<u32>,
    pub(crate) dep_end: Vec<u32>,
    pub(crate) deps: Vec<u32>,
    /// Each node's dependents are `rdeps[rdep_start[n]..rdep_end[n]]`: a superset after
    /// [`Graph::set_deps`] dropped an edge (a stale dependent is only re-evaluated).
    pub(crate) rdep_start: Vec<u32>,
    pub(crate) rdep_end: Vec<u32>,
    pub(crate) rdeps: Vec<u32>,
    /// Edges at the last build: [`Graph::set_deps`] gives up (the caller rebuilds, which
    /// compacts) once appended lists pass twice this.
    pub(crate) edges: usize,
    pub(crate) rank: Vec<u32>,
    pub(crate) order: Vec<u32>,
    /// Nodes on a dependency cycle (`X005`), with the error.
    pub(crate) cyclic: Vec<(u32, XError)>,
    /// Current value per node (default context in the editor).
    pub vals: Vec<Val>,
    /// D20: the value rests on a `cached` stand-in for a missing plugin function.
    pub unverified: Vec<bool>,
    /// Own interface index.
    pub(crate) own: u32,
    /// Own params.
    pub params: u32,
    /// Own vars (including vars added by substitution).
    pub vars: u32,
    /// Own var names (interface vars, then added ones).
    pub var_name: Vec<Name>,
    /// First node of each instance's block, or `u32::MAX` (unresolved cell).
    pub inst_first: Vec<u32>,
    /// Interface of each instance row (`u32::MAX`: unresolved).
    pub(crate) inst_iface: Vec<u32>,
    /// `(instance row, child var) → node`, sorted.
    pub(crate) child_vars: Vec<(u32, u32, u32)>,
    pub(crate) progs: Progs,
}

/// Returns the override text symbol of `key` on instance row `i`: the last attribute
/// line with that key (a duplicate is `E024` at load; the later line wins), else the value
/// of its `cached <key> <value> <digest>` line (a value Virtuoso derived, D20 as amended
/// 2026-10-15).
pub(crate) fn override_of(doc: &Document, i: usize, key: &str) -> Option<Sym> {
    let s = doc.strings();
    let mut found = None;
    let mut cached = None;
    for (k, vals) in doc.pool().attrs(doc.instances().attrs[i]) {
        let k = s.get(k);
        if Instances::is_override(k) && k == key {
            found = vals.first().copied();
        } else if k == "cached" && vals.first().is_some_and(|p| s.get(*p) == key) {
            cached = vals.get(1).copied();
        }
    }
    found.or(cached)
}

/// The `(value, digest)` of instance row `i`'s `cached <key> <value> <digest>` line, and
/// whether `key` has an override of its own.
pub(crate) fn cached_of<'d>(
    doc: &'d Document,
    i: usize,
    key: &str,
) -> (Option<(&'d str, &'d str)>, bool) {
    let s = doc.strings();
    let (mut cached, mut own) = (None, false);
    for (k, vals) in doc.pool().attrs(doc.instances().attrs[i]) {
        let k = s.get(k);
        if Instances::is_override(k) && k == key {
            own = true;
        } else if k == "cached" && vals.len() == 3 && s.get(vals[0]) == key {
            cached = Some((s.get(vals[1]), s.get(vals[2])));
        }
    }
    (cached, own)
}

impl Graph {
    /// Builds the graph of `doc`. `own` indexes `ifaces` (the cell's own interface);
    /// `inst_iface[i]` is instance row `i`'s interface or `u32::MAX`. `subst` replaces
    /// own var expressions by name (later entries win), adding vars that do not exist.
    ///
    /// Cost: O(nodes + edges) plus one compile per distinct override text.
    pub(crate) fn build(
        doc: &Document,
        own: u32,
        ifaces: &[Iface],
        inst_iface: &[u32],
        subst: &[(Name, &str)],
        names: &Names,
    ) -> Graph {
        let s = doc.strings();
        let ni = doc.instances().id.len();
        assert_eq!(inst_iface.len(), ni);
        let me = &ifaces[own as usize];
        let mut g = Graph {
            own,
            params: me.param_name.len() as u32,
            var_name: me.var_name.clone(),
            inst_iface: inst_iface.to_vec(),
            ..Graph::default()
        };
        // Substituted / added vars (later wins).
        let mut var_subst: Vec<Option<&str>> = vec![None; me.var_name.len()];
        for &(n, text) in subst {
            match g.var_name.iter().position(|v| *v == n) {
                Some(v) => var_subst[v] = Some(text),
                None => {
                    g.var_name.push(n);
                    var_subst.push(Some(text));
                }
            }
        }
        g.vars = g.var_name.len() as u32;
        let p = g.params;
        // Own params and vars.
        let own_src = own_sources(doc);
        for k in 0..p {
            let src = own_src.0.get(k as usize).copied().unwrap_or(Sym::NONE);
            g.push(NONE, k, own, me.param_prog[k as usize], src);
        }
        for v in 0..g.vars {
            let src = own_src.1.get(v as usize).copied().unwrap_or(Sym::NONE);
            match var_subst[v as usize] {
                Some(text) => {
                    let prog = {
                        let lookup =
                            |n: &str| crate::iface::own_ref(names, &me.param_name, &g.var_name, n);
                        g.progs.push(text, Syntax::Expr, None, lookup)
                    };
                    g.push(NONE, VAR | v, OWN, prog, Sym::NONE);
                }
                None => g.push(NONE, VAR | v, own, me.var_prog[v as usize], src),
            }
        }
        // Instance blocks.
        let mut dedupe: HashMap<(Sym, bool), u32> = HashMap::new();
        for i in 0..ni {
            let Some(c) = ifaces.get(inst_iface[i] as usize) else {
                g.inst_first.push(NONE);
                continue;
            };
            g.inst_first.push(g.owner.len() as u32);
            for k in 0..c.param_name.len() {
                let ovr = if c.param_computed[k] {
                    None // E030: the override is reported and ignored
                } else {
                    override_of(doc, i, names.get(c.param_name[k]))
                };
                match ovr {
                    Some(sym) => {
                        let syntax = c.param_kind[k].syntax();
                        let key = (sym, syntax == Syntax::Template);
                        let prog = match dedupe.get(&key) {
                            Some(&prog) => prog,
                            None => {
                                let lookup = |n: &str| {
                                    crate::iface::own_ref(names, &me.param_name, &g.var_name, n)
                                };
                                let prog = g.progs.push(s.get(sym), syntax, None, lookup);
                                dedupe.insert(key, prog);
                                prog
                            }
                        };
                        g.push(i as u32, k as u32, OWN, prog, sym);
                    }
                    None => g.push(
                        i as u32,
                        k as u32,
                        inst_iface[i],
                        c.param_prog[k],
                        Sym::NONE,
                    ),
                }
            }
        }
        // Dependencies (child var nodes are appended while resolving).
        g.dep_start.push(0);
        g.dep_start.reserve(g.owner.len());
        let mut n = 0;
        let mut refs: Vec<Ref> = Vec::new(); // reused: one allocation, not one per node
        while n < g.owner.len() {
            let (owner, table, prog) = (g.owner[n], g.table[n], g.prog[n]);
            let child_scope = owner != NONE && table != OWN;
            let progs = if table == OWN {
                &g.progs
            } else {
                &ifaces[table as usize].progs
            };
            refs.clear();
            refs.extend_from_slice(progs.refs(prog));
            for &r in &refs {
                let d = if child_scope {
                    g.child_ref(owner, table, r, ifaces)
                } else {
                    own_node(p, r)
                };
                g.deps.push(d);
            }
            if child_scope && g.slot[n] & VAR == 0 {
                // Bounds may read child vars: make sure their nodes exist for gating.
                let c = &ifaces[table as usize];
                for b in c.param_bound[g.slot[n] as usize] {
                    if b != NONE {
                        for &r in c.progs.refs(b) {
                            if r & VAR != 0 {
                                g.child_ref(owner, table, r, ifaces);
                            }
                        }
                    }
                }
            }
            g.dep_start.push(g.deps.len() as u32);
            n += 1;
        }
        g.finish(ifaces, names);
        g
    }

    fn push(&mut self, owner: u32, slot: u32, table: u32, prog: u32, src: Sym) {
        self.owner.push(owner);
        self.slot.push(slot);
        self.table.push(table);
        self.prog.push(prog);
        self.src.push(src);
        assert!(self.owner.len() < NONE as usize - 1, "graph node capacity");
    }

    /// Node for a child-scope reference of instance `owner` (interface `table`).
    fn child_ref(&mut self, owner: u32, table: u32, r: Ref, ifaces: &[Iface]) -> u32 {
        if r & VAR == 0 {
            return self.inst_first[owner as usize] + r;
        }
        let v = r & !VAR;
        let key = (owner, v);
        match self.child_vars.binary_search_by_key(&key, |e| (e.0, e.1)) {
            Ok(i) => self.child_vars[i].2,
            Err(i) => {
                let node = self.owner.len() as u32;
                let prog = ifaces[table as usize].var_prog[v as usize];
                self.push(owner, VAR | v, table, prog, Sym::NONE);
                self.child_vars.insert(i, (owner, v, node));
                node
            }
        }
    }

    /// Builds reverse edges and the topological order. Every node on a cycle gets `X005`
    /// (expr spec §4: all cycles in one pass) and loses its edges, which leaves the order
    /// valid: an edge inside a cyclic component starts at a cycle node.
    fn finish(&mut self, ifaces: &[Iface], names: &Names) {
        let n = self.owner.len();
        assert_eq!(self.dep_start.len(), n + 1);
        self.dep_end = self.dep_start[1..].to_vec();
        let deps = Deps {
            offsets: &self.dep_start,
            targets: &self.deps,
        };
        // The acyclic case (nearly every cell) takes the cheaper order.
        if topo_order(deps, &mut self.order).is_err() {
            let mut cycles = Vec::new();
            topo_order_all(deps, &mut self.order, &mut cycles);
            for c in &cycles {
                let err = c.error(|n| self.node_name(n, ifaces, names));
                self.cyclic
                    .extend(c.nodes.iter().map(|&node| (node, err.clone())));
            }
            // A node on several cycles reports the first.
            self.cyclic.sort_by_key(|e| e.0);
            self.cyclic.dedup_by_key(|e| e.0);
            assert!(!self.cyclic.is_empty(), "a failed order has a cycle");
            self.cut();
        }
        let deps = Deps {
            offsets: &self.dep_start,
            targets: &self.deps,
        };
        reverse(deps, &mut self.rdep_start, &mut self.rdeps);
        self.rdep_end = self.rdep_start[1..].to_vec();
        self.edges = self.deps.len();
        self.rank = vec![0; n];
        for (r, &node) in self.order.iter().enumerate() {
            self.rank[node as usize] = r as u32;
        }
        self.vals = vec![Val::Upstream; n];
        self.unverified = vec![false; n];
        assert_eq!(self.order.len(), n);
    }

    /// Names node `n` for messages: `param`, `var`, or `<inst row>.<param>`.
    fn node_name<'a>(&self, n: u32, ifaces: &'a [Iface], names: &'a Names) -> &'a str {
        let (owner, slot) = (self.owner[n as usize], self.slot[n as usize]);
        let f = if owner == NONE {
            &ifaces[self.own as usize]
        } else {
            &ifaces[self.inst_iface[owner as usize] as usize]
        };
        match (slot & VAR == 0, owner == NONE) {
            (true, _) => names.get(f.param_name[slot as usize]),
            (false, true) => names.get(self.var_name[(slot & !VAR) as usize]),
            (false, false) => names.get(f.var_name[(slot & !VAR) as usize]),
        }
    }

    /// Drops the edges of the `cyclic` nodes (rebuilds the CSR).
    fn cut(&mut self) {
        // Rebuild the CSR without the cyclic nodes' edges.
        let mut start = vec![0u32];
        let mut deps = Vec::with_capacity(self.deps.len());
        for node in 0..self.owner.len() {
            if self
                .cyclic
                .binary_search_by_key(&(node as u32), |e| e.0)
                .is_err()
            {
                deps.extend_from_slice(self.node_deps(node as u32));
            }
            start.push(deps.len() as u32);
        }
        self.dep_start = start;
        self.deps = deps;
        self.dep_end = self.dep_start[1..].to_vec();
        assert_eq!(self.dep_end.len(), self.owner.len());
    }

    /// Returns the nodes node `n` depends on, in its program's identifier order.
    pub(crate) fn node_deps(&self, n: u32) -> &[u32] {
        &self.deps[self.dep_start[n as usize] as usize..self.dep_end[n as usize] as usize]
    }

    /// Replaces node `n`'s dependencies by `new` (in its program's identifier order) when
    /// every one precedes `n` in the topological order, which then stays valid; returns
    /// `false`, changing nothing, when one does not, when `n` is on a cycle, or when the
    /// appended lists outgrew the build (the caller rebuilds the graph, which compacts).
    /// Values are not touched: the caller marks `n` dirty.
    ///
    /// Cost: O(new) to write them, plus a copy of each new dependency's dependent list (it
    /// moves to the end with `n` added).
    pub(crate) fn set_deps(&mut self, n: u32, new: &[u32]) -> bool {
        let at = n as usize;
        assert!(at < self.len(), "node {n} in range");
        let r = self.rank[at];
        if new.iter().any(|&d| self.rank[d as usize] >= r)
            || self.cyclic.binary_search_by_key(&n, |e| e.0).is_ok()
            || self.deps.len() + self.rdeps.len() > 4 * self.edges + (1 << 16)
        {
            return false;
        }
        let (start, end) = (self.dep_start[at] as usize, self.dep_end[at] as usize);
        let old: Vec<u32> = self.deps[start..end].to_vec();
        if new.len() <= old.len() {
            self.deps[start..start + new.len()].copy_from_slice(new);
            self.dep_end[at] = (start + new.len()) as u32;
        } else {
            self.dep_start[at] = self.deps.len() as u32;
            self.deps.extend_from_slice(new);
            self.dep_end[at] = self.deps.len() as u32;
        }
        for &d in new.iter().filter(|d| !old.contains(d)) {
            let d = d as usize;
            let (a, b) = (self.rdep_start[d] as usize, self.rdep_end[d] as usize);
            if b != self.rdeps.len() {
                self.rdep_start[d] = self.rdeps.len() as u32;
                self.rdeps.extend_from_within(a..b);
            }
            self.rdeps.push(n);
            self.rdep_end[d] = self.rdeps.len() as u32;
        }
        assert_eq!(self.node_deps(n), new);
        true
    }

    /// Returns the node count.
    pub fn len(&self) -> usize {
        self.owner.len()
    }

    /// Returns whether the graph has no nodes.
    pub fn is_empty(&self) -> bool {
        self.owner.is_empty()
    }

    /// Returns the node of param `k` of instance row `i`, if the instance resolved.
    pub fn inst_param(&self, i: usize, k: usize) -> Option<u32> {
        let first = *self.inst_first.get(i)?;
        (first != NONE).then_some(first + k as u32)
    }

    /// Returns the program of node `n` and the table holding it.
    pub(crate) fn code<'a>(&'a self, n: u32, ifaces: &'a [Iface]) -> &'a PCode {
        let progs = self.progs_of(n, ifaces);
        &progs.code[self.prog[n as usize] as usize]
    }

    fn progs_of<'a>(&'a self, n: u32, ifaces: &'a [Iface]) -> &'a Progs {
        match self.table[n as usize] {
            OWN => &self.progs,
            t => &ifaces[t as usize].progs,
        }
    }

    /// Evaluates every node into `vals`/`unv` in topological order. With `fixed`, own
    /// params take those values (an elaboration context) instead of their defaults.
    pub(crate) fn eval_all<F: Funcs>(
        &self,
        fixed: Option<&[Val]>,
        vals: &mut Vec<Val>,
        unv: &mut Vec<bool>,
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
    ) {
        let n = self.len();
        vals.clear();
        vals.resize(n, Val::Upstream);
        unv.clear();
        unv.resize(n, false);
        if let Some(f) = fixed {
            assert_eq!(f.len(), self.params as usize, "one value per own param");
            vals[..f.len()].clone_from_slice(f);
        }
        for &node in &self.order {
            if fixed.is_some() && node < self.params {
                continue;
            }
            self.eval_node(node, vals, unv, ifaces, funcs, ws);
        }
    }

    /// Evaluates only the own params and vars (default context): they never read
    /// instance nodes, so this is their full evaluation at a fraction of the cost.
    /// Instance nodes are left as [`Val::Upstream`].
    pub(crate) fn eval_own<F: Funcs>(
        &self,
        vals: &mut Vec<Val>,
        unv: &mut Vec<bool>,
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
    ) {
        let n = self.len();
        vals.clear();
        vals.resize(n, Val::Upstream);
        unv.clear();
        unv.resize(n, false);
        for &node in &self.order {
            if self.owner[node as usize] == NONE {
                debug_assert!(
                    self.node_deps(node)
                        .iter()
                        .all(|&d| self.owner[d as usize] == NONE)
                );
                self.eval_node(node, vals, unv, ifaces, funcs, ws);
            }
        }
    }

    /// Re-evaluates the `dirty` nodes and everything depending on them, in order, and
    /// appends to `changed` each evaluated node whose value or `unverified` flag differs
    /// from before (in evaluation order). Returns the number of nodes evaluated. Valid
    /// while edges are unchanged.
    ///
    /// Cost: O(affected + their edges), plus O(nodes) once to size the marks.
    pub(crate) fn recompute<F: Funcs>(
        &mut self,
        dirty: &[u32],
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
        changed: &mut Vec<u32>,
    ) -> usize {
        self.affected(dirty, ws);
        let todo = std::mem::take(&mut ws.affected);
        let (mut vals, mut unv) = (
            std::mem::take(&mut self.vals),
            std::mem::take(&mut self.unverified),
        );
        let before = changed.len();
        for &node in &todo {
            // A node never reads itself (cycle nodes lost their edges): its old value can
            // step aside while it is evaluated.
            let was = std::mem::replace(&mut vals[node as usize], Val::Upstream);
            let was_unv = unv[node as usize];
            self.eval_node(node, &mut vals, &mut unv, ifaces, funcs, ws);
            if vals[node as usize] != was || unv[node as usize] != was_unv {
                changed.push(node);
            }
        }
        self.vals = vals;
        self.unverified = unv;
        let count = todo.len();
        assert!(
            changed.len() - before <= count,
            "only evaluated nodes change"
        );
        ws.affected = todo;
        count
    }

    /// The `dirty` nodes and every node depending on them, in topological order, into
    /// `ws.affected`. Marks are epoch stamps, so a walk clears nothing.
    fn affected(&self, dirty: &[u32], ws: &mut Work) {
        let n = self.len();
        if ws.seen.len() != n {
            ws.seen = vec![0; n];
            ws.epoch = 0;
        }
        ws.epoch = ws.epoch.wrapping_add(1);
        if ws.epoch == 0 {
            ws.seen.fill(0);
            ws.epoch = 1;
        }
        let (out, seen, epoch) = (&mut ws.affected, &mut ws.seen, ws.epoch);
        out.clear();
        for &d in dirty {
            assert!((d as usize) < n, "dirty node {d} out of range");
            if seen[d as usize] != epoch {
                seen[d as usize] = epoch;
                out.push(d);
            }
        }
        let mut i = 0;
        while i < out.len() {
            let v = out[i] as usize;
            for &w in &self.rdeps[self.rdep_start[v] as usize..self.rdep_end[v] as usize] {
                if seen[w as usize] != epoch {
                    seen[w as usize] = epoch;
                    out.push(w);
                }
            }
            i += 1;
        }
        out.sort_unstable_by_key(|&v| self.rank[v as usize]);
        assert!(out.len() <= n, "each node once");
    }

    fn eval_node<F: Funcs>(
        &self,
        node: u32,
        vals: &mut [Val],
        unv: &mut [bool],
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
    ) {
        if let Ok(i) = self.cyclic.binary_search_by_key(&node, |e| e.0) {
            vals[node as usize] = Val::Err(Box::new(self.cyclic[i].1.clone()));
            return;
        }
        let (val, u) = {
            let mut inputs: Vec<Value<'_>> = recycle(std::mem::take(&mut ws.inputs));
            let mut upstream = false;
            let mut u = false;
            for &d in self.node_deps(node) {
                match vals[d as usize].value() {
                    Some(v) => inputs.push(v),
                    None => upstream = true,
                }
                u |= unv[d as usize];
            }
            let r = if upstream {
                (Val::Upstream, false)
            } else {
                self.progs_of(node, ifaces).eval(
                    self.prog[node as usize],
                    &inputs,
                    funcs,
                    &mut ws.scratch,
                    &mut ws.text,
                )
            };
            ws.inputs = recycle(inputs);
            (r.0, r.1 || u)
        };
        vals[node as usize] = val;
        unv[node as usize] = u;
    }

    /// Evaluates bound `which` (`MIN`, `MAX`, `STEP`, `GRID`, `VALID`) of param `k` for
    /// instance row `i` in its child scope; `None` when absent or failing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bound<F: Funcs>(
        &self,
        i: usize,
        k: usize,
        which: usize,
        vals: &[Val],
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
    ) -> Option<Val> {
        assert!(matches!(which, MIN | MAX | STEP | GRID | VALID));
        let first = self.inst_first[i];
        assert!(first != NONE, "gating needs a resolved instance");
        let c = &ifaces[self.inst_iface[i] as usize];
        let b = c.param_bound[k][which];
        if b == NONE {
            return None;
        }
        if let Some(v) = &c.bound_const[k][which] {
            return Some(v.clone());
        }
        let mut inputs: Vec<Value<'_>> = recycle(std::mem::take(&mut ws.inputs));
        for &r in c.progs.refs(b) {
            let d = if r & VAR == 0 {
                first + r
            } else {
                let key = (i as u32, r & !VAR);
                let at = self
                    .child_vars
                    .binary_search_by_key(&key, |e| (e.0, e.1))
                    .ok()?;
                self.child_vars[at].2
            };
            inputs.push(vals[d as usize].value()?);
        }
        let (v, _) = c
            .progs
            .eval(b, &inputs, funcs, &mut ws.scratch, &mut ws.text);
        ws.inputs = recycle(inputs);
        (!v.is_err()).then_some(v)
    }
}

/// Reusable evaluation memory.
#[derive(Debug, Default)]
pub(crate) struct Work {
    pub(crate) scratch: Scratch,
    pub(crate) text: String,
    inputs: Vec<Value<'static>>,
    affected: Vec<u32>,
    /// Epoch marks of the affected walk, one per node.
    seen: Vec<u32>,
    epoch: u32,
}

/// Reuses a value buffer's allocation across lifetimes (clears it).
fn recycle<'b>(mut v: Vec<Value<'_>>) -> Vec<Value<'b>> {
    v.clear();
    v.into_iter()
        .map(|_| -> Value<'b> { unreachable!("cleared") })
        .collect()
}

fn own_node(params: u32, r: Ref) -> u32 {
    if r & VAR == 0 { r } else { params + (r & !VAR) }
}

/// Doc symbols of the own params' expressions (computed, else default) and own vars'.
fn own_sources(doc: &Document) -> (Vec<Sym>, Vec<Sym>) {
    let s = doc.strings();
    let params = doc.params();
    let ps = (0..params.kind.len())
        .map(|r| {
            let computed = doc
                .pool()
                .find_attr(s, params.attrs[r], "computed")
                .and_then(|v| v.first().copied());
            computed.unwrap_or(params.fields[r][1])
        })
        .collect();
    let blocks = doc.doc_blocks();
    let vs = (0..blocks.kind.len())
        .filter(|&r| blocks.kind[r] == schemify_format::RecordKind::Var)
        .map(|r| blocks.fields[r][1])
        .collect();
    (ps, vs)
}
