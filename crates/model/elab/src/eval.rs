//! A built [`Graph`] and its programs in, node values out (D19, D20): full evaluation in
//! topological order, own-scope-only evaluation (a cell's defaults), incremental
//! re-evaluation over the affected set after an edit, and one bound of one instance param
//! for gating (D13). Cites expr spec §4 (scopes, cycles) and D20 (`unverified` values).
//!
//! Contents:
//! - `pub(crate) struct Work`, `pub(crate) struct BoundOf`
//! - `Graph::eval_all`, `Graph::eval_own`, `Graph::recompute`, `Graph::bound`
//!   (all `pub(crate)`)

use crate::graph::Graph;
use crate::iface::{GRID, Iface, MAX, MIN, STEP, VALID};
use crate::values::{Evaluated, Funcs, NONE, VAR, Val};
use crate::violations::Code;
use schemify_expr::{Scratch, Value};
use std::mem;

/// Reusable evaluation memory.
#[derive(Debug, Default)]
pub(crate) struct Work {
    pub(crate) scratch: Scratch,
    pub(crate) text: String,
    /// Gating findings of one instance param, `(code, detail)` ([`crate::check::gate`]).
    pub(crate) gated: Vec<(Code, String)>,
    inputs: Vec<Value<'static>>,
    affected: Vec<u32>,
    /// Epoch marks of the affected walk, one per node.
    seen: Vec<u32>,
    epoch: u32,
}

/// One bound of one instance param: what [`Graph::bound`] evaluates.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoundOf {
    /// Instance row.
    pub(crate) inst: usize,
    /// Param index in the instance's interface.
    pub(crate) param: usize,
    /// `MIN`, `MAX`, `STEP`, `GRID` or `VALID`.
    pub(crate) which: usize,
}

impl Graph {
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
        assert_eq!(vals.len(), n);
        assert_eq!(unv.len(), n);
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
        assert_eq!(vals.len(), n);
        assert_eq!(unv.len(), n);
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
        assert_eq!(
            self.vals.len(),
            self.len(),
            "recompute needs evaluated values"
        );
        assert_eq!(self.unverified.len(), self.len());
        self.affected(dirty, ws);
        let todo = mem::take(&mut ws.affected);
        let (mut vals, mut unv) = (mem::take(&mut self.vals), mem::take(&mut self.unverified));
        let before = changed.len();
        for &node in &todo {
            // A node never reads itself (cycle nodes lost their edges): its old value can
            // step aside while it is evaluated.
            let was = mem::replace(&mut vals[node as usize], Val::Upstream);
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

    /// Evaluates bound `at.which` of param `at.param` for instance row `at.inst` in its
    /// child scope; `None` when absent or failing.
    pub(crate) fn bound<F: Funcs>(
        &self,
        at: BoundOf,
        vals: &[Val],
        ifaces: &[Iface],
        funcs: &mut F,
        ws: &mut Work,
    ) -> Option<Val> {
        let BoundOf {
            inst: i,
            param: k,
            which,
        } = at;
        assert!(matches!(which, MIN | MAX | STEP | GRID | VALID));
        assert_eq!(vals.len(), self.len(), "values indexed like the graph");
        let first = self.inst_first[i];
        assert!(first != NONE, "gating needs a resolved instance");
        let c = &ifaces[self.inst_iface[i] as usize];
        assert!(k < c.param_name.len(), "param {k} of instance {i}");
        let b = c.param_bound[k][which];
        if b == NONE {
            return None;
        }
        let mut inputs: Vec<Value<'_>> = recycle(mem::take(&mut ws.inputs));
        for &r in c.progs.refs(b) {
            let d = if r & VAR == 0 {
                first + r
            } else {
                self.child_var(i as u32, r & !VAR)?
            };
            inputs.push(vals[d as usize].value()?);
        }
        let r = c
            .progs
            .eval(b, &inputs, funcs, &mut ws.scratch, &mut ws.text);
        ws.inputs = recycle(inputs);
        (!r.val.is_err()).then_some(r.val)
    }
}

// ---- private ----

impl Graph {
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
        // Each node enters `out` once (its mark), so the walk ends after at most n steps.
        let mut i = 0;
        while i < out.len() {
            debug_assert!(i < n, "the affected walk visits each node once");
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
        assert!((node as usize) < vals.len(), "node {node} has a value slot");
        assert_eq!(vals.len(), unv.len(), "value columns in step");
        if let Ok(i) = self.cyclic.binary_search_by_key(&node, |e| e.node) {
            vals[node as usize] = Val::Err(Box::new(self.cyclic[i].error.clone()));
            return;
        }
        let (val, u) = {
            let mut inputs: Vec<Value<'_>> = recycle(mem::take(&mut ws.inputs));
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
                Evaluated {
                    val: Val::Upstream,
                    unverified: false,
                }
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
            (r.val, r.unverified || u)
        };
        vals[node as usize] = val;
        unv[node as usize] = u;
    }
}

/// Reuses a value buffer's allocation across lifetimes (clears it).
fn recycle<'b>(mut v: Vec<Value<'_>>) -> Vec<Value<'b>> {
    v.clear();
    v.into_iter()
        .map(|_| -> Value<'b> { unreachable!("cleared") })
        .collect()
}
