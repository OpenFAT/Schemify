//! Components and their names in, bit nets out (netlist spec §3–§5): every component bit,
//! own pin bit and `inherit` override bit is a slot; slots merge by equal name and by
//! `ignore short`; each merged set is a bit net, named by its best name (global, port,
//! label; then bytewise) or `net_<id>[k]`. Then the [`CellNets`] row outputs:
//! connections (bus slicing, §4), ports, overrides, component rows, pin components and
//! connection counts. The phases `connect` runs after naming its components.
//!
//! Contents:
//! - `pub(crate) struct Slots`
//! - `pub(crate) fn slots(cx, geo, comps, src, names, cn, out) -> Slots`
//! - `pub(crate) fn merge(slots: &mut Slots, cn: &CellNets) -> (Vec<u32>, usize)`
//! - `pub(crate) fn name_bits(cx, comps, slots, net_of, nb, names, cn)`
//! - `pub(crate) fn outputs(cx, geo, comps, src, slots, net_of, names, cn)`
//! - `pub(crate) fn domains(slots: &Slots, net_of: &[u32], nb: usize) -> (Vec<u32>, Vec<u32>)`

use crate::connect::{Comps, Cx, Found, Sources};
use crate::iface::Domain;
use crate::names::{Name, Names, push_decimal};
use crate::netexpr::{self, is_global};
use crate::nets::{
    CellNets, GLOBAL, Ignore, LABEL, NO_NET, NONE_PIN, OWN_STATIC, PORT, Seed, find, glob, id_cmp,
    slicing, union, write_key,
};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use std::cmp::Ordering;

/// The slots of one `connect` run: component bits first (`start[c]..start[c + 1]`), then
/// one per own pin bit and per override bit.
pub(crate) struct Slots {
    /// CSR offsets (`components + 1`) of each component's slots.
    pub(crate) start: Vec<u32>,
    /// Slots in all.
    pub(crate) count: u32,
    /// `(bit name, slot, class)` of every named slot; sorted by (name, slot) by [`merge`].
    pub(crate) pairs: Vec<(Name, u32, u8)>,
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
    /// `(slot, domain)` of every instance pin bit.
    pub(crate) domain: Vec<(u32, Domain)>,
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
    for k in (0..src.comp.len()).filter(|&k| src.usable[k]) {
        let c = src.comp[k] as usize;
        for (j, &b) in src.bits(k).iter().enumerate() {
            let class = class_of(cx, names, b, src.class[k]);
            sl.pairs.push((b, sl.start[c] + j as u32, class));
        }
    }
    // Own interface pins (ports and inherited pins bind by name).
    for p in 0..cx.own.pin_name.len() {
        let (a, b) = sl.name_slots(cx, cx.own.bits(p), PORT, names);
        sl.port.extend(a..b);
        sl.port_start.push(sl.port.len() as u32);
    }
    sl.overrides(cx, names, cn, out);
    sl.connections(cx, geo, comps, names, cn, out);
    assert_eq!(sl.port_start.len(), cx.own.pin_name.len() + 1);
    sl
}

/// Merges slots of equal names, then the connections of each `ignore short` instance, and
/// numbers the merged sets densely in slot order: `(bit net of each slot, bit nets)`.
pub(crate) fn merge(sl: &mut Slots, cn: &CellNets) -> (Vec<u32>, usize) {
    let mut sp: Vec<u32> = (0..sl.count).collect();
    sl.pairs.sort_unstable_by_key(|p| (p.0, p.1));
    for w in sl.pairs.windows(2) {
        if w[0].0 == w[1].0 {
            union(&mut sp, w[0].1, w[1].1);
        }
    }
    for i in (0..cn.ignore.len()).filter(|&i| cn.ignore[i] == Ignore::Short) {
        let r = cn.conn_start[i] as usize..cn.conn_start[i + 1] as usize;
        let mut live = sl.conn[r].iter().copied().filter(|&s| s != NONE);
        if let Some(first) = live.next() {
            for slot in live {
                union(&mut sp, first, slot);
            }
        }
    }
    let mut net_of = vec![NONE; sl.count as usize];
    let mut nbits = 0u32;
    for slot in 0..sl.count as usize {
        let r = find(&mut sp, slot as u32) as usize; // the set's first slot (see `geometry`)
        net_of[slot] = if r == slot {
            nbits += 1;
            nbits - 1
        } else {
            net_of[r]
        };
    }
    assert!(nbits <= sl.count);
    (net_of, nbits as usize)
}

/// Names the `nb` bit nets: the best named slot's name (class, then bytewise), else
/// `net_<key>[k]` from the smallest (component key, bit index) among its slots, interned
/// as one batch in bit order. Fills `bit_name`, `bit_global` and `by_name`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn name_bits(
    cx: &Cx<'_>,
    comps: &Comps,
    sl: &Slots,
    net_of: &[u32],
    nb: usize,
    names: &mut Names,
    cn: &mut CellNets,
) {
    let s = cx.doc.strings();
    let mut best: Vec<(u8, Name)> = vec![(u8::MAX, Name::NONE); nb];
    for &(name, slot, class) in &sl.pairs {
        let b = net_of[slot as usize] as usize;
        let (bc, bn) = best[b];
        if class < bc || (class == bc && names.cmp(name, bn) == Ordering::Less) {
            best[b] = (class, name);
        }
    }
    // Owned ids sort before pin nodes; then by id text, then pin index.
    let key_cmp = |a: (ObjId, u32), b: (ObjId, u32)| {
        (a.1 != NONE_PIN)
            .cmp(&(b.1 != NONE_PIN))
            .then_with(|| id_cmp(a.0, b.0, s))
            .then(a.1.cmp(&b.1))
    };
    // Unnamed bits: smallest (component key, bit index) among the slots, with the width.
    let mut unnamed: Vec<((ObjId, u32), u32, u32)> = vec![((ObjId::NONE, 0), 0, 1); nb];
    for c in 0..comps.width.len() {
        for j in 0..comps.width[c] {
            let b = net_of[(sl.start[c] + j) as usize] as usize;
            if best[b].1 != Name::NONE {
                continue;
            }
            let (key, k, _) = unnamed[b];
            let take = key.0 == ObjId::NONE
                || match key_cmp(comps.key[c], key) {
                    Ordering::Less => true,
                    Ordering::Equal => j < k,
                    Ordering::Greater => false,
                };
            if take {
                unnamed[b] = (comps.key[c], j, comps.width[c]);
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
        let (key, k, width) = unnamed[b];
        assert!(
            key.0 != ObjId::NONE,
            "every component holds an object with an id"
        );
        write_key(key, s, &mut gen_text);
        if width > 1 {
            gen_text.push('[');
            push_decimal(k, &mut gen_text);
            gen_text.push(']');
        }
        gen_end.push(gen_text.len());
    }
    let texts: Vec<&str> = (0..gen_end.len())
        .map(|g| &gen_text[if g == 0 { 0 } else { gen_end[g - 1] }..gen_end[g]])
        .collect();
    let mut gen_name = Vec::with_capacity(texts.len());
    names.intern_batch(&texts, &mut gen_name);
    cn.bit_global = vec![false; nb];
    for (b, best) in best.iter().enumerate().filter(|(_, b)| b.1 != Name::NONE) {
        cn.bit_global[b] = glob(cx.globals, names, best.1);
    }
    // A generated bit's text is at hand: no second read of the arena per bit.
    for (g, &b) in gen_bits.iter().enumerate() {
        cn.bit_name[b] = gen_name[g];
        cn.bit_global[b] = is_global(texts[g]) || cx.globals.binary_search(&gen_name[g]).is_ok();
    }
    // All pairs with one name were merged, so one net per name; `pairs` is sorted by
    // name, so `by_name` already is (no sort).
    cn.by_name = sl
        .pairs
        .iter()
        .map(|p| (p.0, net_of[p.1 as usize]))
        .collect();
    assert!(
        cn.by_name.windows(2).all(|w| w[0] <= w[1]),
        "pairs sorted by name"
    );
    cn.by_name.dedup_by_key(|p| p.0);
    debug_assert!(cn.by_name.windows(2).all(|w| w[0].0 < w[1].0));
}

/// The row outputs: connections, own pin and override bits as bit nets (`ignore open`
/// rows unconnected), component rows (name, width, pins, bits, global), the component of
/// every wire, label, port and instance pin, and each bit's connection count.
#[allow(clippy::too_many_arguments)]
pub(crate) fn outputs(
    cx: &Cx<'_>,
    geo: &Seed,
    comps: &Comps,
    src: &Sources,
    sl: &Slots,
    net_of: &[u32],
    names: &mut Names,
    cn: &mut CellNets,
) {
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
    for i in (0..cn.ignore.len()).filter(|&i| cn.ignore[i] == Ignore::Open) {
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
                write_key(comps.key[c], s, &mut buf);
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
    for &b in cn.conns.iter().chain(&cn.port_bits).chain(&cn.inh_bits) {
        if b != NO_NET {
            cn.bit_conns[b as usize] += 1;
        }
    }
}

/// Instance pin bits on each of the `nb` bit nets in the `real` domain and in the others
/// (`W010`).
pub(crate) fn domains(sl: &Slots, net_of: &[u32], nb: usize) -> (Vec<u32>, Vec<u32>) {
    let (mut real, mut other) = (vec![0u32; nb], vec![0u32; nb]);
    for &(slot, d) in &sl.domain {
        let b = net_of[slot as usize] as usize;
        if d == Domain::Real {
            real[b] += 1;
        } else {
            other[b] += 1;
        }
    }
    assert_eq!(real.len(), other.len());
    (real, other)
}

// ---- private ----

impl Slots {
    /// Fresh slots for the bits `text_bits` bound by name in `class`: `(first, end)`.
    fn name_slots(
        &mut self,
        cx: &Cx<'_>,
        text_bits: &[Name],
        class: u8,
        names: &Names,
    ) -> (u32, u32) {
        let first = self.count;
        for &b in text_bits {
            self.pairs
                .push((b, self.count, class_of(cx, names, b, class)));
            self.count += 1;
        }
        assert_eq!(self.count - first, text_bits.len() as u32);
        (first, self.count)
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
                let (a, b) = self.name_slots(cx, &tmp, LABEL, names);
                cn.inh_inst.push(i as u32);
                cn.inh_pin.push(pin_name);
                self.inh.extend(a..b);
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
            for p in (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE) {
                let c = geo.comp[node] as usize;
                node += 1;
                let w = f.bits(p).len();
                let sliced = match slicing(names.get(f.pin_name[p]), w, elems, comps.width[c]) {
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
                        self.domain.push((slot, f.pin_domain[p]));
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
    if glob(cx.globals, names, b) {
        GLOBAL
    } else {
        class
    }
}
