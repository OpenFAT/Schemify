//! One cell document and its interfaces in, its expression graph out (D19, expr spec §4):
//! one node per value the cell defines or instantiates, edges "depends on", a topological
//! order, and the in-place dependency edits an incremental recompute needs. Evaluation
//! over the graph is `eval`'s.
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
//! Six questions: in, a document + interfaces; out, SoA node columns. Nodes ≤ instances ×
//! 2^8 params (2^30 bound, `u32`). Eval walks `order` reading each node's deps (a
//! `[start, end)` range per node: built as CSR, an edit that changes a node's dependencies
//! writes them in place or appends them, [`Graph::set_deps`]). Lifetime: until a
//! structural edit (the editor) or one elaboration. Parallel: one graph per cell,
//! independent.
//!
//! Contents:
//! - `pub struct Graph`: `len`, `is_empty`, `inst_param`; `pub(crate)` `build`,
//!   `node_param`, `child_var`, `node_deps`, `set_deps`, `code`, `progs_of`
//! - `pub(crate) const OWN`
//! - `pub(crate) fn override_of(doc: &Document, i: usize, key: &str) -> Option<Sym>`
//! - `pub(crate) fn cached_of(doc, i, key) -> (Option<(&str, &str)>, bool)`
//! - `pub(crate) fn own_node(params: u32, r: Ref) -> u32`

use crate::iface::Iface;
use crate::names::{Name, Names};
use crate::values::{Code as PCode, NONE, Progs, Ref, Syntax, VAR, Val};
use schemify_doc::{Document, Instances, Sym};
use schemify_expr::{Deps, Error as XError, reverse, topo_order, topo_order_all};
use std::collections::HashMap;

/// One cell's dependency graph and its current values.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    /// Instance row owning the node, or `u32::MAX` for own params/vars.
    pub owner: Vec<u32>,
    /// Param index (own or child), or `VAR | var index` for var nodes.
    pub(crate) slot: Vec<u32>,
    /// Program table of the node: [`OWN`], or the interface index holding it.
    pub(crate) table: Vec<u32>,
    /// Program index in that table.
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
    /// compacts) once the appended lists pass four times this.
    pub(crate) edges: usize,
    /// Position of each node in `order`.
    pub(crate) rank: Vec<u32>,
    /// Nodes in topological order (dependencies first).
    pub(crate) order: Vec<u32>,
    /// Nodes on a dependency cycle (`X005`), with the error, sorted by node.
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
    /// Programs the graph compiled itself: overrides and substituted vars.
    pub(crate) progs: Progs,
}

/// Program table of a node: the graph's own table (overrides, substituted vars), or the
/// interface at this index.
pub(crate) const OWN: u32 = u32::MAX - 1;

impl Graph {
    /// Builds the graph of `doc`. `own` indexes `ifaces` (the cell's own interface);
    /// `inst_iface[i]` is instance row `i`'s interface or `u32::MAX`. `subst` replaces
    /// own var expressions by name (later entries win), adding vars that do not exist.
    /// Values start as [`Val::Upstream`]; `eval` fills them.
    ///
    /// Cost: O(nodes + edges) plus one compile per distinct override text.
    ///
    /// # Panics
    ///
    /// Panics if `inst_iface` is not one slot per instance row.
    pub(crate) fn build(
        doc: &Document,
        own: u32,
        ifaces: &[Iface],
        inst_iface: &[u32],
        subst: &[(Name, &str)],
        names: &Names,
    ) -> Graph {
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
        g.own_nodes(doc, me, subst, names);
        g.inst_blocks(doc, me, ifaces, names);
        g.resolve_deps(ifaces);
        g.finish(ifaces, names);
        assert_eq!(g.inst_first.len(), ni);
        g
    }

    /// The param or var node `n` is the value of: an own param or var, or a param or
    /// var of its instance's cell.
    pub(crate) fn node_param(&self, n: u32, ifaces: &[Iface]) -> Name {
        let (owner, slot) = (self.owner[n as usize], self.slot[n as usize]);
        let f = if owner == NONE {
            &ifaces[self.own as usize]
        } else {
            &ifaces[self.inst_iface[owner as usize] as usize]
        };
        match (slot & VAR == 0, owner == NONE) {
            (true, _) => f.param_name[slot as usize],
            (false, true) => self.var_name[(slot & !VAR) as usize],
            (false, false) => f.var_name[(slot & !VAR) as usize],
        }
    }

    /// The node of child var `v` of instance row `i`, if the graph made one.
    pub(crate) fn child_var(&self, i: u32, v: u32) -> Option<u32> {
        let at = self
            .child_vars
            .binary_search_by_key(&(i, v), |e| (e.0, e.1))
            .ok()?;
        Some(self.child_vars[at].2)
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

    /// Returns the compiled program of node `n`.
    pub(crate) fn code<'a>(&'a self, n: u32, ifaces: &'a [Iface]) -> &'a PCode {
        let progs = self.progs_of(n, ifaces);
        &progs.code[self.prog[n as usize] as usize]
    }

    /// Returns the program table holding node `n`'s program.
    pub(crate) fn progs_of<'a>(&'a self, n: u32, ifaces: &'a [Iface]) -> &'a Progs {
        match self.table[n as usize] {
            OWN => &self.progs,
            t => &ifaces[t as usize].progs,
        }
    }
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

/// The own-scope node of ref `r` (params first, then vars).
pub(crate) fn own_node(params: u32, r: Ref) -> u32 {
    if r & VAR == 0 { r } else { params + (r & !VAR) }
}

// ---- private ----

impl Graph {
    /// Own param and var nodes; `subst` replaces var expressions or adds vars (later wins).
    fn own_nodes(&mut self, doc: &Document, me: &Iface, subst: &[(Name, &str)], names: &Names) {
        let mut var_subst: Vec<Option<&str>> = vec![None; me.var_name.len()];
        for &(n, text) in subst {
            match self.var_name.iter().position(|v| *v == n) {
                Some(v) => var_subst[v] = Some(text),
                None => {
                    self.var_name.push(n);
                    var_subst.push(Some(text));
                }
            }
        }
        self.vars = self.var_name.len() as u32;
        let own = self.own;
        let (param_src, var_src) = own_sources(doc);
        for k in 0..self.params {
            let src = param_src.get(k as usize).copied().unwrap_or(Sym::NONE);
            self.push(NONE, k, own, me.param_prog[k as usize], src);
        }
        for v in 0..self.vars {
            let src = var_src.get(v as usize).copied().unwrap_or(Sym::NONE);
            match var_subst[v as usize] {
                Some(text) => {
                    let lookup =
                        |n: &str| crate::iface::own_ref(names, &me.param_name, &self.var_name, n);
                    let prog = self.progs.push(text, Syntax::Expr, None, lookup);
                    self.push(NONE, VAR | v, OWN, prog, Sym::NONE);
                }
                None => self.push(NONE, VAR | v, own, me.var_prog[v as usize], src),
            }
        }
        assert_eq!(self.len(), (self.params + self.vars) as usize);
    }

    /// One node per param of each resolved instance: its override (own scope, one compile
    /// per distinct text), else the cell's program in the child scope.
    fn inst_blocks(&mut self, doc: &Document, me: &Iface, ifaces: &[Iface], names: &Names) {
        let s = doc.strings();
        let mut dedupe: HashMap<(Sym, bool), u32> = HashMap::new();
        for i in 0..self.inst_iface.len() {
            let t = self.inst_iface[i];
            let Some(c) = ifaces.get(t as usize) else {
                self.inst_first.push(NONE);
                continue;
            };
            self.inst_first.push(self.len() as u32);
            for k in 0..c.param_name.len() {
                let ovr = if c.param_computed[k] {
                    None // E030: the override is reported and ignored
                } else {
                    override_of(doc, i, names.get(c.param_name[k]))
                };
                let Some(sym) = ovr else {
                    self.push(i as u32, k as u32, t, c.param_prog[k], Sym::NONE);
                    continue;
                };
                let syntax = c.param_kind[k].syntax();
                let key = (sym, syntax == Syntax::Template);
                let prog = match dedupe.get(&key) {
                    Some(&prog) => prog,
                    None => {
                        let lookup = |n: &str| {
                            crate::iface::own_ref(names, &me.param_name, &self.var_name, n)
                        };
                        let prog = self.progs.push(s.get(sym), syntax, None, lookup);
                        dedupe.insert(key, prog);
                        prog
                    }
                };
                self.push(i as u32, k as u32, OWN, prog, sym);
            }
        }
        assert_eq!(self.inst_first.len(), self.inst_iface.len());
    }

    /// The CSR of each node's dependencies; child var nodes are appended (and resolved in
    /// turn) as references meet them.
    fn resolve_deps(&mut self, ifaces: &[Iface]) {
        self.dep_start.push(0);
        self.dep_start.reserve(self.len());
        let mut refs: Vec<Ref> = Vec::new(); // reused: one allocation, not one per node
        let mut n = 0;
        while n < self.len() {
            let (owner, table, prog) = (self.owner[n], self.table[n], self.prog[n]);
            let child_scope = owner != NONE && table != OWN;
            let progs = if table == OWN {
                &self.progs
            } else {
                &ifaces[table as usize].progs
            };
            refs.clear();
            refs.extend_from_slice(progs.refs(prog));
            for &r in &refs {
                let d = if child_scope {
                    self.child_ref(owner, table, r, ifaces)
                } else {
                    own_node(self.params, r)
                };
                self.deps.push(d);
            }
            if child_scope && self.slot[n] & VAR == 0 {
                // Bounds may read child vars: make sure their nodes exist for gating.
                let c = &ifaces[table as usize];
                for b in c.param_bound[self.slot[n] as usize] {
                    if b == NONE {
                        continue;
                    }
                    for &r in c.progs.refs(b).iter().filter(|&&r| r & VAR != 0) {
                        self.child_ref(owner, table, r, ifaces);
                    }
                }
            }
            self.dep_start.push(self.deps.len() as u32);
            n += 1;
        }
        assert_eq!(self.dep_start.len(), self.len() + 1);
    }

    fn push(&mut self, owner: u32, slot: u32, table: u32, prog: u32, src: Sym) {
        assert!(
            self.owner.len() + 1 < NONE as usize - 1,
            "graph node capacity"
        );
        self.owner.push(owner);
        self.slot.push(slot);
        self.table.push(table);
        self.prog.push(prog);
        self.src.push(src);
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
                let node = self.len() as u32;
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
        let n = self.len();
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
                let err = c.error(|n| names.get(self.node_param(n, ifaces)));
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

    /// Drops the edges of the `cyclic` nodes (rebuilds the CSR).
    fn cut(&mut self) {
        let mut start = vec![0u32];
        let mut deps = Vec::with_capacity(self.deps.len());
        for node in 0..self.len() as u32 {
            if self.cyclic.binary_search_by_key(&node, |e| e.0).is_err() {
                deps.extend_from_slice(self.node_deps(node));
            }
            start.push(deps.len() as u32);
        }
        self.dep_start = start;
        self.deps = deps;
        self.dep_end = self.dep_start[1..].to_vec();
        assert_eq!(self.dep_end.len(), self.len());
    }
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
