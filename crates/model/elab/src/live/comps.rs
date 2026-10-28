//! Components and bits of live connectivity (netlist spec §3–§5): a dirty component in,
//! its findings, pin counts and bits taken out; a region piece in, a component built with
//! `connect`'s rules (naming, widths, bus slicing, `W004`, `E011`) out. Handles are
//! recycled through free lists, so untouched components keep theirs.
//!
//! Contents:
//! - `Live::make_dirty`, `Live::alloc_comp`, `Live::build_comp`, `Live::bit_rows`
//!   (`pub(super)`)

use super::{Ctx, INSTS, Kind, Live};
use crate::iface::{Domain, Iface};
use crate::names::{Name, Names, push_decimal};
use crate::netcheck::bit_findings;
use crate::netexpr;
use crate::nets::{
    CellNets, GLOBAL, Ignore, LABEL, NO_NET, NONE_PIN, PORT, glob, id_cmp, slicing, write_key,
};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use std::cmp::Ordering;

impl Live {
    /// Takes component `c` out (once per edit): its findings leave, its pins' counts leave
    /// its bits, its bits are released (a named one when no slot carries its name any
    /// more), its live nodes join the region and its handle is freed.
    pub(super) fn make_dirty(&mut self, c: u32, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) {
        let ci = c as usize;
        if self.cmark[ci] == self.epoch {
            return;
        }
        assert!(self.head[ci] != NONE, "a live component");
        self.cmark[ci] = self.epoch;
        self.gone.append(&mut self.found[ci]);
        let (start, width) = (cn.net_bit_start[ci] as usize, usize::from(cn.net_width[ci]));
        for k in start..start + width {
            self.touch(cn.net_bits[k], cx, cn, names);
        }
        let head = self.head[ci];
        let mut v = head;
        loop {
            if self.kind[v as usize] == Kind::Pin {
                self.pin(v, c, false, cx, cn, names);
            }
            if self.comp[v as usize] == c {
                self.enter(v);
            }
            v = self.next[v as usize];
            if v == head {
                break;
            }
        }
        for k in start..start + width {
            let b = cn.net_bits[k];
            if self.named[ci] {
                self.slots[b as usize] -= 1;
                if self.slots[b as usize] > 0 {
                    continue;
                }
            }
            self.free_bit(b, cn);
        }
        self.live_bits -= width;
        cn.net_name[ci] = self.empty;
        cn.net_width[ci] = 0;
        cn.net_global[ci] = false;
        cn.net_pins[ci] = 0;
        cn.net_bit_start[ci] = 0;
        self.head[ci] = NONE;
        self.free_comps.push(c);
    }

    pub(super) fn alloc_comp(&mut self, cn: &mut CellNets) -> u32 {
        if let Some(c) = self.free_comps.pop() {
            return c;
        }
        let c = cn.net_name.len() as u32;
        cn.net_name.push(self.empty);
        cn.net_width.push(0);
        cn.net_global.push(false);
        cn.net_pins.push(0);
        cn.net_bit_start.push(0);
        self.head.push(NONE);
        self.named.push(false);
        self.found.push(Vec::new());
        self.cmark.push(self.epoch);
        assert_eq!(self.head.len(), cn.net_name.len());
        c
    }

    /// Builds component `c` from `members` with `connect`'s rules: counts, naming key,
    /// sources and their findings, width, bits, pins, `W004` and the row outputs. Returns
    /// `false` when two names short (`E003`): bits would merge by name, which only a full
    /// build handles.
    pub(super) fn build_comp(
        &mut self,
        c: u32,
        members: &[u32],
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &mut Names,
    ) -> bool {
        let (doc, ci) = (cx.doc, c as usize);
        let s = doc.strings();
        let (wires, labels, ports, noconns, inst) = (
            doc.wires(),
            doc.labels(),
            doc.ports(),
            doc.noconns(),
            doc.instances(),
        );
        assert!(!members.is_empty() && self.head[ci] == NONE);
        for (k, &v) in members.iter().enumerate() {
            self.next[v as usize] = members[(k + 1) % members.len()];
            self.comp[v as usize] = c;
        }
        self.head[ci] = members[0];
        let finding =
            |code, object, param, detail| Violation::new(code, cx.own.cell, object, param, detail);
        let mut found: Vec<Violation> = Vec::new();

        // ---- counts, naming key, sources (class, text, object, bits range)
        let (mut pins, mut pinw) = (0u32, 1usize);
        let (mut minid, mut minpin) = (ObjId::NONE, (ObjId::NONE, 0u32));
        let mut srcs: Vec<(u8, Name, ObjId, usize, usize)> = Vec::new();
        let (mut bits, mut buf): (Vec<Name>, String) = (Vec::new(), String::new());
        for &v in members {
            let r = self.row[v as usize] as usize;
            let (id, src) = match self.kind[v as usize] {
                Kind::Wire => (wires.id[r], None),
                Kind::Noconn => (noconns.id[r], None),
                Kind::Label => (labels.id[r], Some((LABEL, labels.text[r]))),
                Kind::Port => {
                    pins += 1;
                    (ports.id[r], Some((PORT, ports.text[r])))
                }
                Kind::Pin => {
                    pins += 1;
                    let f = &cx.ifaces[cx.inst_iface[r] as usize];
                    let p = self.sub[v as usize];
                    pinw = pinw.max(f.bits(usize::from(p)).len());
                    let take = minpin.0 == ObjId::NONE
                        || match id_cmp(inst.id[r], minpin.0, s) {
                            Ordering::Less => true,
                            Ordering::Equal => u32::from(p) < minpin.1,
                            Ordering::Greater => false,
                        };
                    if take {
                        minpin = (inst.id[r], u32::from(p));
                    }
                    continue;
                }
            };
            if minid == ObjId::NONE || id_cmp(id, minid, s) == Ordering::Less {
                minid = id;
            }
            let Some((class, sym)) = src else {
                continue;
            };
            let text = s.get(sym);
            let before = bits.len();
            if netexpr::expand(text, names, &mut bits, &mut buf).is_err() {
                let detail = format!("`{text}` is wider than 65535 bits");
                found.push(finding(Code::E011, id, Name::NONE, detail));
                continue;
            }
            let global = bits[before..]
                .iter()
                .all(|&b| glob(&self.globals, names, b));
            let class = if global { GLOBAL } else { class };
            srcs.push((class, names.intern(text), id, before, bits.len()));
        }
        let key = if minid == ObjId::NONE {
            minpin
        } else {
            (minid, NONE_PIN)
        };
        assert!(
            key.0 != ObjId::NONE,
            "every component holds an object with an id"
        );

        // ---- best name, E011 (unequal widths); a short (E003) needs a full build
        let mut best: Option<usize> = None;
        for k in 0..srcs.len() {
            let better = best.is_none_or(|b| {
                (srcs[k].0, names.get(srcs[k].1)) < (srcs[b].0, names.get(srcs[b].1))
            });
            if better {
                best = Some(k);
            }
        }
        let width = match best {
            Some(b) => {
                let bb = &bits[srcs[b].3..srcs[b].4];
                for k in (0..srcs.len()).filter(|&k| k != b) {
                    let bk = &bits[srcs[k].3..srcs[k].4];
                    if bk == bb {
                        continue;
                    }
                    if bk.len() == bb.len() {
                        return false;
                    }
                    let detail = format!(
                        "`{}` ({} bits) and `{}` ({} bits) name one net",
                        names.get(srcs[k].1),
                        bk.len(),
                        names.get(srcs[b].1),
                        bb.len()
                    );
                    found.push(finding(Code::E011, srcs[k].2, srcs[k].1, detail));
                }
                bb.len()
            }
            None => pinw,
        };
        assert!(width <= usize::from(u16::MAX), "widths fit u16");

        // ---- bits: a named slot is its name's bit; an unnamed slot gets its own
        let start = cn.net_bits.len();
        for j in 0..width {
            let b = match best {
                Some(b) => self.named_bit(bits[srcs[b].3 + j], cx, cn, names),
                None => {
                    buf.clear();
                    write_key(key, s, &mut buf);
                    if width > 1 {
                        buf.push('[');
                        push_decimal(j as u32, &mut buf);
                        buf.push(']');
                    }
                    let name = names.intern(&buf);
                    self.alloc_bit(name, cx, cn, names)
                }
            };
            cn.net_bits.push(b);
        }
        self.live_bits += width;
        self.named[ci] = best.is_some();
        cn.net_bit_start[ci] = start as u32;
        cn.net_width[ci] = width as u16;
        cn.net_pins[ci] = pins;
        cn.net_global[ci] = cn.net_bits[start..]
            .iter()
            .any(|&b| cn.bit_global[b as usize]);
        cn.net_name[ci] = match best {
            Some(b) => srcs[b].1,
            None if width == 1 => cn.bit_name[cn.net_bits[start] as usize],
            None => {
                buf.clear();
                write_key(key, s, &mut buf);
                names.intern(&buf)
            }
        };

        // ---- pins, W004, row outputs
        for &v in members {
            if self.kind[v as usize] == Kind::Pin {
                found.extend(self.pin(v, c, true, cx, cn, names));
            }
        }
        let lone = members[0] as usize;
        if members.len() == 1 && self.kind[lone] == Kind::Pin {
            let i = self.row[lone] as usize;
            let f = &cx.ifaces[cx.inst_iface[i] as usize];
            let pin = f.pin_name[usize::from(self.sub[lone])];
            let detail = format!("pin `{}` is unconnected", names.get(pin));
            found.push(finding(Code::W004, inst.id[i], pin, detail));
        }
        for &v in members {
            let vi = v as usize;
            match self.kind[vi] {
                Kind::Pin => {
                    let i = self.row[vi] as usize;
                    let k = (v - self.first[INSTS][i]) as usize;
                    cn.pin_net[cn.pin_start[i] as usize + k] = c;
                }
                Kind::Wire if self.sub[vi] == 1 => {}
                _ => self.write_row(v, cn),
            }
        }
        self.new.extend(found.iter().cloned());
        self.found[ci] = found;
        true
    }

    pub(super) fn bit_rows(
        &self,
        b: usize,
        cn: &CellNets,
        own: &Iface,
        names: &Names,
        out: &mut Vec<Violation>,
    ) {
        let counts = (cn.bit_conns[b], self.real[b], self.other[b]);
        bit_findings(
            own.cell,
            cn.bit_name[b],
            cn.bit_global[b],
            counts,
            names,
            out,
        );
    }
}

// ---- private ----

impl Live {
    /// Records bit `b`'s findings as gone the first time this edit changes it.
    fn touch(&mut self, b: u32, cx: &Ctx<'_>, cn: &CellNets, names: &Names) {
        let bi = b as usize;
        if self.bmark[bi] == self.epoch {
            return;
        }
        self.bmark[bi] = self.epoch;
        self.touched.push(b);
        if cn.bit_name[bi] != Name::NONE {
            let mut rows = std::mem::take(&mut self.gone);
            self.bit_rows(bi, cn, cx.own, names, &mut rows);
            self.gone = rows;
        }
    }

    /// Adds (`add`) or takes out the connections and domains pin node `v` gives the bits
    /// of component `c` (`connect`'s bus slicing). Adding writes its `conns` and returns
    /// its `E011` when its width fits neither the net nor a slice of it.
    fn pin(
        &mut self,
        v: u32,
        c: u32,
        add: bool,
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &Names,
    ) -> Option<Violation> {
        let i = self.row[v as usize] as usize;
        let p = usize::from(self.sub[v as usize]);
        let f = &cx.ifaces[cx.inst_iface[i] as usize];
        let w = f.bits(p).len();
        let elems = (cn.elem_start[i + 1] - cn.elem_start[i]) as usize;
        // An alias pin shares its target's connection slots (same component, same width):
        // the target writes and counts them.
        let alias = f.pin_alias[p] != NONE;
        let (base, stride) = (
            cn.conn_start[i] as usize + f.pin_off[p] as usize,
            f.bits_total as usize,
        );
        let net = u32::from(cn.net_width[c as usize]);
        let sliced = match slicing(names.get(f.pin_name[p]), w, elems, net) {
            Ok(sliced) => sliced,
            Err(_) if !add => return None,
            Err(detail) => {
                if !alias {
                    for e in 0..elems {
                        cn.conns[base + e * stride..base + e * stride + w].fill(NO_NET);
                    }
                }
                return Some(Violation::new(
                    Code::E011,
                    cx.own.cell,
                    cx.doc.instances().id[i],
                    f.pin_name[p],
                    detail,
                ));
            }
        };
        let start = cn.net_bit_start[c as usize] as usize;
        let counted = !alias && cn.ignore[i] != Ignore::Open;
        let real = f.pin_domain[p] == Domain::Real;
        for e in 0..elems {
            for j in 0..w {
                let b = cn.net_bits[start + if sliced { e * w + j } else { j }] as usize;
                let dom = if real {
                    &mut self.real[b]
                } else {
                    &mut self.other[b]
                };
                if add {
                    *dom += 1;
                } else {
                    *dom -= 1;
                }
                if counted {
                    if add {
                        cn.bit_conns[b] += 1;
                        cn.conns[base + e * stride + j] = b as u32;
                    } else {
                        cn.bit_conns[b] -= 1;
                    }
                }
            }
        }
        None
    }

    /// A fresh bit named `name` (touched first, so a reused handle's old findings leave).
    fn alloc_bit(&mut self, name: Name, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) -> u32 {
        let b = match self.free_bits.pop() {
            Some(b) => b,
            None => {
                cn.bit_name.push(Name::NONE);
                cn.bit_global.push(false);
                cn.bit_conns.push(0);
                self.slots.push(0);
                self.real.push(0);
                self.other.push(0);
                self.bmark.push(0);
                (cn.bit_name.len() - 1) as u32
            }
        };
        self.touch(b, cx, cn, names);
        let bi = b as usize;
        assert!(cn.bit_name[bi] == Name::NONE && cn.bit_conns[bi] == 0);
        cn.bit_name[bi] = name;
        cn.bit_global[bi] = glob(&self.globals, names, name);
        b
    }

    /// The bit named `name`, made when no slot carried it; counts one more slot.
    fn named_bit(&mut self, name: Name, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) -> u32 {
        let b = match cn.by_name.binary_search_by_key(&name, |p| p.0) {
            Ok(k) => {
                let b = cn.by_name[k].1;
                self.touch(b, cx, cn, names);
                b
            }
            Err(k) => {
                let b = self.alloc_bit(name, cx, cn, names);
                cn.by_name.insert(k, (name, b));
                b
            }
        };
        self.slots[b as usize] += 1;
        b
    }

    fn free_bit(&mut self, b: u32, cn: &mut CellNets) {
        let bi = b as usize;
        assert!(
            self.real[bi] == 0 && self.other[bi] == 0 && cn.bit_conns[bi] == 0,
            "no pin on a freed bit"
        );
        if let Ok(k) = cn.by_name.binary_search_by_key(&cn.bit_name[bi], |p| p.0)
            && cn.by_name[k].1 == b
        {
            cn.by_name.remove(k);
        }
        cn.bit_name[bi] = Name::NONE;
        cn.bit_global[bi] = false;
        self.slots[bi] = 0;
        self.free_bits.push(b);
    }
}
