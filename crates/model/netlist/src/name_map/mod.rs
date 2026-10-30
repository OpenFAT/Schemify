//! The name map (netlist spec §7.1, §7.6.13): the emitted rows and nets of each
//! definition in, hierarchical paths (`/X1/M1`, `/X1/n1`) ↔ emitted hierarchical names
//! (`X1.XM1`, `X1.n1`) out, for instances and nets.
//!
//! Stored hierarchically, like elab's definitions: one group of instance rows and one
//! group of nets per emitted definition, so a design of 10^6 leaves in a few
//! definitions costs only its rows. A lookup walks the path from the top, one binary
//! search per level. The emitted-name indexes are sorted on the first reverse lookup.
//!
//! Contents:
//! - `pub struct NameMap`: `inst_name(&self, path: &str) -> Option<String>`,
//!   `spice_device(&self, path: &str) -> Result<String, String>`,
//!   `inst_path(&self, name: &str) -> Option<String>`,
//!   `net_name(&self, path: &str) -> Option<String>`,
//!   `net_path(&self, name: &str) -> Option<String>`,
//!   `leaves(&self, max: usize) -> Vec<String>`
//! - re-exported: `Builder` (build.rs), which writes the map

mod build;

pub(crate) use build::Builder;

use crate::NONE;
use schemify_elab::{DEPTH_MAX, Dialect};
use std::cmp::Ordering;
use std::iter;
use std::ops::Range;
use std::sync::OnceLock;

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
///
/// Every lookup asserts the map's own invariants (CSR offsets in step, a walk no deeper
/// than [`DEPTH_MAX`]); they hold for every map [`crate::emit`] builds, so a panic in a
/// lookup is a bug in this crate, never a bad argument.
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

/// A range of [`NameMap`]'s text arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Span {
    start: u32,
    len: u32,
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
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    #[must_use]
    pub fn inst_name(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix('/').filter(|r| !r.is_empty())?;
        let (mut def, mut out) = (0u32, String::new());
        for segment in rest.split('/') {
            let row = self.find_local(def, segment)?;
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(self.span_text(self.row_emitted[row]));
            def = self.row_child[row];
        }
        assert!(!out.is_empty());
        assert!(
            def == NONE || (def as usize) + 1 < self.def_rows.len(),
            "a row's child is a definition of the map"
        );
        Some(out)
    }

    /// Returns the ngspice name of the device at instance path `path` (`op()`, run spec
    /// §4): the emitted names of the path's instances, lowercased and joined with `.`,
    /// with the device's type letter and a dot in front inside a subcircuit (`/M1` →
    /// `m1`, `/X1/M2` → `m.x1.mm2`). Segments after a leaf name devices inside the leaf's
    /// own subcircuit, which Schemify does not elaborate (a PDK device model), verbatim:
    /// `/M1/msky130_fd_pr__nfet_01v8` → `m.xm1.msky130_fd_pr__nfet_01v8` when `M1` is
    /// emitted `XM1`. The leading `/` is optional.
    ///
    /// # Errors
    ///
    /// The message naming the problem: an empty path or segment, a segment no emitted
    /// instance has (ignored and failed rows have none), or a path that ends at a
    /// subcircuit instance rather than a device.
    ///
    /// Cost: one binary search per schematic level.
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    pub fn spice_device(&self, path: &str) -> Result<String, String> {
        let rest = path.strip_prefix('/').unwrap_or(path);
        if rest.is_empty() || rest.split('/').any(str::is_empty) {
            return Err(format!("`{path}` is not an instance path"));
        }
        let (mut def, mut out, mut last) = (0u32, String::new(), "");
        for segment in rest.split('/') {
            if !out.is_empty() {
                out.push('.');
            }
            if def == NONE {
                out.push_str(segment); // inside a leaf's own subcircuit
                last = segment;
                continue;
            }
            let Some(row) = self.find_local(def, segment) else {
                return Err(format!("no instance `{segment}` on the path `{path}`"));
            };
            last = self.span_text(self.row_emitted[row]);
            out.push_str(last);
            def = self.row_child[row];
        }
        if def != NONE {
            assert!(
                (def as usize) + 1 < self.def_rows.len(),
                "a row's child is a definition of the map"
            );
            return Err(format!("`{path}` is a subcircuit instance, not a device"));
        }
        out.make_ascii_lowercase();
        let nested = out.contains('.');
        let letter = last.chars().next().map(|c| c.to_ascii_lowercase());
        let device = match letter {
            Some(c) if nested => format!("{c}.{out}"),
            _ => out,
        };
        assert!(!device.is_empty());
        Ok(device)
    }

    /// Returns the path of the instance whose emitted hierarchical name is `name`
    /// (`x1.xm1` → `/X1/M1`).
    ///
    /// Cost: the first reverse lookup sorts every definition's rows and nets by name;
    /// then a binary search per level, retried at each `.` when names contain dots.
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    #[must_use]
    pub fn inst_path(&self, name: &str) -> Option<String> {
        let mut path = String::new();
        let found = self.walk_inst(0, name, &mut path, 0);
        assert!(!found || path.starts_with('/'));
        assert!(found || path.is_empty(), "a failed walk leaves no path");
        found.then_some(path)
    }

    /// Returns the emitted hierarchical name of the net at `path` (`/X1/n1` → `X1.n1`):
    /// a port resolves to the net it connects to in the parent, a global to its bare
    /// name. `None` when the path names no emitted instance or no net of its
    /// definition.
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    #[must_use]
    pub fn net_name(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix('/').filter(|r| !r.is_empty())?;
        let (insts, net) = match rest.rsplit_once('/') {
            Some((insts, net)) => (Some(insts), net),
            None => (None, rest),
        };
        // (parent definition, row) per instance on the path.
        let mut chain: Vec<(u32, usize)> = Vec::new();
        let mut def = 0u32;
        let segments = insts.into_iter().flat_map(|p| p.split('/'));
        for segment in segments {
            let row = self.find_local(def, segment)?;
            chain.push((def, row));
            def = self.row_child[row];
        }
        assert!(
            chain.len() <= DEPTH_MAX as usize,
            "a path of emitted instances is no deeper than the elaboration"
        );
        let mut id = self.find_net(def, net)?;
        while !self.net_global[id as usize] {
            let Some(j) = self.ports(def).iter().position(|p| *p == id) else {
                break;
            };
            let Some((parent, row)) = chain.pop() else {
                break; // a port of the top is a top-level net
            };
            id = *self.conn.get(self.row_conn[row] as usize + j)?;
            if id == NONE {
                return None;
            }
            def = parent;
        }
        assert!(
            (id as usize) < self.net_name.len(),
            "a connection names a net of the map"
        );
        let mut out = String::new();
        if !self.net_global[id as usize] {
            for &(_, row) in &chain {
                out.push_str(self.span_text(self.row_emitted[row]));
                out.push('.');
            }
        }
        crate::push_name(
            &mut out,
            self.span_text(self.net_name[id as usize]),
            self.dialect?,
        );
        Some(out)
    }

    /// Returns the path of the net whose emitted hierarchical name is `name`
    /// (`x1.n1` → `/X1/n1`; a global's bare name → `/<name>`). Cost as
    /// [`NameMap::inst_path`].
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    #[must_use]
    pub fn net_path(&self, name: &str) -> Option<String> {
        let globals = &self.index().globals;
        if let Some(id) = self.find_spelled(globals, &self.net_name, &self.unescape(name)) {
            return Some(format!("/{}", self.span_text(self.net_name[id as usize])));
        }
        let mut path = String::new();
        let found = self.walk_net(0, name, &mut path, 0);
        assert!(!found || path.starts_with('/'));
        assert!(found || path.is_empty(), "a failed walk leaves no path");
        found.then_some(path)
    }

    /// Returns the emitted hierarchical names of the first `max` leaf instances (rows with
    /// no child definition: `MM1`, `X1.RR2`), depth first in each definition's row order.
    ///
    /// Cost: O(`max` + the definitions walked); the hierarchy is walked, never flattened in
    /// memory, so a design of 10^6 leaves costs `max` names.
    ///
    /// # Panics
    ///
    /// Only on a broken map invariant (see [`NameMap`]).
    #[must_use]
    pub fn leaves(&self, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        if self.def_rows.len() >= 2 {
            let mut prefix = String::new();
            self.walk_leaves(0, &mut prefix, max, &mut out, 0);
            assert!(prefix.is_empty(), "the walk restores its prefix");
        }
        assert!(out.len() <= max);
        out
    }
}

// ---- private ----

/// Group `def` of the CSR offsets `csr`; `None` past the last group (`NONE`, the child of
/// a leaf, included), so a walk below a leaf finds nothing.
fn group(csr: &[u32], def: u32) -> Option<Range<usize>> {
    let group = match csr.get(def as usize..)? {
        [a, b, ..] => *a as usize..*b as usize,
        _ => return None,
    };
    debug_assert!(group.start <= group.end, "CSR offsets ascend");
    Some(group)
}

impl NameMap {
    fn walk_leaves(
        &self,
        def: u32,
        prefix: &mut String,
        max: usize,
        out: &mut Vec<String>,
        depth: u32,
    ) {
        assert!(depth <= DEPTH_MAX, "deeper than any elaboration");
        let start_len = prefix.len();
        for row in self.rows_of(def) {
            if out.len() >= max {
                return;
            }
            prefix.push_str(self.span_text(self.row_emitted[row]));
            match self.row_child[row] {
                NONE => out.push(prefix.clone()),
                child => {
                    prefix.push('.');
                    self.walk_leaves(child, prefix, max, out, depth + 1);
                }
            }
            prefix.truncate(start_len);
        }
        assert!(out.len() <= max);
    }

    fn span_text(&self, span: Span) -> &str {
        &self.text[span.start as usize..(span.start + span.len) as usize]
    }

    /// Rows of definition `def`; `def` is a definition of this map.
    fn rows_of(&self, def: u32) -> Range<usize> {
        group(&self.def_rows, def).expect("a definition of the map")
    }

    /// Port net ids of definition `def`; `def` is a definition of this map.
    fn ports(&self, def: u32) -> &[u32] {
        &self.port[group(&self.def_ports, def).expect("a definition of the map")]
    }

    fn folds_case(&self) -> bool {
        self.dialect.is_some_and(crate::is_spice)
    }

    fn key_cmp(&self, a: &str, b: &str) -> Ordering {
        let order = if self.folds_case() {
            let lower = |c: u8| c.to_ascii_lowercase();
            a.bytes().map(lower).cmp(b.bytes().map(lower))
        } else {
            a.cmp(b)
        };
        debug_assert!(order.is_ne() || a.eq_ignore_ascii_case(b));
        order
    }

    /// Removes the dialect's identifier escapes (spectre `\c`, verilog `\name `).
    fn unescape(&self, name: &str) -> String {
        let out = match self.dialect {
            Some(Dialect::Spectre) => {
                let mut out = String::with_capacity(name.len());
                let mut escaped = false;
                for c in name.chars() {
                    if c == '\\' && !escaped {
                        escaped = true;
                        continue;
                    }
                    escaped = false;
                    out.push(c);
                }
                out
            }
            Some(Dialect::Verilog) => name
                .strip_prefix('\\')
                .map_or(name, |n| n.trim_end_matches(' '))
                .to_owned(),
            _ => name.to_owned(),
        };
        assert!(
            out.len() <= name.len(),
            "unescaping only removes characters"
        );
        out
    }

    /// The row of definition `def` (`NONE`: a leaf, none) whose local name is `segment`.
    fn find_local(&self, def: u32, segment: &str) -> Option<usize> {
        let rows = group(&self.def_rows, def)?;
        let k = self.row_local[rows.clone()]
            .binary_search_by(|span| self.span_text(*span).cmp(segment))
            .ok()?;
        assert!(rows.start + k < rows.end);
        debug_assert_eq!(self.span_text(self.row_local[rows.start + k]), segment);
        Some(rows.start + k)
    }

    /// The row of definition `def` emitted as `name`.
    fn find_emitted(&self, def: u32, name: &str) -> Option<usize> {
        let group = group(&self.def_rows, def)?;
        let row = self.find_spelled(&self.index().rows[group.clone()], &self.row_emitted, name)?;
        // The index sorts each definition's rows in place, never across groups.
        assert!(
            group.contains(&(row as usize)),
            "an index entry stays in its definition's group"
        );
        Some(row as usize)
    }

    /// The net of definition `def` (`NONE`: a leaf, none) named `name` (unescaped).
    fn find_net(&self, def: u32, name: &str) -> Option<u32> {
        let nets = &self.index().nets[group(&self.def_nets, def)?];
        let id = self.find_spelled(nets, &self.net_name, name)?;
        assert!((id as usize) < self.net_name.len());
        Some(id)
    }

    /// The id among `ids` (sorted by [`NameMap::key_cmp`] of their `text`) whose text is
    /// `name`, preferring the exact spelling over a case-folded match.
    fn find_spelled(&self, ids: &[u32], text: &[Span], name: &str) -> Option<u32> {
        let key = |id: u32| self.key_cmp(self.span_text(text[id as usize]), name);
        let first_equal = ids.partition_point(|&id| key(id).is_lt());
        let equal = ids[first_equal..]
            .iter()
            .copied()
            .take_while(|&id| key(id).is_eq());
        let mut first = None;
        for id in equal {
            if self.span_text(text[id as usize]) == name {
                return Some(id);
            }
            first = first.or(Some(id));
        }
        debug_assert!(
            first.is_none_or(|id| key(id).is_eq()),
            "a case-folded match folds equal"
        );
        first
    }

    /// Candidate `(head, rest)` splits of a dotted name: at each `.`, then whole.
    fn splits(name: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
        name.match_indices('.')
            .map(move |(i, _)| (&name[..i], Some(&name[i + 1..])))
            .chain(iter::once((name, None)))
    }

    fn walk_inst(&self, def: u32, name: &str, path: &mut String, depth: u32) -> bool {
        assert!(depth <= DEPTH_MAX, "deeper than any elaboration");
        let start_len = path.len();
        for (head, rest) in Self::splits(name) {
            let Some(row) = self.find_emitted(def, head) else {
                continue;
            };
            path.push('/');
            path.push_str(self.span_text(self.row_local[row]));
            match rest {
                None => return true,
                Some(rest) => {
                    let child = self.row_child[row];
                    if child != NONE && self.walk_inst(child, rest, path, depth + 1) {
                        return true;
                    }
                }
            }
            path.truncate(start_len);
        }
        assert_eq!(
            path.len(),
            start_len,
            "a failed walk leaves the path as it was"
        );
        false
    }

    fn walk_net(&self, def: u32, name: &str, path: &mut String, depth: u32) -> bool {
        assert!(depth <= DEPTH_MAX, "deeper than any elaboration");
        let start_len = path.len();
        if let Some(id) = self.find_net(def, &self.unescape(name)) {
            path.push('/');
            path.push_str(self.span_text(self.net_name[id as usize]));
            return true;
        }
        for (head, rest) in Self::splits(name) {
            let (Some(rest), Some(row)) = (rest, self.find_emitted(def, head)) else {
                continue;
            };
            let child = self.row_child[row];
            if child == NONE {
                continue;
            }
            path.push('/');
            path.push_str(self.span_text(self.row_local[row]));
            if self.walk_net(child, rest, path, depth + 1) {
                return true;
            }
            path.truncate(start_len);
        }
        assert_eq!(
            path.len(),
            start_len,
            "a failed walk leaves the path as it was"
        );
        false
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            let sort = |ids: &mut [u32], text: &[Span]| {
                ids.sort_by(|a, b| {
                    self.key_cmp(
                        self.span_text(text[*a as usize]),
                        self.span_text(text[*b as usize]),
                    )
                });
            };
            let mut rows: Vec<u32> = (0..self.row_local.len() as u32).collect();
            let mut nets = self.net.clone();
            for def in 0..self.def_rows.len().saturating_sub(1) as u32 {
                sort(&mut rows[self.rows_of(def)], &self.row_emitted);
                let group = group(&self.def_nets, def).expect("a definition of the map");
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
