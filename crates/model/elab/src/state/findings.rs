//! Findings of one open document (netlist spec §8, D13): the connectivity patch's and the
//! touched instances' findings in, [`CellState::violations`] updated in place and the
//! pass delta ([`CellState::changes`]) out; or the full check pass.
//!
//! Contents:
//! - `CellState::check`, `CellState::next_pass`, `CellState::apply_net_delta`,
//!   `CellState::recheck` (`pub(super)`)

use super::{CellState, RowKey};
use crate::check::{
    CellCtx, INST_CODES, check_overrides, check_values, inst_overrides, inst_rules, value_errors,
    waivers,
};
use crate::live::Delta;
use crate::values::Funcs;
use crate::violations::{Violation, Violations, cmp_key};
use schemify_doc::{Document, ObjId};
use std::cmp::Ordering;

/// Process-unique check pass ids (0 is "no pass").
static PASSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Changes past this count merge in one pass instead of a memmove per row.
const MERGE_MIN: usize = 32;

impl CellState {
    /// Recomputes every finding (the full pass).
    pub(super) fn check<F: Funcs>(&mut self, doc: &Document, funcs: &mut F) {
        self.waivers = waivers(doc, &self.names);
        let mut net = self.live.rows(&self.nets, &self.ifaces[0], &self.names);
        net.sort_by(|a, b| cmp_key(a, b, &self.names));
        // Bulk: one stable sort by raw key (runs keep their key order), then the map built
        // from the sorted runs.
        let mut keyed: Vec<(RowKey, Violation)> =
            net.iter().map(|v| (row_key(v), v.clone())).collect();
        keyed.sort_by_key(|e| e.0);
        let mut runs: Vec<(RowKey, Vec<Violation>)> = Vec::new();
        for (k, v) in keyed {
            match runs.last_mut() {
                Some(r) if r.0 == k => r.1.push(v),
                _ => runs.push((k, vec![v])),
            }
        }
        self.net_rows = runs.into_iter().collect();
        let mut rows = net;
        let cx = CellCtx {
            doc,
            ifaces: &self.ifaces,
            own: 0,
            inst_iface: &self.inst_iface,
            graph: &self.graph,
            waivers: &self.waivers,
        };
        check_overrides(&cx, &self.unresolved, &mut self.names, &mut rows);
        let vals = &self.graph.vals;
        check_values(&cx, vals, &self.names, funcs, &mut self.ws, &mut rows);
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
    pub(super) fn next_pass(&mut self, changes: (Vec<Violation>, Vec<Violation>)) {
        self.prev_pass = self.pass;
        self.pass = PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.changes = changes;
        assert!(self.pass > self.prev_pass);
    }

    /// Posts a connectivity patch's findings: `gone` leave the table and `new` enter it
    /// (rows on both sides cancel). Returns what the table lost and gained, each in key
    /// order (the caller's pass delta). The table keeps one row per key, so a key leaves
    /// only with its last connectivity row.
    ///
    /// Cost: O((gone + new) · log rows) compares plus a memmove per changed row.
    pub(super) fn apply_net_delta(&mut self, (mut gone, mut new): Delta) -> Delta {
        let names = &self.names;
        let order = |a: &Violation, b: &Violation| {
            cmp_key(a, b, names).then_with(|| a.detail.cmp(&b.detail))
        };
        gone.sort_by(order);
        new.sort_by(order);
        let (g, n) = cancel(gone, new, order);
        let (rows, net) = (&mut self.violations.rows, &mut self.net_rows);
        let mut changes: Delta = (Vec::new(), Vec::new());
        for r in g {
            let key = row_key(&r);
            let run = net
                .get_mut(&key)
                .expect("a gone finding is a connectivity row");
            let at = run
                .iter()
                .position(|x| *x == r)
                .expect("a gone finding is a connectivity row");
            run.remove(at);
            let kept = run.first().cloned();
            if run.is_empty() {
                net.remove(&key);
            }
            let t = rows
                .binary_search_by(|x| cmp_key(x, &r, names))
                .expect("its key is in the table");
            match kept {
                None => changes.0.push(rows.remove(t)),
                Some(k) if rows[t] == r => {
                    changes.0.push(std::mem::replace(&mut rows[t], k.clone()));
                    changes.1.push(k);
                }
                Some(_) => {}
            }
        }
        for r in n {
            net.entry(row_key(&r)).or_default().push(r.clone());
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
        changes
    }

    /// Reruns the per-instance checks of instance rows `rows` (sorted, unique) and, with
    /// `own`, the own scope's `E008` rows, and replaces those rows of the table. Returns what
    /// the table lost and gained (key order; a row whose key stayed but whose detail changed
    /// is in both), or `None` near [`crate::VIOLATIONS_MAX`] (the caller runs the full pass,
    /// which truncates).
    ///
    /// Equals the full pass's rows because every row of a per-instance code
    /// (`check::INST_CODES`) is a function of its instance's nodes, attributes and the
    /// waivers, and rows of one key are emitted in the full pass's order.
    ///
    /// Cost: O(rows' nodes) to check, O(codes · rows · log table) to find the old rows, and a
    /// memmove per changed row (one merge pass past [`MERGE_MIN`] changes).
    pub(super) fn recheck<F: Funcs>(
        &mut self,
        doc: &Document,
        rows: &[u32],
        own: bool,
        funcs: &mut F,
    ) -> Option<Delta> {
        assert!(
            rows.windows(2).all(|w| w[0] < w[1]),
            "rows sorted and unique"
        );
        if self.violations.rows.len() >= crate::VIOLATIONS_MAX / 2 {
            return None;
        }
        if rows.is_empty() && !own {
            return Some((Vec::new(), Vec::new()));
        }
        let fresh = self.fresh_rows(doc, rows, own, funcs);
        let names = &self.names;
        let order = |a: &Violation, b: &Violation| cmp_key(a, b, names);
        let old = self.old_rows(doc, rows, own);
        // Rows on both sides cancel.
        let table = &self.violations.rows;
        let (mut gone, mut new): (Vec<usize>, Vec<Violation>) = (Vec::new(), Vec::new());
        let (mut a, mut b) = (0, 0);
        while a < old.len() || b < fresh.len() {
            match (old.get(a), fresh.get(b)) {
                (Some(&o), Some(f)) if table[o] == *f => (a, b) = (a + 1, b + 1),
                (Some(&o), Some(f)) if order(&table[o], f).is_le() => {
                    gone.push(o);
                    a += 1;
                }
                (Some(_), Some(f)) | (None, Some(f)) => {
                    new.push(f.clone());
                    b += 1;
                }
                (Some(&o), None) => {
                    gone.push(o);
                    a += 1;
                }
                (None, None) => unreachable!("loop condition"),
            }
        }
        let rows = &mut self.violations.rows;
        let lost: Vec<Violation> = gone.iter().map(|&o| rows[o].clone()).collect();
        if gone.len() + new.len() > MERGE_MIN {
            let mut out = Vec::with_capacity(rows.capacity());
            let (mut k, mut n) = (0, new.iter().peekable());
            for (t, r) in std::mem::take(rows).into_iter().enumerate() {
                if gone.get(k) == Some(&t) {
                    k += 1;
                    continue;
                }
                while let Some(x) = n.next_if(|x| order(x, &r).is_lt()) {
                    out.push(x.clone());
                }
                out.push(r);
            }
            out.extend(n.cloned());
            *rows = out;
        } else {
            for &o in gone.iter().rev() {
                rows.remove(o);
            }
            for r in &new {
                let t = rows
                    .binary_search_by(|x| order(x, r))
                    .expect_err("a fresh key is not in the table");
                rows.insert(t, r.clone());
            }
        }
        debug_assert!(rows.windows(2).all(|w| order(&w[0], &w[1]).is_lt()));
        assert_eq!(lost.len(), gone.len());
        Some((lost, new))
    }
}

// ---- private ----

impl CellState {
    /// The per-instance findings of instance rows `rows` (and the own scope's with `own`),
    /// sorted and one per key.
    fn fresh_rows<F: Funcs>(
        &mut self,
        doc: &Document,
        rows: &[u32],
        own: bool,
        funcs: &mut F,
    ) -> Vec<Violation> {
        let g = &self.graph;
        let mut nodes: Vec<u32> = if own {
            (0..g.params + g.vars).collect()
        } else {
            Vec::new()
        };
        for &i in rows {
            let i = i as usize;
            let Some(c) = self.ifaces.get(self.inst_iface[i] as usize) else {
                continue;
            };
            let first = g.inst_first[i];
            nodes.extend(first..first + c.param_name.len() as u32);
            let at = g.child_vars.partition_point(|e| e.0 < i as u32);
            let child = g.child_vars[at..].iter().take_while(|e| e.0 == i as u32);
            nodes.extend(child.map(|e| e.2));
        }
        nodes.sort_unstable();
        let cx = CellCtx {
            doc,
            ifaces: &self.ifaces,
            own: 0,
            inst_iface: &self.inst_iface,
            graph: g,
            waivers: &self.waivers,
        };
        let mut fresh = Vec::new();
        for &i in rows {
            let why = self.unresolved[i as usize].as_deref();
            inst_overrides(&cx, i as usize, why, &mut self.names, &mut fresh);
        }
        value_errors(&cx, &g.vals, nodes.into_iter(), &mut fresh);
        let mut found = Vec::new();
        for &i in rows {
            let (names, ws) = (&self.names, &mut self.ws);
            inst_rules(
                &cx, i as usize, &g.vals, names, funcs, ws, &mut found, &mut fresh,
            );
        }
        let order = |a: &Violation, b: &Violation| cmp_key(a, b, &self.names);
        fresh.sort_by(order);
        fresh.dedup_by(|b, a| order(a, b).is_eq());
        fresh
    }

    /// Indices of the table rows the per-instance checks wrote last time for `rows` (and
    /// the own scope with `own`), ascending: per code, a run sorted by object (one cell).
    fn old_rows(&self, doc: &Document, rows: &[u32], own: bool) -> Vec<usize> {
        let inst = doc.instances();
        let mut objs: Vec<ObjId> = rows.iter().map(|&r| inst.id[r as usize]).collect();
        if own {
            objs.push(ObjId::NONE);
        }
        objs.sort_unstable();
        let (table, cell) = (&self.violations.rows, self.ifaces[0].cell);
        let mut old: Vec<usize> = Vec::new();
        for code in INST_CODES {
            let lo = table.partition_point(|v| v.code.as_str() < code.as_str());
            let hi = lo + table[lo..].partition_point(|v| v.code == code);
            let run = &table[lo..hi];
            debug_assert!(run.iter().all(|v| v.cell == cell), "one cell's rows");
            for &o in &objs {
                let a = run.partition_point(|v| v.object < o);
                let b = a + run[a..].partition_point(|v| v.object == o);
                old.extend(lo + a..lo + b);
            }
        }
        old.sort_unstable();
        old
    }
}

fn row_key(v: &Violation) -> RowKey {
    (v.code, v.cell.0, v.object.0, v.path.0, v.param.0)
}

/// Drops the findings both sorted lists hold; returns what remains of each, in order.
fn cancel(
    gone: Vec<Violation>,
    new: Vec<Violation>,
    order: impl Fn(&Violation, &Violation) -> Ordering,
) -> Delta {
    let (mut g, mut n) = (Vec::new(), Vec::new());
    let (mut gi, mut ni) = (gone.into_iter().peekable(), new.into_iter().peekable());
    loop {
        match (gi.peek(), ni.peek()) {
            (Some(a), Some(b)) => match order(a, b) {
                Ordering::Equal => {
                    gi.next();
                    ni.next();
                }
                Ordering::Less => g.extend(gi.next()),
                Ordering::Greater => n.extend(ni.next()),
            },
            (Some(_), None) => g.extend(gi.next()),
            (None, Some(_)) => n.extend(ni.next()),
            (None, None) => break,
        }
    }
    (g, n)
}
