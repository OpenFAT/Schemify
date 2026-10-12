//! The name map (netlist spec §7.1): hierarchical paths (`/X1/M1`, `/X1/n1`) ↔ emitted
//! hierarchical names (`X1.XM1`, `X1.n1`), for instances and nets.
//!
//! Stored hierarchically, like elab's definitions: one group of instance rows and one
//! group of nets per emitted definition, so a design of 10^6 leaves in a few
//! definitions costs only its rows. A lookup walks the path from the top, one binary
//! search per level. The emitted-name indexes are sorted on the first reverse lookup.

use crate::{is_spice, push_name};
use schemify_elab::{Dialect, Elab, Name};
use std::cmp::Ordering;
use std::ops::Range;
use std::sync::OnceLock;

use crate::NONE;

/// A `(start, len)` range of [`NameMap`]'s text arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Span(u32, u32);

/// Paths ↔ emitted names of one [`crate::Netlist`].
///
/// A path is `/`-joined local instance names from the top (`/X1/X3`); a net path adds
/// the net's name in that instance's definition (`/X1/n1`, top-level nets `/in`).
/// Emitted names join the emitted instance names with `.` (`X1.XM1`, `X1.n1`). A net
/// that is a port of its definition is the net it connects to one level up (ngspice
/// has no `x1.a` node for port `a`), and a global net is its bare name at every depth.
///
/// Reverse lookups (emitted → path) compare ASCII case-insensitively for the SPICE
/// dialects (ngspice lowercases names in its output) and take net names with or
/// without the dialect's escapes.
#[derive(Clone, Debug, Default)]
pub struct NameMap {
    dialect: Option<Dialect>,
    text: String,
    /// CSR offsets (`defs + 1`) into the row columns; definition 0 is the top.
    def_rows: Vec<u32>,
    /// CSR offsets (`defs + 1`) into [`NameMap::port`].
    def_ports: Vec<u32>,
    /// CSR offsets (`defs + 1`) into [`NameMap::net`].
    def_nets: Vec<u32>,
    /// Port net ids in port order.
    port: Vec<u32>,
    /// Distinct net ids of each definition.
    net: Vec<u32>,
    /// Instance rows, per definition in elab's local-name order.
    row_local: Vec<Span>,
    row_emitted: Vec<Span>,
    /// Child definition, or `NONE` for a leaf.
    row_child: Vec<u32>,
    /// CSR offsets (`rows + 1`) into [`NameMap::conn`].
    row_conn: Vec<u32>,
    /// Net id per connection bit (`NONE`: unconnected).
    conn: Vec<u32>,
    /// Net names (one per distinct elab name).
    net_name: Vec<Span>,
    net_global: Vec<bool>,
    index: OnceLock<Index>,
}

/// Lookup orders, built on the first query that needs them.
#[derive(Clone, Debug)]
struct Index {
    /// Per definition (same CSR as the rows): rows by emitted name.
    rows: Vec<u32>,
    /// Per definition (same CSR as the nets): net ids by name.
    nets: Vec<u32>,
    /// Global net ids by name.
    globals: Vec<u32>,
}

impl NameMap {
    /// Returns the emitted hierarchical name of the instance at `path` (`/X1/M1` →
    /// `X1.XM1`); `None` when no emitted instance has that path (ignored instances and
    /// rows whose template failed have none).
    ///
    /// Cost: one binary search per path level.
    pub fn inst_name(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix('/').filter(|r| !r.is_empty())?;
        let (mut d, mut out) = (0u32, String::new());
        for seg in rest.split('/') {
            let r = self.find_local(d, seg)?;
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(self.s(self.row_emitted[r]));
            d = self.row_child[r];
        }
        assert!(!out.is_empty());
        Some(out)
    }

    /// Returns the path of the instance whose emitted hierarchical name is `name`
    /// (`x1.xm1` → `/X1/M1`).
    ///
    /// Cost: the first reverse lookup sorts every definition's rows and nets by name;
    /// then a binary search per level, retried at each `.` when names contain dots.
    pub fn inst_path(&self, name: &str) -> Option<String> {
        let mut path = String::new();
        let found = self.walk_inst(0, name, &mut path, 0);
        assert!(!found || path.starts_with('/'));
        found.then_some(path)
    }

    /// Returns the emitted hierarchical name of the net at `path` (`/X1/n1` → `X1.n1`):
    /// a port resolves to the net it connects to in the parent, a global to its bare
    /// name. `None` when the path names no emitted instance or no net of its
    /// definition.
    pub fn net_name(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix('/').filter(|r| !r.is_empty())?;
        let (insts, net) = match rest.rsplit_once('/') {
            Some((a, b)) => (Some(a), b),
            None => (None, rest),
        };
        let mut chain: Vec<(u32, usize)> = Vec::new();
        let mut d = 0u32;
        for seg in insts.into_iter().flat_map(|p| p.split('/')) {
            let r = self.find_local(d, seg)?;
            chain.push((d, r));
            d = self.row_child[r];
        }
        let mut id = self.find_net(d, net)?;
        while !self.net_global[id as usize] {
            let Some(j) = self.ports(d).iter().position(|p| *p == id) else {
                break;
            };
            let Some((parent, r)) = chain.pop() else {
                break; // a port of the top is a top-level net
            };
            id = *self.conn.get(self.row_conn[r] as usize + j)?;
            if id == NONE {
                return None;
            }
            d = parent;
        }
        let mut out = String::new();
        if !self.net_global[id as usize] {
            for &(_, r) in &chain {
                out.push_str(self.s(self.row_emitted[r]));
                out.push('.');
            }
        }
        push_name(&mut out, self.s(self.net_name[id as usize]), self.dialect?);
        Some(out)
    }

    /// Returns the path of the net whose emitted hierarchical name is `name`
    /// (`x1.n1` → `/X1/n1`; a global's bare name → `/<name>`). Cost as
    /// [`NameMap::inst_path`].
    pub fn net_path(&self, name: &str) -> Option<String> {
        let globals = &self.index().globals;
        if let Some(id) = self.find_spelled(globals, &self.net_name, &self.unescape(name)) {
            return Some(format!("/{}", self.s(self.net_name[id as usize])));
        }
        let mut path = String::new();
        let found = self.walk_net(0, name, &mut path, 0);
        assert!(!found || path.starts_with('/'));
        found.then_some(path)
    }

    /// Returns the emitted hierarchical names of the first `max` leaf instances (rows with
    /// no child definition: `MM1`, `X1.RR2`), depth first in each definition's row order.
    ///
    /// Cost: O(`max` + the definitions walked); the hierarchy is walked, never flattened in
    /// memory, so a design of 10^6 leaves costs `max` names.
    pub fn leaves(&self, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        if self.def_rows.len() >= 2 {
            let mut prefix = String::new();
            self.walk_leaves(0, &mut prefix, max, &mut out, 0);
        }
        assert!(out.len() <= max);
        out
    }

    // ---------------------------------------------------------------- internals

    fn walk_leaves(
        &self,
        d: u32,
        prefix: &mut String,
        max: usize,
        out: &mut Vec<String>,
        depth: u32,
    ) {
        assert!(
            depth <= schemify_elab::DEPTH_MAX,
            "deeper than any elaboration"
        );
        let len0 = prefix.len();
        for r in self.rows_of(d) {
            if out.len() >= max {
                return;
            }
            prefix.push_str(self.s(self.row_emitted[r]));
            match self.row_child[r] {
                NONE => out.push(prefix.clone()),
                c => {
                    prefix.push('.');
                    self.walk_leaves(c, prefix, max, out, depth + 1);
                }
            }
            prefix.truncate(len0);
        }
    }

    fn s(&self, sp: Span) -> &str {
        &self.text[sp.0 as usize..(sp.0 + sp.1) as usize]
    }

    /// Rows of definition `d`; `d` is a definition of this map.
    fn rows_of(&self, d: u32) -> Range<usize> {
        group(&self.def_rows, d).expect("a definition of the map")
    }

    /// Port net ids of definition `d`; `d` is a definition of this map.
    fn ports(&self, d: u32) -> &[u32] {
        &self.port[group(&self.def_ports, d).expect("a definition of the map")]
    }

    fn fold(&self) -> bool {
        self.dialect.is_some_and(is_spice)
    }

    fn key_cmp(&self, a: &str, b: &str) -> Ordering {
        if self.fold() {
            let lower = |c: u8| c.to_ascii_lowercase();
            a.bytes().map(lower).cmp(b.bytes().map(lower))
        } else {
            a.cmp(b)
        }
    }

    /// Removes the dialect's identifier escapes (spectre `\c`, verilog `\name `).
    fn unescape(&self, name: &str) -> String {
        match self.dialect {
            Some(Dialect::Spectre) => {
                let mut out = String::with_capacity(name.len());
                let mut esc = false;
                for c in name.chars() {
                    if c == '\\' && !esc {
                        esc = true;
                        continue;
                    }
                    esc = false;
                    out.push(c);
                }
                out
            }
            Some(Dialect::Verilog) => name
                .strip_prefix('\\')
                .map_or(name, |n| n.trim_end_matches(' '))
                .to_owned(),
            _ => name.to_owned(),
        }
    }

    /// The row of definition `d` (`NONE`: a leaf, none) whose local name is `seg`.
    fn find_local(&self, d: u32, seg: &str) -> Option<usize> {
        let rows = group(&self.def_rows, d)?;
        let k = self.row_local[rows.clone()]
            .binary_search_by(|sp| self.s(*sp).cmp(seg))
            .ok()?;
        assert!(rows.start + k < rows.end);
        Some(rows.start + k)
    }

    /// The row of definition `d` emitted as `name`.
    fn find_emitted(&self, d: u32, name: &str) -> Option<usize> {
        let rows = &self.index().rows[group(&self.def_rows, d)?];
        let r = self.find_spelled(rows, &self.row_emitted, name)?;
        Some(r as usize)
    }

    /// The net of definition `d` (`NONE`: a leaf, none) named `name` (unescaped).
    fn find_net(&self, d: u32, name: &str) -> Option<u32> {
        let nets = &self.index().nets[group(&self.def_nets, d)?];
        self.find_spelled(nets, &self.net_name, name)
    }

    /// The id among `ids` (sorted by [`NameMap::key_cmp`] of their `text`) whose text is
    /// `name`, preferring the exact spelling over a case-folded match.
    fn find_spelled(&self, ids: &[u32], text: &[Span], name: &str) -> Option<u32> {
        let key = |id: u32| self.key_cmp(self.s(text[id as usize]), name);
        let k0 = ids.partition_point(|&id| key(id).is_lt());
        let equal = ids[k0..].iter().copied().take_while(|&id| key(id).is_eq());
        let mut first = None;
        for id in equal {
            if self.s(text[id as usize]) == name {
                return Some(id);
            }
            first = first.or(Some(id));
        }
        first
    }
    /// Candidate `(head, rest)` splits of a dotted name: at each `.`, then whole.
    fn splits(name: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
        name.match_indices('.')
            .map(move |(i, _)| (&name[..i], Some(&name[i + 1..])))
            .chain(std::iter::once((name, None)))
    }

    fn walk_inst(&self, d: u32, name: &str, path: &mut String, depth: u32) -> bool {
        assert!(
            depth <= schemify_elab::DEPTH_MAX,
            "deeper than any elaboration"
        );
        let len0 = path.len();
        for (head, rest) in Self::splits(name) {
            let Some(r) = self.find_emitted(d, head) else {
                continue;
            };
            path.push('/');
            path.push_str(self.s(self.row_local[r]));
            match rest {
                None => return true,
                Some(rest) => {
                    let c = self.row_child[r];
                    if c != NONE && self.walk_inst(c, rest, path, depth + 1) {
                        return true;
                    }
                }
            }
            path.truncate(len0);
        }
        false
    }

    fn walk_net(&self, d: u32, name: &str, path: &mut String, depth: u32) -> bool {
        assert!(
            depth <= schemify_elab::DEPTH_MAX,
            "deeper than any elaboration"
        );
        let len0 = path.len();
        if let Some(id) = self.find_net(d, &self.unescape(name)) {
            path.push('/');
            path.push_str(self.s(self.net_name[id as usize]));
            return true;
        }
        for (head, rest) in Self::splits(name) {
            let (Some(rest), Some(r)) = (rest, self.find_emitted(d, head)) else {
                continue;
            };
            let c = self.row_child[r];
            if c == NONE {
                continue;
            }
            path.push('/');
            path.push_str(self.s(self.row_local[r]));
            if self.walk_net(c, rest, path, depth + 1) {
                return true;
            }
            path.truncate(len0);
        }
        false
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            let sort = |ids: &mut [u32], text: &[Span]| {
                ids.sort_by(|a, b| {
                    self.key_cmp(self.s(text[*a as usize]), self.s(text[*b as usize]))
                });
            };
            let mut rows: Vec<u32> = (0..self.row_local.len() as u32).collect();
            let mut nets = self.net.clone();
            for d in 0..self.def_rows.len().saturating_sub(1) as u32 {
                sort(&mut rows[self.rows_of(d)], &self.row_emitted);
                let group = group(&self.def_nets, d).expect("a definition of the map");
                sort(&mut nets[group], &self.net_name);
            }
            let mut globals: Vec<u32> = (0..self.net_name.len() as u32)
                .filter(|&id| self.net_global[id as usize])
                .collect();
            sort(&mut globals, &self.net_name);
            assert_eq!(rows.len(), self.row_local.len());
            assert_eq!(nets.len(), self.net.len());
            Index {
                rows,
                nets,
                globals,
            }
        })
    }
}

/// Group `d` of the CSR offsets `csr`; `None` past the last group (`NONE`, the child of
/// a leaf, included), so a walk below a leaf finds nothing.
fn group(csr: &[u32], d: u32) -> Option<Range<usize>> {
    match csr.get(d as usize..)? {
        [a, b, ..] => Some(*a as usize..*b as usize),
        _ => None,
    }
}

/// Appends `s` to the text arena `text` and returns its span.
fn span(text: &mut String, s: &str) -> Span {
    let start = text.len();
    text.push_str(s);
    assert!(text.len() < u32::MAX as usize, "name map holds < 4 GiB");
    Span(start as u32, s.len() as u32)
}

/// Builds a [`NameMap`] while the emitter writes, definition by definition.
pub(crate) struct Builder {
    map: NameMap,
    /// Elab name → net id (`NONE`: not seen).
    remap: Vec<u32>,
    /// Net id → the last definition that listed it.
    stamp: Vec<u32>,
    /// The definition being recorded.
    cur: u32,
    /// Net respellings of the netlist (XSPICE bus bits, spec §7.2), sorted by name.
    pub(crate) respell: Vec<(Name, String)>,
}

impl Builder {
    /// `defs` and `rows` are capacity hints.
    pub(crate) fn new(e: &Elab, d: Dialect, defs: usize, rows: usize) -> Builder {
        let mut map = NameMap {
            dialect: Some(d),
            ..NameMap::default()
        };
        map.def_rows.reserve(defs + 1);
        map.row_local.reserve(rows);
        map.row_emitted.reserve(rows);
        map.row_child.reserve(rows);
        map.row_conn.reserve(rows + 1);
        map.row_conn.push(0);
        // Local and emitted names per row plus net names: ~32 bytes a row.
        map.text.reserve(rows * 32);
        Builder {
            map,
            remap: vec![NONE; e.names.len() as usize],
            stamp: Vec::new(),
            cur: NONE,
            respell: Vec::new(),
        }
    }

    /// The net id of elab name `n` (interning it), listed in the current definition.
    fn net(&mut self, e: &Elab, n: Name) -> u32 {
        if n == Name::NONE {
            return NONE;
        }
        let mut id = self.remap[n.0 as usize];
        if id == NONE {
            id = self.map.net_name.len() as u32;
            self.remap[n.0 as usize] = id;
            // Copied straight from elab's arena (no owned copy per new net: 10^6 in a flat
            // design); `respell` and `map` are disjoint fields.
            let name = span(&mut self.map.text, crate::spelled(e, &self.respell, n));
            self.map.net_name.push(name);
            let global = e.globals.binary_search_by(|g| e.names.cmp(*g, n)).is_ok();
            self.map.net_global.push(global);
            self.stamp.push(NONE);
        }
        assert!((id as usize) < self.map.net_name.len() && self.cur != NONE);
        if self.stamp[id as usize] != self.cur {
            self.stamp[id as usize] = self.cur;
            self.map.net.push(id);
        }
        id
    }

    /// Starts elab definition `d` (map definitions are numbered in call order, which
    /// must be `def_map`'s numbering).
    pub(crate) fn def(&mut self, e: &Elab, d: u32, def_map: &[u32]) {
        assert_eq!(def_map[d as usize] as usize, self.map.def_rows.len());
        self.cur = self.map.def_rows.len() as u32;
        self.map.def_rows.push(self.map.row_local.len() as u32);
        self.map.def_ports.push(self.map.port.len() as u32);
        self.map.def_nets.push(self.map.net.len() as u32);
        for &p in e.ports(d) {
            let id = self.net(e, p);
            self.map.port.push(id);
        }
    }

    /// Records emitted row `r` of the current definition.
    pub(crate) fn row(&mut self, e: &Elab, r: usize, emitted: &str, child: u32) {
        assert!(
            !self.map.def_rows.is_empty(),
            "a row belongs to a definition"
        );
        let local = span(&mut self.map.text, e.names.get(e.insts.name[r]));
        let emitted = span(&mut self.map.text, emitted);
        self.map.row_local.push(local);
        self.map.row_emitted.push(emitted);
        self.map.row_child.push(child);
        for &c in e.conns(r) {
            let id = self.net(e, c);
            self.map.conn.push(id);
        }
        self.map.row_conn.push(self.map.conn.len() as u32);
    }

    pub(crate) fn finish(mut self) -> NameMap {
        let m = &mut self.map;
        if !m.def_rows.is_empty() {
            m.def_rows.push(m.row_local.len() as u32);
            m.def_ports.push(m.port.len() as u32);
            m.def_nets.push(m.net.len() as u32);
        }
        assert_eq!(m.row_conn.len(), m.row_local.len() + 1);
        assert_eq!(m.net_name.len(), m.net_global.len());
        self.map
    }
}
