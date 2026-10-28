//! Value edits in, the graph's programs and edges patched and the dirty nodes out (D19,
//! D20): what lets [`CellState::recompute`] re-evaluate only an edit's dependents. A
//! change the graph cannot absorb in place (a structural one) returns `false` and the
//! caller rebuilds the graph.
//!
//! Contents:
//! - `CellState::diff_values`, `CellState::refresh_cached` (`pub(super)`)

use super::CellState;
use crate::graph::{OWN, override_of, own_node};
use crate::iface::own_ref;
use crate::values::{NONE, Syntax, VAR};
use schemify_doc::{Document, ObjId, Sym};
use schemify_format::RecordKind;

/// A node whose program source changed.
struct Retext {
    node: u32,
    /// Own-scope text, or `None` (back to the cell's default in the child scope).
    text: Option<Source>,
    /// Instance row and param index of an override node (both 0 for a var).
    inst: usize,
    param: usize,
}

/// An own-scope program source: its text and how it is read.
struct Source {
    sym: Sym,
    syntax: Syntax,
}

impl CellState {
    /// Recompiles the expressions whose text changed, and points a node whose override was
    /// added or removed at its new program (own scope, or the cell's default in the child
    /// scope). Returns `false` when the change is structural: a new dependency that does not
    /// precede the node in the topological order (`Graph::set_deps`), a child var the
    /// graph has no node for, a cycle, or a var added or removed.
    pub(super) fn diff_values(
        &mut self,
        doc: &Document,
        insts: &[ObjId],
        vars: bool,
        dirty: &mut Vec<u32>,
    ) -> bool {
        if !self.graph.cyclic.is_empty() {
            // Cycle nodes had their edges cut; any edit may break or move the cycle.
            return false;
        }
        let mut nodes: Vec<Retext> = Vec::new();
        for id in insts {
            let Some(loc) = doc.find_id(*id) else {
                return false;
            };
            self.inst_texts(doc, loc.row as usize, &mut nodes);
        }
        if vars && !self.var_texts(doc, &mut nodes) {
            return false;
        }
        let s = doc.strings();
        let g = &mut self.graph;
        assert_eq!(g.prog.len(), g.len(), "node columns in step");
        assert_eq!(g.src.len(), g.len(), "node columns in step");
        let me = &self.ifaces[0];
        let mut new_deps: Vec<u32> = Vec::new();
        for Retext {
            node: n,
            text,
            inst: i,
            param: k,
        } in nodes
        {
            assert!((n as usize) < g.len(), "node {n} exists");
            new_deps.clear();
            let (table, prog, src) = match text {
                Some(Source { sym, syntax }) => {
                    let names = &self.names;
                    let lookup = |name: &str| own_ref(names, &me.param_name, &g.var_name, name);
                    let prog = g.progs.push(s.get(sym), syntax, None, lookup);
                    let refs = g.progs.refs(prog).iter();
                    new_deps.extend(refs.map(|&r| own_node(g.params, r)));
                    (OWN, prog, sym)
                }
                None => {
                    // The cell's default reads the instance's own params and child vars.
                    let t = self.inst_iface[i];
                    let c = &self.ifaces[t as usize];
                    let prog = c.param_prog[k];
                    // A build gives the child vars its bounds read nodes (for gating).
                    let bounds = c.param_bound[k].iter().filter(|&&b| b != NONE);
                    if bounds
                        .flat_map(|&b| c.progs.refs(b))
                        .any(|&r| r & VAR != 0 && g.child_var(i as u32, r & !VAR).is_none())
                    {
                        return false;
                    }
                    for &r in c.progs.refs(prog) {
                        let d = if r & VAR == 0 {
                            g.inst_first[i] + r
                        } else {
                            match g.child_var(i as u32, r & !VAR) {
                                Some(d) => d,
                                None => return false, // a node the graph lacks
                            }
                        };
                        new_deps.push(d);
                    }
                    (t, prog, Sym::NONE)
                }
            };
            if new_deps != g.node_deps(n) && !g.set_deps(n, &new_deps) {
                return false;
            }
            g.table[n as usize] = table;
            g.prog[n as usize] = prog;
            g.src[n as usize] = src;
            dirty.push(n);
        }
        assert!(dirty.iter().all(|&d| (d as usize) < g.len()));
        true
    }

    /// Points the programs of own vars `vars` and own params `params` at their current
    /// `cached` attribute (D20), marking dirty the ones whose value rests on it. A cache write
    /// never changes a value that a loaded plugin computed, so it costs no rebuild. Returns
    /// `false` when a name is not an own var / param (the caller rebuilds).
    pub(super) fn refresh_cached(
        &mut self,
        doc: &Document,
        vars: &[&str],
        params: &[&str],
        dirty: &mut Vec<u32>,
    ) -> bool {
        let s = doc.strings();
        let (blocks, prows) = (doc.doc_blocks(), doc.params());
        let g = &mut self.graph;
        assert_eq!(g.unverified.len(), g.len(), "node columns in step");
        assert_eq!(g.vals.len(), g.len(), "node columns in step");
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
            assert!(prog < progs.cached.len(), "node {node}'s program exists");
            progs.cached[prog] = cached;
            if g.unverified[node as usize] || g.vals[node as usize].is_err() {
                dirty.push(node);
            }
        }
        debug_assert!(dirty.iter().all(|&d| (d as usize) < g.len()));
        true
    }
}

// ---- private ----

impl CellState {
    /// Appends the param nodes of instance row `i` whose source changed (an override set,
    /// changed or removed); none for an unresolved row. Override keys the cell lacks
    /// change nothing but `W007` (rechecked by the caller).
    fn inst_texts(&self, doc: &Document, i: usize, out: &mut Vec<Retext>) {
        let g = &self.graph;
        assert!(i < self.inst_iface.len(), "instance row {i} exists");
        let Some(c) = self.ifaces.get(self.inst_iface[i] as usize) else {
            return;
        };
        let first = g.inst_first[i];
        assert!(
            first as usize + c.param_name.len() <= g.len(),
            "instance {i}'s param nodes exist"
        );
        for k in 0..c.param_name.len() {
            let n = first + k as u32;
            let now = if c.param_computed[k] {
                None
            } else {
                override_of(doc, i, self.names.get(c.param_name[k]))
            };
            let was = (g.table[n as usize] == OWN).then_some(g.src[n as usize]);
            let text = match (was, now) {
                (None, None) => continue,
                (Some(a), Some(b)) if a == b => continue,
                (_, Some(sym)) => Some(Source {
                    sym,
                    syntax: c.param_kind[k].syntax(),
                }),
                (Some(_), None) => None,
            };
            out.push(Retext {
                node: n,
                text,
                inst: i,
                param: k,
            });
        }
    }

    /// Appends the own var nodes whose expression text changed. `false` when a var was
    /// added or removed (structural).
    fn var_texts(&self, doc: &Document, out: &mut Vec<Retext>) -> bool {
        let (g, blocks) = (&self.graph, doc.doc_blocks());
        let rows: Vec<usize> = (0..blocks.kind.len())
            .filter(|&r| blocks.kind[r] == RecordKind::Var)
            .collect();
        if rows.len() != self.ifaces[0].var_name.len() {
            return false;
        }
        assert!(
            g.params as usize + rows.len() <= g.len(),
            "every own var has a node after the own params"
        );
        for (v, r) in rows.into_iter().enumerate() {
            let n = g.params + v as u32;
            let sym = blocks.fields[r][1];
            if g.src[n as usize] != sym {
                out.push(Retext {
                    node: n,
                    text: Some(Source {
                        sym,
                        syntax: Syntax::Expr,
                    }),
                    inst: 0,
                    param: 0,
                });
            }
        }
        true
    }
}
