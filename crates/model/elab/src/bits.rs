//! Components and their names in, bit nets out (netlist spec §3–§5): every component bit,
//! own pin bit and `inherit` override bit is a slot; slots merge by equal name and by
//! `ignore short`; each merged set is a bit net, named by its best name (global, port,
//! label; then bytewise) or `net_<id>[k]`. Then the [`CellNets`] row outputs:
//! connections (bus slicing, §4), ports, overrides, component rows, pin components and
//! connection counts. The phases `connect` runs after naming its components.
//!
//! Contents:
//! - `pub(crate) struct Slots`, `pub(crate) struct NamedSlot`, `pub(crate) struct SlotDomain`
//! - `pub(crate) struct BitNets`, `pub(crate) struct DomainCounts`
//! - `pub(crate) fn slots(cx, geo, comps, src, names, cn, out) -> Slots`
//! - `pub(crate) fn merge(slots: Slots, cn: &CellNets) -> BitNets`
//! - `pub(crate) fn name_bits(cx, comps, bits, names, cn)`
//! - `pub(crate) fn outputs(cx, geo, comps, src, bits, names, cn)`
//! - `pub(crate) fn domains(bits: &BitNets) -> DomainCounts`

use crate::connect::{Comps, Cx, Found, Sources};
use crate::iface::Domain;
use crate::names::{self, Name, Names};
use crate::netexpr;
use crate::nets::{
    self, CellNets, GLOBAL, Ignore, LABEL, NO_NET, NONE_PIN, NameBit, NetKey, OWN_STATIC, PORT,
    Seed,
};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use std::cmp::Ordering;
use std::ops::Range;

/// The slots of one `connect` run: component bits first (`start[c]..start[c + 1]`), then
/// one per own pin bit and per override bit.
pub(crate) struct Slots {
    /// CSR offsets (`components + 1`) of each component's slots.
    pub(crate) start: Vec<u32>,
    /// Slots in all.
    pub(crate) count: u32,
    /// Every named slot; sorted by (name, slot) by [`merge`].
    pub(crate) pairs: Vec<NamedSlot>,
    /// CSR offsets (`own pins + 1`) into `port`.
    pub(crate) port_start: Vec<u32>,
    /// Slots of the own pins' bits.
    pub(crate) port: Vec<u32>,
    /// CSR offsets (`overrides + 1`) into `inh`.
    pub(crate) inh_start: Vec<u32>,
    /// Slots of the override nets' bits.
    pub(crate) inh: Vec<u32>,
    /// Slot of each connection (`CellNets::conn_start` layout), `u32::MAX` unconnected.
    pub(crate) conn: Vec<u32>,
    /// The slot and domain of every instance pin bit.
    pub(crate) domain: Vec<SlotDomain>,
}

/// A slot that carries a bit name.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NamedSlot {
    pub(crate) name: Name,
    pub(crate) slot: u32,
    /// [`GLOBAL`], [`PORT`] or [`LABEL`].
    pub(crate) class: u8,
}

/// The domain of an instance pin bit's slot (`W010`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotDomain {
    pub(crate) slot: u32,
    pub(crate) domain: Domain,
}

/// The slots merged into bit nets: what [`merge`] returns.
pub(crate) struct BitNets {
    pub(crate) slots: Slots,
    /// Bit net of each slot.
    pub(crate) net_of: Vec<u32>,
    /// Bit nets in all (dense, `0..count`).
    pub(crate) count: usize,
}

/// Instance pin bits on each bit net, by domain (`W010`).
pub(crate) struct DomainCounts {
    /// Pin bits in the `real` domain.
    pub(crate) real: Vec<u32>,
    /// Pin bits in the other domains.
    pub(crate) other: Vec<u32>,
}

/// The bit a still-unnamed bit net takes its generated name from: the smallest
/// (component key, bit index) among its slots, and the component's width.
#[derive(Clone, Copy)]
struct Unnamed {
    key: NetKey,
    bit: u32,
    width: u32,
}

/// Lays out the slots: component bits (named by their usable sources), own pins and
/// inherited pins (bound by name), `inherit <pin> <net>` overrides (written to
/// `cn.inh_inst`, `cn.inh_pin`), and every instance connection with its bus slicing
/// (written to `cn.conn_start`). Width findings (`E011`) go to `out`.
pub(crate) fn slots(
    cx: &Cx<'_>,
    geo: &Seed,
    comps: &Comps,
    src: &Sources,
    names: &mut Names,
    cn: &mut CellNets,
    out: &mut Found<'_>,
) -> Slots {
    let nc = comps.width.len();
    assert_eq!(comps.best.len(), nc, "component columns in step");
    assert_eq!(src.usable.len(), src.comp.len(), "source columns in step");
    let mut start = vec![0u32; nc + 1];
    for c in 0..nc {
        start[c + 1] = start[c] + comps.width[c];
    }
    let mut sl = Slots {
        count: start[nc],
        start,
        pairs: Vec::new(),
        port_start: vec![0],
        port: Vec::new(),
        inh_start: vec![0],
        inh: Vec::new(),
        conn: Vec::new(),
        domain: Vec::new(),
    };
    let usable = (0..src.comp.len()).filter(|&k| src.usable[k]);
    for k in usable {
        let c = src.comp[k] as usize;
        for (j, &b) in src.bits(k).iter().enumerate() {
            sl.pairs.push(NamedSlot {
                name: b,
                slot: sl.start[c] + j as u32,
                class: class_of(cx, names, b, src.class[k]),
            });
        }
    }
    // Own interface pins (ports and inherited pins bind by name).
    for p in 0..cx.own.pin_name.len() {
        let fresh = sl.name_slots(cx, cx.own.bits(p), PORT, names);
        sl.port.extend(fresh);
        sl.port_start.push(sl.port.len() as u32);
    }
    sl.overrides(cx, names, cn, out);
    sl.connections(cx, geo, comps, names, cn, out);
    assert_eq!(sl.port_start.len(), cx.own.pin_name.len() + 1);
    assert_eq!(cn.conn_start.len(), cx.doc.instances().id.len() + 1);
    assert!(
        sl.pairs.iter().all(|p| p.slot < sl.count),
        "named slots exist"
    );
    sl
}

/// Merges slots of equal names, then the connections of each `ignore short` instance, and
/// numbers the merged sets densely in slot order.
pub(crate) fn merge(mut sl: Slots, cn: &CellNets) -> BitNets {
    assert_eq!(
        cn.conn_start.len(),
        cn.ignore.len() + 1,
        "instance columns in step"
    );
    let mut sp: Vec<u32> = (0..sl.count).collect();
    sl.pairs.sort_unstable_by_key(|p| (p.name, p.slot));
    for w in sl.pairs.windows(2) {
        if w[0].name == w[1].name {
            nets::union(&mut sp, w[0].slot, w[1].slot);
        }
    }
    let shorted = (0..cn.ignore.len()).filter(|&i| cn.ignore[i] == Ignore::Short);
    for i in shorted {
        let r = cn.conn_start[i] as usize..cn.conn_start[i + 1] as usize;
        let mut live = sl.conn[r].iter().copied().filter(|&s| s != NONE);
        if let Some(first) = live.next() {
            for slot in live {
                nets::union(&mut sp, first, slot);
            }
        }
    }
    let mut net_of = vec![NONE; sl.count as usize];
    let mut nbits = 0u32;
    for slot in 0..sl.count as usize {
        let r = nets::find(&mut sp, slot as u32) as usize; // the set's first slot (`geometry`)
        net_of[slot] = if r == slot {
            nbits += 1;
            nbits - 1
        } else {
            net_of[r]
        };
    }
    assert!(nbits <= sl.count);
    debug_assert!(
        net_of.iter().all(|&b| b < nbits),
        "every slot is on a bit net"
    );
    BitNets {
        slots: sl,
        net_of,
        count: nbits as usize,
    }
}

/// Names the `nb` bit nets: the best named slot's name (class, then bytewise), else
/// `net_<key>[k]` from the smallest (component key, bit index) among its slots, interned
/// as one batch in bit order. Fills `bit_name`, `bit_global` and `by_name`.
pub(crate) fn name_bits(
    cx: &Cx<'_>,
    comps: &Comps,
    bits: &BitNets,
    names: &mut Names,
    cn: &mut CellNets,
) {
    let (sl, net_of, nb) = (&bits.slots, &bits.net_of, bits.count);
    assert_eq!(net_of.len(), sl.count as usize, "one bit net per slot");
    let s = cx.doc.strings();
    // (class, name) of each bit net's best named slot.
    let mut best: Vec<(u8, Name)> = vec![(u8::MAX, Name::NONE); nb];
    for p in &sl.pairs {
        let b = net_of[p.slot as usize] as usize;
        let (bc, bn) = best[b];
        if p.class < bc || (p.class == bc && names.cmp(p.name, bn) == Ordering::Less) {
            best[b] = (p.class, p.name);
        }
    }
    // Owned ids sort before pin nodes; then by id text, then pin index.
    let key_cmp = |a: NetKey, b: NetKey| {
        (a.pin != NONE_PIN)
            .cmp(&(b.pin != NONE_PIN))
            .then_with(|| nets::id_cmp(a.id, b.id, s))
            .then(a.pin.cmp(&b.pin))
    };
    // Unnamed bits: smallest (component key, bit index) among the slots, with the width.
    let none = Unnamed {
        key: NetKey {
            id: ObjId::NONE,
            pin: 0,
        },
        bit: 0,
        width: 1,
    };
    let mut unnamed: Vec<Unnamed> = vec![none; nb];
    for c in 0..comps.width.len() {
        for j in 0..comps.width[c] {
            let b = net_of[(sl.start[c] + j) as usize] as usize;
            if best[b].1 != Name::NONE {
                continue;
            }
            let held = unnamed[b];
            let take = held.key.id == ObjId::NONE
                || match key_cmp(comps.key[c], held.key) {
                    Ordering::Less => true,
                    Ordering::Equal => j < held.bit,
                    Ordering::Greater => false,
                };
            if take {
                unnamed[b] = Unnamed {
                    key: comps.key[c],
                    bit: j,
                    width: comps.width[c],
                };
            }
        }
    }
    cn.bit_name = best.iter().map(|b| b.1).collect();
    // Generated names are written into one buffer and interned as a batch, in bit order
    // (the order one-by-one interning used).
    let gen_bits: Vec<usize> = (0..nb).filter(|&b| best[b].1 == Name::NONE).collect();
    let mut gen_text = String::with_capacity(gen_bits.len() * 16);
    let mut gen_end = Vec::with_capacity(gen_bits.len());
    for &b in &gen_bits {
        let Unnamed { key, bit, width } = unnamed[b];
        assert!(
            key.id != ObjId::NONE,
            "every component holds an object with an id"
        );
        nets::write_key(key, s, &mut gen_text);
        if width > 1 {
            gen_text.push('[');
            names::push_decimal(bit, &mut gen_text);
            gen_text.push(']');
        }
        gen_end.push(gen_text.len());
    }
    let texts: Vec<&str> = (0..gen_end.len())
        .map(|g| &gen_text[if g == 0 { 0 } else { gen_end[g - 1] }..gen_end[g]])
        .collect();
    let mut gen_name = Vec::with_capacity(texts.len());
    names.intern_batch(&texts, &mut gen_name);
    assert_eq!(gen_name.len(), gen_bits.len(), "one name per generated bit");
    cn.bit_global = vec![false; nb];
    for (b, best) in best.iter().enumerate() {
        if best.1 != Name::NONE {
            cn.bit_global[b] = nets::glob(cx.globals, names, best.1);
        }
    }
    // A generated bit's text is at hand: no second read of the arena per bit.
    for (g, &b) in gen_bits.iter().enumerate() {
        cn.bit_name[b] = gen_name[g];
        cn.bit_global[b] =
            netexpr::is_global(texts[g]) || cx.globals.binary_search(&gen_name[g]).is_ok();
    }
    // All pairs with one name were merged, so one net per name; `pairs` is sorted by
    // name, so `by_name` already is (no sort).
    cn.by_name = sl
        .pairs
        .iter()
        .map(|p| NameBit {
            name: p.name,
            bit: net_of[p.slot as usize],
        })
        .collect();
    assert!(
        cn.by_name.windows(2).all(|w| w[0] <= w[1]),
        "pairs sorted by name"
    );
    cn.by_name.dedup_by_key(|p| p.name);
    debug_assert!(cn.by_name.windows(2).all(|w| w[0].name < w[1].name));
}

/// The row outputs: connections, own pin and override bits as bit nets (`ignore open`
/// rows unconnected), component rows (name, width, pins, bits, global), the component of
/// every wire, label, port and instance pin, and each bit's connection count.
pub(crate) fn outputs(
    cx: &Cx<'_>,
    geo: &Seed,
    comps: &Comps,
    src: &Sources,
    bits: &BitNets,
    names: &mut Names,
    cn: &mut CellNets,
) {
    let (sl, net_of) = (&bits.slots, &bits.net_of);
    assert_eq!(cn.bit_name.len(), bits.count, "name_bits ran first");
    assert!(cn.net_name.is_empty(), "component rows are written once");
    let s = cx.doc.strings();
    let to_net = |slot: u32| {
        if slot == NONE {
            NO_NET
        } else {
            net_of[slot as usize]
        }
    };
    cn.conns = sl.conn.iter().map(|&x| to_net(x)).collect();
    cn.port_bit_start.clone_from(&sl.port_start);
    cn.port_bits = sl.port.iter().map(|&x| to_net(x)).collect();
    cn.inh_bit_start.clone_from(&sl.inh_start);
    cn.inh_bits = sl.inh.iter().map(|&x| to_net(x)).collect();
    let opened = (0..cn.ignore.len()).filter(|&i| cn.ignore[i] == Ignore::Open);
    for i in opened {
        let r = cn.conn_start[i] as usize..cn.conn_start[i + 1] as usize;
        cn.conns[r].fill(NO_NET);
    }
    let mut buf = String::new();
    for c in 0..comps.width.len() {
        let width = comps.width[c];
        cn.net_name.push(match comps.best[c] {
            NONE if width == 1 => cn.bit_name[net_of[sl.start[c] as usize] as usize],
            NONE => {
                buf.clear();
                nets::write_key(comps.key[c], s, &mut buf);
                names.intern(&buf)
            }
            k => src.text[k as usize],
        });
        assert!(width <= u32::from(u16::MAX), "widths fit u16 (E011 above)");
        cn.net_width.push(width as u16);
        cn.net_pins.push(comps.pins[c]);
        cn.net_bit_start.push(cn.net_bits.len() as u32);
        let r = sl.start[c]..sl.start[c + 1];
        cn.net_bits.extend(r.map(|x| net_of[x as usize]));
        let global = cn.bits_of(c).iter().any(|&b| cn.bit_global[b as usize]);
        cn.net_global.push(global);
    }
    let comp = &geo.comp;
    let doc = cx.doc;
    cn.wire_net = (0..doc.wires().id.len()).map(|w| comp[2 * w]).collect();
    cn.label_net = (0..doc.labels().id.len())
        .map(|l| comp[geo.labels0 + l])
        .collect();
    cn.port_net = (0..doc.ports().id.len())
        .map(|p| comp[geo.ports0 + p])
        .collect();
    cn.pin_net = comp[geo.pins0..geo.labels0].to_vec();
    let ni = doc.instances().id.len();
    cn.pin_start = Vec::with_capacity(ni + 1);
    cn.pin_start.push(0);
    let mut k = 0usize;
    for i in 0..ni {
        while k < geo.pin_inst.len() && geo.pin_inst[k] as usize == i {
            k += 1;
        }
        cn.pin_start.push(k as u32);
    }
    assert_eq!(k, cn.pin_net.len());
    cn.bit_conns = vec![0; cn.bit_name.len()];
    let connected = cn.conns.iter().chain(&cn.port_bits).chain(&cn.inh_bits);
    for &b in connected {
        if b != NO_NET {
            cn.bit_conns[b as usize] += 1;
        }
    }
    assert_eq!(
        cn.net_name.len(),
        comps.width.len(),
        "one row per component"
    );
    assert_eq!(
        cn.net_bit_start.len(),
        cn.net_width.len(),
        "component columns in step"
    );
}

/// Instance pin bits on each bit net in the `real` domain and in the others (`W010`).
pub(crate) fn domains(bits: &BitNets) -> DomainCounts {
    let nb = bits.count;
    let (mut real, mut other) = (vec![0u32; nb], vec![0u32; nb]);
    for d in &bits.slots.domain {
        let b = bits.net_of[d.slot as usize] as usize;
        if d.domain == Domain::Real {
            real[b] += 1;
        } else {
            other[b] += 1;
        }
    }
    let total: u64 = real.iter().chain(&other).map(|&n| u64::from(n)).sum();
    assert_eq!(
        total,
        bits.slots.domain.len() as u64,
        "every pin bit counted once"
    );
    DomainCounts { real, other }
}

// ---- private ----

impl Slots {
    /// Fresh slots for the bits `text_bits` bound by name in `class`.
    fn name_slots(
        &mut self,
        cx: &Cx<'_>,
        text_bits: &[Name],
        class: u8,
        names: &Names,
    ) -> Range<u32> {
        let first = self.count;
        for &b in text_bits {
            self.pairs.push(NamedSlot {
                name: b,
                slot: self.count,
                class: class_of(cx, names, b, class),
            });
            self.count += 1;
        }
        assert_eq!(self.count - first, text_bits.len() as u32);
        first..self.count
    }

    /// The `inherit <pin> <net>` override slots of every instance. A declared inherited
    /// pin checks its width here (`E011`); a pin propagated from the instance's subtree is
    /// checked where elaboration resolves it (§5).
    fn overrides(
        &mut self,
        cx: &Cx<'_>,
        names: &mut Names,
        cn: &mut CellNets,
        out: &mut Found<'_>,
    ) {
        let doc = cx.doc;
        let (s, inst) = (doc.strings(), doc.instances());
        assert!(cn.inh_inst.is_empty(), "overrides are laid out once");
        let Some(inherit_key) = s.find("inherit") else {
            return; // the document never interned the key: no row has it
        };
        let finding =
            |object, param, detail| Violation::new(Code::E011, cx.own.cell, object, param, detail);
        let (mut tmp, mut buf) = (Vec::new(), String::new());
        for i in 0..inst.id.len() {
            for (k, vals) in doc.pool().attrs(inst.attrs[i]) {
                if k != inherit_key || vals.len() != 2 {
                    continue;
                }
                let (pin, net) = (s.get(vals[0]), s.get(vals[1]));
                tmp.clear();
                if netexpr::expand(net, names, &mut tmp, &mut buf).is_err() {
                    let detail = format!("`{net}` is wider than 65535 bits");
                    out.push(OWN_STATIC, finding(inst.id[i], Name::NONE, detail));
                    continue;
                }
                let pin_name = names.intern(pin);
                if let Some(f) = cx.ifaces.get(cx.inst_iface[i] as usize) {
                    let declared = (0..f.pin_name.len())
                        .find(|&p| f.pin_name[p] == pin_name && f.pin_inherit[p] != Name::NONE);
                    if let Some(p) = declared.filter(|&p| f.bits(p).len() != tmp.len()) {
                        let detail = format!(
                            "inherit `{pin}` ({} bits) set to `{net}` ({} bits)",
                            f.bits(p).len(),
                            tmp.len()
                        );
                        out.push(OWN_STATIC, finding(inst.id[i], pin_name, detail));
                        continue;
                    }
                }
                let fresh = self.name_slots(cx, &tmp, LABEL, names);
                cn.inh_inst.push(i as u32);
                cn.inh_pin.push(pin_name);
                self.inh.extend(fresh);
                self.inh_start.push(self.inh.len() as u32);
            }
        }
        assert_eq!(self.inh_start.len(), cn.inh_inst.len() + 1);
    }

    /// Every instance's connection slots, element by element, by bus slicing (§4): a pin
    /// takes its whole net, or one slice per element; a net of neither width is `E011`
    /// and the pin stays unconnected.
    fn connections(
        &mut self,
        cx: &Cx<'_>,
        geo: &Seed,
        comps: &Comps,
        names: &Names,
        cn: &mut CellNets,
        out: &mut Found<'_>,
    ) {
        let inst = cx.doc.instances();
        assert!(cn.conn_start.is_empty(), "connections are laid out once");
        assert_eq!(
            cn.elem_start.len(),
            inst.id.len() + 1,
            "element names come first"
        );
        let mut node = geo.pins0;
        for i in 0..inst.id.len() {
            cn.conn_start.push(self.conn.len() as u32);
            let Some(f) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
                continue;
            };
            let elems = (cn.elem_start[i + 1] - cn.elem_start[i]) as usize;
            let stride = f.bits_total as usize;
            let base = self.conn.len();
            self.conn.resize(base + elems * stride, NONE);
            let ordinary = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE);
            for p in ordinary {
                let c = geo.comp[node] as usize;
                node += 1;
                let w = f.bits(p).len();
                let pin = names.get(f.pin_name[p]);
                let sliced = match nets::slicing(pin, w, elems, comps.width[c]) {
                    Ok(sliced) => sliced,
                    Err(detail) => {
                        let v = Violation::new(
                            Code::E011,
                            cx.own.cell,
                            inst.id[i],
                            f.pin_name[p],
                            detail,
                        );
                        out.push(c as u32, v);
                        continue;
                    }
                };
                for e in 0..elems {
                    for j in 0..w {
                        let bit = if sliced { e * w + j } else { j };
                        let slot = self.start[c] + bit as u32;
                        self.conn[base + e * stride + f.pin_off[p] as usize + j] = slot;
                        self.domain.push(SlotDomain {
                            slot,
                            domain: f.pin_domain[p],
                        });
                    }
                }
            }
        }
        cn.conn_start.push(self.conn.len() as u32);
        assert_eq!(node, geo.labels0, "every pin node consumed in order");
    }
}

/// A slot's class: [`GLOBAL`] when bit `b` is global, else `class`.
fn class_of(cx: &Cx<'_>, names: &Names, b: Name, class: u8) -> u8 {
    assert!(class <= LABEL, "a class is GLOBAL, PORT or LABEL");
    if nets::glob(cx.globals, names, b) {
        GLOBAL
    } else {
        class
    }
}
