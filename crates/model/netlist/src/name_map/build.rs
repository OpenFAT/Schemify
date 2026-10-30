//! The name map's writer (netlist spec §7.1, §7.6.13): the emitted rows and nets of each
//! definition in, as the emitter writes them, one finished [`NameMap`] out.
//!
//! Contents:
//! - `pub(crate) struct Builder`: `Builder::new(elab, dialect, defs, rows, respellings) -> Builder`,
//!   `def(&mut self, elab: &Elab, def: u32, def_map: &[u32])`,
//!   `row(&mut self, elab: &Elab, row: usize, emitted: &str, child: u32)`,
//!   `finish(self) -> NameMap`

use super::{NameMap, Span};
use crate::{NONE, Respelling};
use schemify_elab::{Dialect, Elab, Name};

/// Builds a [`NameMap`] while the emitter writes, definition by definition.
pub(crate) struct Builder {
    map: NameMap,
    /// Elab name → net id (`NONE`: not seen).
    remap: Vec<u32>,
    /// Net id → the last definition that listed it.
    stamp: Vec<u32>,
    /// The definition being recorded.
    current: u32,
    /// Net respellings of the netlist (XSPICE bus bits, spec §7.2), sorted by name.
    pub(crate) respellings: Vec<Respelling>,
}

impl Builder {
    /// An empty map of `dialect` whose nets are spelled under `respellings` (sorted by
    /// [`Name`]); `defs` and `rows` are capacity hints.
    pub(crate) fn new(
        elab: &Elab,
        dialect: Dialect,
        defs: usize,
        rows: usize,
        respellings: Vec<Respelling>,
    ) -> Builder {
        assert!(
            respellings.windows(2).all(|w| w[0].net.0 < w[1].net.0),
            "respellings are sorted by name, one per net"
        );
        assert!(
            !respellings.iter().any(|r| r.net == Name::NONE),
            "an unconnected bit is never respelled"
        );
        let mut map = NameMap {
            dialect: Some(dialect),
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
            remap: vec![NONE; elab.names.len() as usize],
            stamp: Vec::new(),
            current: NONE,
            respellings,
        }
    }

    /// Starts elab definition `def` (map definitions are numbered in call order, which
    /// must be `def_map`'s numbering).
    pub(crate) fn def(&mut self, elab: &Elab, def: u32, def_map: &[u32]) {
        assert_eq!(def_map[def as usize] as usize, self.map.def_rows.len());
        self.current = self.map.def_rows.len() as u32;
        self.map.def_rows.push(self.map.row_local.len() as u32);
        self.map.def_ports.push(self.map.port.len() as u32);
        self.map.def_nets.push(self.map.net.len() as u32);
        let first_port = self.map.port.len();
        let ports = elab.ports(def);
        for &p in ports {
            let id = self.net(elab, p);
            self.map.port.push(id);
        }
        assert_eq!(self.map.def_ports.len(), self.map.def_rows.len());
        assert_eq!(
            self.map.port.len() - first_port,
            ports.len(),
            "one port net id per port"
        );
    }

    /// Records emitted row `row` of the current definition.
    pub(crate) fn row(&mut self, elab: &Elab, row: usize, emitted: &str, child: u32) {
        assert!(
            !self.map.def_rows.is_empty(),
            "a row belongs to a definition"
        );
        let local = span(&mut self.map.text, elab.names.get(elab.insts.name[row]));
        let emitted = span(&mut self.map.text, emitted);
        self.map.row_local.push(local);
        self.map.row_emitted.push(emitted);
        self.map.row_child.push(child);
        for &c in elab.conns(row) {
            let id = self.net(elab, c);
            self.map.conn.push(id);
        }
        self.map.row_conn.push(self.map.conn.len() as u32);
        assert_eq!(self.map.row_emitted.len(), self.map.row_local.len());
        assert_eq!(self.map.row_child.len(), self.map.row_local.len());
    }

    /// Closes the last definition's groups and returns the map.
    pub(crate) fn finish(mut self) -> NameMap {
        let map = &mut self.map;
        if !map.def_rows.is_empty() {
            map.def_rows.push(map.row_local.len() as u32);
            map.def_ports.push(map.port.len() as u32);
            map.def_nets.push(map.net.len() as u32);
        }
        assert_eq!(map.row_conn.len(), map.row_local.len() + 1);
        assert_eq!(map.net_name.len(), map.net_global.len());
        self.map
    }
}

// ---- private ----

/// Appends `s` to the text arena `text` and returns its span.
fn span(text: &mut String, s: &str) -> Span {
    let start = text.len();
    text.push_str(s);
    assert!(text.len() < u32::MAX as usize, "name map holds < 4 GiB");
    debug_assert_eq!(&text[start..], s);
    Span {
        start: start as u32,
        len: s.len() as u32,
    }
}

impl Builder {
    /// The net id of elab name `net` (interning it), listed in the current definition.
    fn net(&mut self, elab: &Elab, net: Name) -> u32 {
        if net == Name::NONE {
            return NONE;
        }
        let mut id = self.remap[net.0 as usize];
        if id == NONE {
            id = self.map.net_name.len() as u32;
            self.remap[net.0 as usize] = id;
            // Copied straight from elab's arena (no owned copy per new net: 10^6 in a flat
            // design); `respellings` and `map` are disjoint fields.
            let name = span(
                &mut self.map.text,
                crate::spelled(elab, &self.respellings, net),
            );
            self.map.net_name.push(name);
            let global = elab
                .globals
                .binary_search_by(|g| elab.names.cmp(*g, net))
                .is_ok();
            self.map.net_global.push(global);
            self.stamp.push(NONE);
        }
        assert!((id as usize) < self.map.net_name.len());
        assert!(self.current != NONE, "a net belongs to a definition");
        if self.stamp[id as usize] != self.current {
            self.stamp[id as usize] = self.current;
            self.map.net.push(id);
        }
        id
    }
}
