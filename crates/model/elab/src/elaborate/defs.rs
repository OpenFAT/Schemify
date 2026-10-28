//! Definitions (netlist spec §5–§7): one definition's rows in, a shared or new
//! [`Defs`](super::Defs) row with its ports, inherited pins, `ignore short` merges and
//! instance rows out; then every definition's name (§6: `<cell>` or
//! `<cell>__<base32 FNV-1a>` of the param tuple).
//!
//! Contents:
//! - `Elaborator::{finish_def, name_defs}` (`pub(super)`)
//! - `pub(super) fn tuple(f: &Iface, vals: &[Val], names: &mut Names) -> (String, Vec<(Name, Name)>)`

use super::{DefRows, Elaborator, RowTmp, Target};
use crate::iface::Iface;
use crate::names::{Name, Names, fnv1a64, push_base32};
use crate::netexpr::{self, is_global};
use crate::nets::Ignore;
use crate::values::{Funcs, NONE, Val};
use std::collections::HashMap;

impl<'p, F: Funcs> Elaborator<'p, '_, F> {
    /// Returns the definition of cell `ci` with tuple `key` (its `pairs`) and the rows
    /// `body`: an existing one with the same children signature, else a new [`Defs`] row
    /// with its ports, inherited pins and instance rows sorted by name. With `sensitive`
    /// (a config `inst` rule reaches below this path) the tuple alone is not memoized.
    ///
    /// [`Defs`]: super::Defs
    pub(super) fn finish_def(
        &mut self,
        ci: u32,
        key: String,
        pairs: Vec<(Name, Name)>,
        sensitive: bool,
        body: DefRows,
        is_top: bool,
    ) -> u32 {
        let DefRows {
            rows,
            conn: mut bufs_conn,
            params: bufs_params,
            propagated,
        } = body;
        let names = &self.names;
        // By name text (stable): a radix sort of (prefix, row) keys, then runs of equal
        // prefixes (names sharing their first 8 bytes) by text. The rows stay put and are
        // read in that order (no sorted copy of 10^6 rows).
        let keys: Vec<(u64, u32)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.prefix, i as u32))
            .collect();
        let mut keys = crate::names::radix_sort(keys);
        let (mut lo, mut run) = (0, Vec::new());
        while lo < keys.len() {
            let hi = lo + keys[lo..].iter().take_while(|k| k.0 == keys[lo].0).count();
            if hi - lo > 1 {
                run.clear();
                run.extend(keys[lo..hi].iter().map(|k| (rows[k.1 as usize].name, k.1)));
                names.sort_by_text(&mut run);
                for (k, r) in keys[lo..hi].iter_mut().zip(&run) {
                    k.1 = r.1;
                }
            }
            lo = hi;
        }
        let order: Vec<u32> = keys.into_iter().map(|k| k.1).collect();
        debug_assert!(order.windows(2).all(|w| {
            let (a, b) = (&rows[w[0] as usize], &rows[w[1] as usize]);
            (a.prefix, names.get(a.name)) <= (b.prefix, names.get(b.name))
        }));
        // The children signature keys reuse of an identical definition; the top is never
        // instantiated, so it has nothing to be shared with (and the longest signature).
        let mut sig = String::new();
        for r in order.iter().map(|&i| &rows[i as usize]).filter(|_| !is_top) {
            sig.push_str(names.get(r.name));
            sig.push('=');
            let (tag, n) = match r.target {
                Target::Def(d) => ('d', d),
                Target::Leaf(l) => ('l', l),
            };
            sig.push(tag);
            crate::names::push_decimal(n, &mut sig);
            sig.push(',');
        }
        let full = (ci, key.clone(), sig);
        if let Some(&d) = self.by_key.get(&full) {
            if !sensitive {
                self.memo.insert((ci, key), d);
            }
            return d;
        }
        let d = self.defs.cell.len() as u32;
        for (name, text) in pairs {
            self.defs.param_name.push(name);
            self.defs.param_text.push(text);
        }
        // Ports: ordinary pins, declared inherited pins, propagated inherited pins.
        let f = &self.ifaces[ci as usize];
        let nets = self.nets[ci as usize].as_ref().expect("prepared");
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] == Name::NONE && f.pin_alias[p] == NONE {
                self.defs
                    .port_net
                    .extend(nets.port(p).iter().map(|&b| nets.bit_name[b as usize]));
            }
        }
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] != Name::NONE {
                self.defs
                    .port_net
                    .extend(nets.port(p).iter().map(|&b| nets.bit_name[b as usize]));
                self.defs.inh_pin.push(f.pin_name[p]);
                self.defs.inh_default.push(f.pin_inherit[p]);
            }
        }
        if !is_top {
            let extra: Vec<(Name, Name)> = propagated
                .into_iter()
                .filter(|(p, _)| {
                    !(0..f.pin_name.len())
                        .any(|q| f.pin_name[q] == *p && f.pin_inherit[q] != Name::NONE)
                })
                .collect();
            let mut order: Vec<(Name, u32)> =
                (0..extra.len()).map(|i| (extra[i].0, i as u32)).collect();
            self.names.sort_by_text(&mut order);
            let mut tmp = Vec::new();
            let mut buf = String::new();
            for (pin, default) in order.into_iter().map(|o| extra[o.1 as usize]) {
                tmp.clear();
                let text = self.names.get(pin).to_owned();
                let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
                let nets = self.nets[ci as usize].as_ref().expect("prepared");
                self.defs.port_net.extend(
                    tmp.iter()
                        .map(|&b| nets.bit(b).map_or(b, |n| nets.bit_name[n as usize])),
                );
                self.defs.inh_pin.push(pin);
                self.defs.inh_default.push(default);
            }
        }
        if rows.iter().any(|r| r.ignore == Ignore::Short) {
            let port0 = *self.defs.port_start.last().expect("seeded") as usize;
            short_nets(
                &rows,
                &mut bufs_conn,
                &mut self.defs.port_net[port0..],
                &self.globals,
                &self.names,
            );
        }
        let body = self.def_body(ci);
        self.defs.body.push(body);
        self.defs.cell.push(ci);
        self.push_def_code(ci);
        self.defs.name.push(Name::NONE);
        self.defs.key.push(key.clone());
        self.defs
            .param_start
            .push(self.defs.param_name.len() as u32);
        self.defs.port_start.push(self.defs.port_net.len() as u32);
        self.defs.inh_start.push(self.defs.inh_pin.len() as u32);
        let i = &mut self.insts;
        i.name.reserve(rows.len());
        i.id.reserve(rows.len());
        i.target.reserve(rows.len());
        i.ignore.reserve(rows.len());
        i.conn_start.reserve(rows.len());
        i.param_start.reserve(rows.len());
        i.conn.reserve(bufs_conn.len());
        i.param_name.reserve(bufs_params.len());
        i.param_text.reserve(bufs_params.len());
        i.param_val.reserve(bufs_params.len());
        i.param_flags.reserve(bufs_params.len());
        for r in order.iter().map(|&i| &rows[i as usize]) {
            self.insts.name.push(r.name);
            self.insts.id.push(r.id);
            self.insts.target.push(r.target);
            self.insts.ignore.push(r.ignore);
            let conn = r.conn.start as usize..r.conn.end as usize;
            self.insts.conn.extend_from_slice(&bufs_conn[conn]);
            self.insts.conn_start.push(self.insts.conn.len() as u32);
            for (name, text, val, flags) in
                &bufs_params[r.params.start as usize..r.params.end as usize]
            {
                self.insts.param_name.push(*name);
                self.insts.param_text.push(*text);
                self.insts.param_val.push(val.clone());
                self.insts.param_flags.push(*flags);
            }
            self.insts
                .param_start
                .push(self.insts.param_name.len() as u32);
        }
        self.defs.inst_start.push(self.insts.name.len() as u32);
        self.by_key.insert(full, d);
        if !sensitive {
            self.memo.insert((ci, key), d);
        }
        assert_eq!(self.defs.inst_start.len(), self.defs.cell.len() + 1);
        d
    }

    /// Names every definition (spec §6): `<cell>` for the default tuple, else
    /// `<cell>__<6 base32 of FNV-1a 64 of the tuple text>`. Definitions sharing a cell
    /// and tuple but differing in children (config `inst` rules) hash the tuple text
    /// plus `|` and their children's `name=target` list. Cells sharing a stem get
    /// `<stem>__<6 base32 of FNV-1a 64 of the display name>` as their base.
    pub(super) fn name_defs(&mut self) {
        let nd = self.defs.cell.len();
        // Base names.
        let mut cells_used: Vec<u32> = self.defs.cell.clone();
        cells_used.sort_unstable();
        cells_used.dedup();
        let mut base: HashMap<u32, String> = HashMap::new();
        for &ci in &cells_used {
            let stem = self.names.get(self.ifaces[ci as usize].stem);
            let clash = cells_used
                .iter()
                .any(|&o| o != ci && self.names.get(self.ifaces[o as usize].stem) == stem);
            let mut b = stem.to_owned();
            if clash {
                b.push_str("__");
                push_base32(
                    fnv1a64(self.names.get(self.ifaces[ci as usize].cell).as_bytes()),
                    6,
                    &mut b,
                );
            }
            base.insert(ci, b);
        }
        // Group sizes per (cell, tuple).
        let mut group: HashMap<(u32, &str), u32> = HashMap::new();
        for d in 0..nd {
            *group
                .entry((self.defs.cell[d], self.defs.key[d].as_str()))
                .or_insert(0) += 1;
        }
        let extended: Vec<bool> = (0..nd)
            .map(|d| group[&(self.defs.cell[d], self.defs.key[d].as_str())] > 1)
            .collect();
        drop(group);
        let mut texts: Vec<String> = vec![String::new(); nd];
        for d in 0..nd {
            let ci = self.defs.cell[d];
            let _ = self.defaults(ci);
            let is_default = !extended[d]
                && self.cells[ci as usize].default_key.as_deref() == Some(&self.defs.key[d]);
            let mut name = base[&ci].clone();
            if !is_default {
                let mut text = self.defs.key[d].clone();
                if extended[d] {
                    text.push('|');
                    for r in self.defs.inst_start[d] as usize..self.defs.inst_start[d + 1] as usize
                    {
                        text.push_str(self.names.get(self.insts.name[r]));
                        text.push('=');
                        match self.insts.target[r] {
                            Target::Def(c) => {
                                text.push_str(self.names.get(self.defs.name[c as usize]))
                            }
                            Target::Leaf(l) => {
                                let f = &self.ifaces[self.impls.cell[l as usize] as usize];
                                text.push_str(self.names.get(f.cell));
                                text.push(':');
                                text.push_str(
                                    self.names
                                        .get(f.impl_name[self.impls.index[l as usize] as usize]),
                                );
                            }
                        }
                        text.push(',');
                    }
                }
                name.push_str("__");
                push_base32(fnv1a64(text.as_bytes()), 6, &mut name);
                texts[d] = text;
            }
            self.defs.name[d] = self.names.intern(&name);
        }
        // 30-bit hash collisions between different tuples: use all 13 digits.
        let mut order: Vec<(Name, u32)> = (0..nd).map(|d| (self.defs.name[d], d as u32)).collect();
        self.names.sort_by_text(&mut order);
        for w in 0..order.len().saturating_sub(1) {
            let (a, b) = (order[w].1 as usize, order[w + 1].1 as usize);
            if self.defs.name[a] == self.defs.name[b] {
                for d in [a, b] {
                    let ci = self.defs.cell[d];
                    let mut name = base[&ci].clone();
                    name.push_str("__");
                    push_base32(fnv1a64(texts[d].as_bytes()), 13, &mut name);
                    self.defs.name[d] = self.names.intern(&name);
                }
            }
        }
    }
}

/// The canonical text of a param tuple (spec §6): non-computed params as `k=v`, sorted
/// bytewise by name, joined by single spaces; string values quoted with expression
/// string escapes so the text is unambiguous. Also returns the `(name, value text)`
/// pairs in that order.
pub(super) fn tuple(f: &Iface, vals: &[Val], names: &mut Names) -> (String, Vec<(Name, Name)>) {
    let mut pairs: Vec<(Name, String, String)> = Vec::new();
    for k in 0..f.param_name.len().min(vals.len()) {
        if f.param_computed[k] {
            continue;
        }
        let (mut quoted, mut plain) = (String::new(), String::new());
        match &vals[k] {
            v if v.is_err() => plain.push('?'),
            v => v.write_text(&mut plain),
        }
        if let Val::Str(_) = &vals[k] {
            crate::values::cached_text(&vals[k], &mut quoted); // expression string escapes
        } else {
            quoted.clone_from(&plain);
        }
        pairs.push((f.param_name[k], quoted, plain));
    }
    pairs.sort_by(|a, b| names.get(a.0).cmp(names.get(b.0)));
    let mut key = String::new();
    for (i, (k, v, _)) in pairs.iter().enumerate() {
        if i > 0 {
            key.push(' ');
        }
        key.push_str(names.get(*k));
        key.push('=');
        key.push_str(v);
    }
    let out = pairs
        .into_iter()
        .map(|(k, _, plain)| (k, names.intern(&plain)))
        .collect();
    (key, out)
}

// ---- private ----

/// `ignore short` rows of one definition (spec §7.1): all nets of a shorted row become
/// one net, renamed everywhere in the definition to the set's best name — global,
/// then a port of the definition, then bytewise smallest. (An instance attr was
/// already merged by `connect`; this is what an impl-level `ignore short` needs.)
fn short_nets(
    rows: &[RowTmp],
    conns: &mut [Name],
    ports: &mut [Name],
    globals: &[Name],
    names: &Names,
) {
    let range = |r: &RowTmp| r.conn.start as usize..r.conn.end as usize;
    let mut ids: Vec<Name> = rows
        .iter()
        .flat_map(|r| conns[range(r)].iter().copied())
        .chain(ports.iter().copied())
        .filter(|n| *n != Name::NONE)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let idx = |n: Name| ids.binary_search(&n).expect("collected above") as u32;
    let mut parent: Vec<u32> = (0..ids.len() as u32).collect();
    for r in rows.iter().filter(|r| r.ignore == Ignore::Short) {
        let mut first = NONE;
        for &n in conns[range(r)].iter().filter(|n| **n != Name::NONE) {
            if first == NONE {
                first = idx(n);
            } else {
                crate::nets::union(&mut parent, first, idx(n));
            }
        }
    }
    let rank = |n: Name| -> u8 {
        if is_global(names.get(n)) || globals.binary_search(&n).is_ok() {
            0
        } else if ports.contains(&n) {
            1
        } else {
            2
        }
    };
    let mut best: Vec<Name> = ids.clone();
    for i in 0..ids.len() {
        let r = crate::nets::find(&mut parent, i as u32) as usize;
        let (a, b) = (ids[i], best[r]);
        if (rank(a), names.get(a)) < (rank(b), names.get(b)) {
            best[r] = a;
        }
    }
    let mut rename = |n: &mut Name| {
        if *n != Name::NONE {
            *n = best[crate::nets::find(&mut parent, idx(*n)) as usize];
        }
    };
    for r in rows {
        conns[range(r)].iter_mut().for_each(&mut rename);
    }
    ports.iter_mut().for_each(&mut rename);
}
