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
use crate::names::{self, Name, Names};
use crate::netcheck::{self, BitCounts};
use crate::netexpr;
use crate::nets::{self, CellNets, GLOBAL, Ignore, LABEL, NO_NET, NONE_PIN, NameBit, NetKey, PORT};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use std::cmp::Ordering;
use std::mem;

/// Whether [`Live::pin`] adds a pin's counts to its bits or takes them out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tally {
    Add,
    Remove,
}

/// A name source on a component being built: a label or port with its bits.
struct Source {
    /// [`GLOBAL`], [`PORT`] or [`LABEL`].
    class: u8,
    text: Name,
    obj: ObjId,
    /// Its bit names are `bits[from..to]` of the build's bit buffer.
    from: usize,
    to: usize,
}

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
        let free_before = self.free_comps.len();
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
                self.pin(v, c, Tally::Remove, cx, cn, names);
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
        assert_eq!(self.free_comps.len(), free_before + 1, "freed once");
    }

    pub(super) fn alloc_comp(&mut self, cn: &mut CellNets) -> u32 {
        if let Some(c) = self.free_comps.pop() {
            assert!(
                self.head[c as usize] == NONE,
                "a free component has no members"
            );
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
        assert!(!members.is_empty(), "a component has a node");
        assert!(self.head[ci] == NONE, "component {c} is free");
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
        let mut minid = ObjId::NONE;
        let mut minpin = NetKey {
            id: ObjId::NONE,
            pin: 0,
        };
        let mut srcs: Vec<Source> = Vec::new();
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
                    let take = minpin.id == ObjId::NONE
                        || match nets::id_cmp(inst.id[r], minpin.id, s) {
                            Ordering::Less => true,
                            Ordering::Equal => u32::from(p) < minpin.pin,
                            Ordering::Greater => false,
                        };
                    if take {
                        minpin = NetKey {
                            id: inst.id[r],
                            pin: u32::from(p),
                        };
                    }
                    continue;
                }
            };
            if minid == ObjId::NONE || nets::id_cmp(id, minid, s) == Ordering::Less {
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
                .all(|&b| nets::glob(&self.globals, names, b));
            srcs.push(Source {
                class: if global { GLOBAL } else { class },
                text: names.intern(text),
                obj: id,
                from: before,
                to: bits.len(),
            });
        }
        let key = if minid == ObjId::NONE {
            minpin
        } else {
            NetKey {
                id: minid,
                pin: NONE_PIN,
            }
        };
        assert!(
            key.id != ObjId::NONE,
            "every component holds an object with an id"
        );

        // ---- best name, E011 (unequal widths); a short (E003) needs a full build
        let mut best: Option<usize> = None;
        for k in 0..srcs.len() {
            let better = best.is_none_or(|b| {
                (srcs[k].class, names.get(srcs[k].text)) < (srcs[b].class, names.get(srcs[b].text))
            });
            if better {
                best = Some(k);
            }
        }
        let width = match best {
            Some(b) => {
                let bb = &bits[srcs[b].from..srcs[b].to];
                let others = (0..srcs.len()).filter(|&k| k != b);
                for k in others {
                    let bk = &bits[srcs[k].from..srcs[k].to];
                    if bk == bb {
                        continue;
                    }
                    if bk.len() == bb.len() {
                        return false;
                    }
                    let detail = format!(
                        "`{}` ({} bits) and `{}` ({} bits) name one net",
                        names.get(srcs[k].text),
                        bk.len(),
                        names.get(srcs[b].text),
                        bb.len()
                    );
                    found.push(finding(Code::E011, srcs[k].obj, srcs[k].text, detail));
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
                Some(b) => self.named_bit(bits[srcs[b].from + j], cx, cn, names),
                None => {
                    buf.clear();
                    nets::write_key(key, s, &mut buf);
                    if width > 1 {
                        buf.push('[');
                        names::push_decimal(j as u32, &mut buf);
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
            Some(b) => srcs[b].text,
            None if width == 1 => cn.bit_name[cn.net_bits[start] as usize],
            None => {
                buf.clear();
                nets::write_key(key, s, &mut buf);
                names.intern(&buf)
            }
        };
        assert_eq!(
            cn.net_bits.len() - start,
            width,
            "one bit per slot of the width"
        );

        // ---- pins, W004, row outputs
        for &v in members {
            if self.kind[v as usize] == Kind::Pin {
                found.extend(self.pin(v, c, Tally::Add, cx, cn, names));
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
        assert!(
            self.found[ci].is_empty(),
            "a freed component's findings left with it"
        );
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
        let counts = BitCounts {
            conns: cn.bit_conns[b],
            real: self.real[b],
            other: self.other[b],
        };
        netcheck::bit_findings(
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
        assert!(bi < cn.bit_name.len(), "bit {b} out of range");
        if self.bmark[bi] == self.epoch {
            return;
        }
        self.bmark[bi] = self.epoch;
        self.touched.push(b);
        if cn.bit_name[bi] != Name::NONE {
            let mut rows = mem::take(&mut self.gone);
            self.bit_rows(bi, cn, cx.own, names, &mut rows);
            self.gone = rows;
        }
    }

    /// Adds or takes out (`tally`) the connections and domains pin node `v` gives the bits
    /// of component `c` (`connect`'s bus slicing). Adding writes its `conns` and returns
    /// its `E011` when its width fits neither the net nor a slice of it.
    fn pin(
        &mut self,
        v: u32,
        c: u32,
        tally: Tally,
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &Names,
    ) -> Option<Violation> {
        assert_eq!(self.kind[v as usize], Kind::Pin, "node {v} is a pin");
        assert_eq!(self.comp[v as usize], c, "pin {v} is on component {c}");
        let add = tally == Tally::Add;
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
        let sliced = match nets::slicing(names.get(f.pin_name[p]), w, elems, net) {
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
        assert!(cn.bit_name[bi] == Name::NONE, "bit {b} is free");
        assert_eq!(cn.bit_conns[bi], 0, "a free bit has no connections");
        cn.bit_name[bi] = name;
        cn.bit_global[bi] = nets::glob(&self.globals, names, name);
        assert_eq!(self.slots.len(), cn.bit_name.len(), "bit columns in step");
        b
    }

    /// The bit named `name`, made when no slot carried it; counts one more slot.
    fn named_bit(&mut self, name: Name, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) -> u32 {
        let b = match cn.by_name.binary_search_by_key(&name, |p| p.name) {
            Ok(k) => {
                let b = cn.by_name[k].bit;
                self.touch(b, cx, cn, names);
                b
            }
            Err(k) => {
                let b = self.alloc_bit(name, cx, cn, names);
                cn.by_name.insert(k, NameBit { name, bit: b });
                b
            }
        };
        self.slots[b as usize] += 1;
        assert_eq!(
            cn.bit_name[b as usize], name,
            "the slot carries its bit's name"
        );
        b
    }

    fn free_bit(&mut self, b: u32, cn: &mut CellNets) {
        let bi = b as usize;
        assert_eq!(self.real[bi], 0, "no real pin on a freed bit");
        assert_eq!(self.other[bi], 0, "no other pin on a freed bit");
        assert_eq!(cn.bit_conns[bi], 0, "no connection on a freed bit");
        if let Ok(k) = cn
            .by_name
            .binary_search_by_key(&cn.bit_name[bi], |p| p.name)
            && cn.by_name[k].bit == b
        {
            cn.by_name.remove(k);
        }
        cn.bit_name[bi] = Name::NONE;
        cn.bit_global[bi] = false;
        self.slots[bi] = 0;
        self.free_bits.push(b);
    }
}
