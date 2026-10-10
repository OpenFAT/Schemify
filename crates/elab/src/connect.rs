//! Connectivity of one cell (netlist spec §3–§5): geometry → connected components
//! ("nets", what render highlights and the plugin `Nets` table lists) → bit nets
//! (what netlists connect), with names, bus slicing, iterated instances, inherited
//! connection overrides and `ignore short`.
//!
//! Six questions:
//! 1. In: one document's rank-3 tables plus the [`Iface`] of every instance. Out:
//!    [`CellNets`].
//! 2. Up to 2^22 objects per kind; a 100k-object cell has ~300k nodes.
//! 3. Coordinates i32 milli-units (D33); node, component and bit indices `u32`; widths
//!    ≤ 2^16 − 1 (`u16`).
//! 4. Whole-table scans: sort the nodes once by `(x, y)` and once by `(y, x)`; every
//!    axis-aligned wire is two binary searches plus the nodes on it.
//! 5. Lifetime: until the next edit of the cell's geometry (or of an instance's
//!    interface); the editor rebuilds, elaboration builds once per reached cell.
//! 6. Cells are independent: elaboration can run cells in parallel.
//!
//! SIMD triage: the hot loops are the two sorts (std) and union-find (a pointer chase,
//! a chain): neither has the lane shape. The only lane-independent loop, the
//! collinearity scan for diagonal wires, runs over the few nodes in a diagonal wire's
//! x-range, so it stays scalar.

use crate::check::{Code, Violation};
use crate::iface::{Domain, Iface};
use crate::names::{Name, Names};
use crate::netexpr::{self, is_global};
use crate::values::NONE;
use schemify_doc::{Document, ObjId, Strings};
use std::cmp::Ordering;

/// "Not connected" in [`CellNets::conns`] (a pin-width mismatch, `E011`).
pub const NO_NET: u32 = u32::MAX;

/// How an instance is left out of netlists (netlist spec §7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ignore {
    /// Emitted normally.
    No,
    /// `ignore true`: not emitted; its pins keep their nets.
    True,
    /// `ignore open`: not emitted; its pins are left unconnected.
    Open,
    /// `ignore short`: not emitted; all its pin nets are one net.
    Short,
}

/// The connectivity of one cell. Component rows ("nets") are geometric; bit rows are
/// what a netlist connects. All names live in the arena passed to [`connect`].
#[derive(Clone, Debug, Default)]
pub struct CellNets {
    /// Component name: its emitted name expression (`d[3:0]`) or `net_<id>`.
    pub net_name: Vec<Name>,
    /// Bits the component carries.
    pub net_width: Vec<u16>,
    /// Whether any of its bits is global.
    pub net_global: Vec<bool>,
    /// Instance pins and ports touching it (the plugin `Nets.pin_count`).
    pub net_pins: Vec<u32>,
    /// CSR offsets (`nets + 1`) into [`CellNets::net_bits`].
    pub net_bit_start: Vec<u32>,
    /// Bit net of each component bit.
    pub net_bits: Vec<u32>,
    /// Component of each wire row.
    pub wire_net: Vec<u32>,
    /// Component of each label row.
    pub label_net: Vec<u32>,
    /// Component of each port row.
    pub port_net: Vec<u32>,
    /// CSR offsets (`instances + 1`) into [`CellNets::pin_net`].
    pub pin_start: Vec<u32>,
    /// Component of each ordinary pin of each instance (interface pin order, inherited
    /// pins skipped); unresolved instances have none.
    pub pin_net: Vec<u32>,

    /// Emitted bit name (`d[3]`, `vdd!`, `net_0abcde`).
    pub bit_name: Vec<Name>,
    /// Whether the bit is a global net (§3).
    pub bit_global: Vec<bool>,
    /// Connections: instance pin bits, port bits and inherit overrides on the bit.
    pub bit_conns: Vec<u32>,
    /// Every name that refers to a bit (emitted or merged by name), sorted by `Name`.
    pub by_name: Vec<(Name, u32)>,

    /// CSR offsets (`instances + 1`) into [`CellNets::elem_name`].
    pub elem_start: Vec<u32>,
    /// Element names of each instance (`X1[3]` … for `X1[3:0]`, else the name).
    pub elem_name: Vec<Name>,
    /// Offset of each instance's connections in [`CellNets::conns`]; element `k`, pin
    /// bit `b` (ordinary pins, `Iface::pin_off`) is at `start + k * bits_total + b`.
    pub conn_start: Vec<u32>,
    /// Bit net per connection, or [`NO_NET`].
    pub conns: Vec<u32>,
    /// `ignore` mode per instance.
    pub ignore: Vec<Ignore>,

    /// `inherit <pin> <net>` overrides: owning instance row.
    pub inh_inst: Vec<u32>,
    /// Overridden pin name.
    pub inh_pin: Vec<Name>,
    /// CSR offsets (`overrides + 1`) into [`CellNets::inh_bits`].
    pub inh_bit_start: Vec<u32>,
    /// Bit nets of the override net, in written order.
    pub inh_bits: Vec<u32>,

    /// CSR offsets (`own pins + 1`) into [`CellNets::port_bits`]: the bit nets each pin
    /// of the cell's own interface binds to (ordinary and inherited pins).
    pub port_bit_start: Vec<u32>,
    /// Bit nets of the own pins, in pin bit order.
    pub port_bits: Vec<u32>,
}

impl CellNets {
    /// Returns the bit net named `name`, if any net carries it.
    pub fn bit(&self, name: Name) -> Option<u32> {
        let i = self.by_name.binary_search_by_key(&name, |p| p.0).ok()?;
        Some(self.by_name[i].1)
    }

    /// Returns the element names of instance row `i`.
    pub fn elems(&self, i: usize) -> &[Name] {
        &self.elem_name[self.elem_start[i] as usize..self.elem_start[i + 1] as usize]
    }

    /// Returns the bit nets of own pin `p`.
    pub fn port(&self, p: usize) -> &[u32] {
        &self.port_bits[self.port_bit_start[p] as usize..self.port_bit_start[p + 1] as usize]
    }

    /// Returns the components of instance row `i`'s ordinary pins.
    pub fn pins(&self, i: usize) -> &[u32] {
        &self.pin_net[self.pin_start[i] as usize..self.pin_start[i + 1] as usize]
    }

    /// Returns the bit nets of component `c`.
    pub fn bits_of(&self, c: usize) -> &[u32] {
        &self.net_bits[self.net_bit_start[c] as usize..self.net_bit_start[c + 1] as usize]
    }
}

/// Places a symbol-local point (format spec §3): mirror x, rotate clockwise by quarter
/// turns (y down), translate. Saturates at the i32 range.
pub fn place(x: i32, y: i32, mirrored: bool, turns: u8, at_x: i32, at_y: i32) -> (i32, i32) {
    assert!(turns < 4);
    let (mut x, y) = (i64::from(x), i64::from(y));
    if mirrored {
        x = -x;
    }
    let (rx, ry) = match turns {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    let sat = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    (sat(rx + i64::from(at_x)), sat(ry + i64::from(at_y)))
}

/// Name classes in precedence order (§3): globals win, then ports, then labels.
const GLOBAL: u8 = 0;
const PORT: u8 = 1;
const LABEL: u8 = 2;

fn key(a: i32, b: i32) -> u64 {
    (u64::from(a as u32 ^ 0x8000_0000) << 32) | u64::from(b as u32 ^ 0x8000_0000)
}

pub(crate) fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[parent[x as usize] as usize];
        parent[x as usize] = p;
        x = p;
    }
    x
}

pub(crate) fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    match ra.cmp(&rb) {
        Ordering::Less => parent[rb as usize] = ra,
        Ordering::Greater => parent[ra as usize] = rb,
        Ordering::Equal => {}
    }
}

/// Compares ids by their text (bytewise), the order `net_<id>` names use.
pub(crate) fn id_cmp(a: ObjId, b: ObjId, s: &Strings) -> Ordering {
    if !a.is_alias() && !b.is_alias() {
        return a.0.cmp(&b.0); // 6-char ids order like their text
    }
    a.text(s).cmp(&b.text(s))
}

/// Appends the bit names of `doc`'s labels marked `global true` (netlist spec §3):
/// they are global in the whole elaborated hierarchy.
pub fn global_labels(doc: &Document, names: &mut Names, out: &mut Vec<Name>) {
    let (s, labels) = (doc.strings(), doc.labels());
    let mut buf = String::new();
    for l in 0..labels.id.len() {
        let flag = doc.pool().find_attr(s, labels.attrs[l], "global");
        if flag
            .and_then(|v| v.first())
            .is_some_and(|v| s.get(*v) == "true")
        {
            // An over-wide expression is reported by `connect` as E011.
            let _ = netexpr::expand(s.get(labels.text[l]), names, out, &mut buf);
        }
    }
}

/// Builds the connectivity of `doc`. `own` is the cell's own interface (ports and
/// inherited pins bind to its pins; a bench passes an interface without pins);
/// `inst_iface[i]` indexes `ifaces` for instance row `i`, or is `u32::MAX` when the
/// instance's cell did not resolve (it then has no pins). `globals` (sorted) are the
/// names made global by `global true` labels anywhere in the hierarchy (see
/// [`global_labels`]); `0` and `…!` are global by spelling. Findings (`E003`, `E009`,
/// `E011`, `E012`, `E002`, `W004`, `W010`, `W016`) are appended to `out`.
///
/// Cost: O(n log n) in nodes plus the nodes lying on wires.
///
/// # Panics
///
/// Panics if `inst_iface.len()` differs from the instance count or `globals` is not
/// sorted.
pub fn connect(
    doc: &Document,
    own: &Iface,
    ifaces: &[Iface],
    inst_iface: &[u32],
    globals: &[Name],
    names: &mut Names,
    out: &mut Vec<Violation>,
) -> CellNets {
    assert!(
        globals.windows(2).all(|w| w[0] <= w[1]),
        "globals are sorted"
    );
    let glob =
        |n: Name, names: &Names| is_global(names.get(n)) || globals.binary_search(&n).is_ok();
    let s = doc.strings();
    let (inst, wires, labels, ports, noconns) = (
        doc.instances(),
        doc.wires(),
        doc.labels(),
        doc.ports(),
        doc.noconns(),
    );
    let ni = inst.id.len();
    assert_eq!(inst_iface.len(), ni, "one interface slot per instance");
    let finding = |code, object, param, detail: String| Violation {
        code,
        cell: own.cell,
        object,
        path: Name::NONE,
        param,
        detail,
    };
    let mut cn = CellNets::default();
    let mut buf = String::new();

    // ---- instance elements and ignore modes
    cn.elem_start.push(0);
    for i in 0..ni {
        let text = s.get(inst.name[i]);
        if netexpr::expand(text, names, &mut cn.elem_name, &mut buf).is_err() {
            out.push(finding(
                Code::E011,
                inst.id[i],
                Name::NONE,
                format!("instance name `{text}` is wider than 65535"),
            ));
            cn.elem_name.push(names.intern(text));
        }
        cn.elem_start.push(cn.elem_name.len() as u32);
        let mode = doc
            .pool()
            .find_attr(s, inst.attrs[i], "ignore")
            .and_then(|v| v.first());
        cn.ignore.push(match mode.map(|v| s.get(*v)) {
            Some("true") => Ignore::True,
            Some("open") => Ignore::Open,
            Some("short") => Ignore::Short,
            _ => Ignore::No,
        });
    }

    // ---- nodes: wire ends, instance pins, labels, ports, noconns
    let nw = wires.id.len();
    let mut px: Vec<i32> = Vec::with_capacity(2 * nw);
    let mut py: Vec<i32> = Vec::with_capacity(2 * nw);
    for w in 0..nw {
        px.extend([wires.x1[w], wires.x2[w]]);
        py.extend([wires.y1[w], wires.y2[w]]);
    }
    let pins0 = px.len();
    let mut pin_inst: Vec<u32> = Vec::new();
    let mut pin_pin: Vec<u16> = Vec::new();
    for i in 0..ni {
        let Some(f) = ifaces.get(inst_iface[i] as usize) else {
            continue;
        };
        let (m, t) = (inst.orient[i].mirrored(), inst.orient[i].quarter_turns());
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] != Name::NONE {
                continue; // inherited pins are not geometric (§3)
            }
            let (x, y) = place(f.pin_x[p], f.pin_y[p], m, t, inst.x[i], inst.y[i]);
            px.push(x);
            py.push(y);
            pin_inst.push(i as u32);
            pin_pin.push(p as u16);
        }
    }
    let labels0 = px.len();
    px.extend_from_slice(&labels.x);
    py.extend_from_slice(&labels.y);
    let ports0 = px.len();
    px.extend_from_slice(&ports.x);
    py.extend_from_slice(&ports.y);
    let noconns0 = px.len();
    px.extend_from_slice(&noconns.x);
    py.extend_from_slice(&noconns.y);
    let n = px.len();
    assert!(n < NONE as usize && py.len() == n);

    // ---- union-find over nodes
    let mut parent: Vec<u32> = (0..n as u32).collect();
    for w in 0..nw {
        union(&mut parent, 2 * w as u32, 2 * w as u32 + 1);
    }
    let mut xy: Vec<(u64, u32)> = (0..n).map(|i| (key(px[i], py[i]), i as u32)).collect();
    xy.sort_unstable();
    for pair in xy.windows(2) {
        if pair[0].0 == pair[1].0 {
            union(&mut parent, pair[0].1, pair[1].1);
        }
    }
    let mut yx: Vec<(u64, u32)> = (0..n).map(|i| (key(py[i], px[i]), i as u32)).collect();
    yx.sort_unstable();
    for w in 0..nw {
        let (x1, y1, x2, y2) = (wires.x1[w], wires.y1[w], wires.x2[w], wires.y2[w]);
        let wn = 2 * w as u32;
        let (sorted, lo, hi) = if x1 == x2 {
            (&xy, key(x1, y1.min(y2)), key(x1, y1.max(y2)))
        } else if y1 == y2 {
            (&yx, key(y1, x1.min(x2)), key(y1, x1.max(x2)))
        } else {
            // ponytail: diagonal wires scan every node in their x-range, O(range) each;
            // a 2-D index if diagonal-heavy imports show up in profiles.
            let (xa, xb) = (x1.min(x2), x1.max(x2));
            let start = xy.partition_point(|e| e.0 < key(xa, i32::MIN));
            let end = xy.partition_point(|e| e.0 <= key(xb, i32::MAX));
            let (dx, dy) = (i64::from(x2) - i64::from(x1), i64::from(y2) - i64::from(y1));
            let (ya, yb) = (y1.min(y2), y1.max(y2));
            for &(_, node) in &xy[start..end] {
                let (x, y) = (px[node as usize], py[node as usize]);
                let cross =
                    (i64::from(x) - i64::from(x1)) * dy - (i64::from(y) - i64::from(y1)) * dx;
                if cross == 0 && (ya..=yb).contains(&y) {
                    union(&mut parent, node, wn);
                }
            }
            continue;
        };
        let start = sorted.partition_point(|e| e.0 < lo);
        for &(k, node) in &sorted[start..] {
            if k > hi {
                break;
            }
            union(&mut parent, node, wn);
        }
    }

    // ---- components, dense in node order
    let mut comp = vec![NONE; n];
    let mut ncomp = 0u32;
    let mut root_comp = vec![NONE; n];
    for i in 0..n {
        let r = find(&mut parent, i as u32) as usize;
        if root_comp[r] == NONE {
            root_comp[r] = ncomp;
            ncomp += 1;
        }
        comp[i] = root_comp[r];
    }
    drop(root_comp);
    let nc = ncomp as usize;
    let mut comp_nodes = vec![0u32; nc];
    let mut comp_pins = vec![0u32; nc];
    let mut comp_minid = vec![ObjId::NONE; nc];
    let mut comp_pinw = vec![1u32; nc];
    let mut better = |c: usize, id: ObjId| {
        if comp_minid[c] == ObjId::NONE || id_cmp(id, comp_minid[c], s) == Ordering::Less {
            comp_minid[c] = id;
        }
    };
    for i in 0..n {
        let c = comp[i] as usize;
        comp_nodes[c] += 1;
        if i < pins0 {
            better(c, wires.id[i / 2]);
        } else if i < labels0 {
            let k = i - pins0;
            let f = &ifaces[inst_iface[pin_inst[k] as usize] as usize];
            let p = pin_pin[k] as usize;
            comp_pins[c] += 1;
            comp_pinw[c] = comp_pinw[c].max(f.pin_bit_start[p + 1] - f.pin_bit_start[p]);
            better(c, inst.id[pin_inst[k] as usize]);
        } else if i < ports0 {
            better(c, labels.id[i - labels0]);
        } else if i < noconns0 {
            comp_pins[c] += 1;
            better(c, ports.id[i - ports0]);
        } else {
            better(c, noconns.id[i - noconns0]);
        }
    }

    // ---- names per component: (comp, class, bits range)
    let mut src_comp: Vec<u32> = Vec::new();
    let mut src_class: Vec<u8> = Vec::new();
    let mut src_text: Vec<Name> = Vec::new();
    let mut src_obj: Vec<ObjId> = Vec::new();
    let mut src_start: Vec<u32> = vec![0];
    let mut src_bits: Vec<Name> = Vec::new();
    let mut add_src =
        |c: u32, class: u8, text: &str, obj: ObjId, names: &mut Names, out: &mut Vec<Violation>| {
            let before = src_bits.len();
            if netexpr::expand(text, names, &mut src_bits, &mut buf).is_err() {
                out.push(finding(
                    Code::E011,
                    obj,
                    Name::NONE,
                    format!("`{text}` is wider than 65535 bits"),
                ));
                return;
            }
            let global = src_bits[before..].iter().all(|b| glob(*b, names));
            src_comp.push(c);
            src_class.push(if global { GLOBAL } else { class });
            src_text.push(names.intern(text));
            src_obj.push(obj);
            src_start.push(src_bits.len() as u32);
        };
    for l in 0..labels.id.len() {
        add_src(
            comp[labels0 + l],
            LABEL,
            s.get(labels.text[l]),
            labels.id[l],
            names,
            out,
        );
    }
    for p in 0..ports.id.len() {
        add_src(
            comp[ports0 + p],
            PORT,
            s.get(ports.text[p]),
            ports.id[p],
            names,
            out,
        );
    }
    let nsrc = src_comp.len();
    // Best source per component: (class, text) smallest.
    let mut comp_best = vec![NONE; nc];
    for k in 0..nsrc {
        let c = src_comp[k] as usize;
        let b = comp_best[c];
        if b == NONE
            || (src_class[k], names.get(src_text[k]))
                < (src_class[b as usize], names.get(src_text[b as usize]))
        {
            comp_best[c] = k as u32;
        }
    }
    let bits = |k: usize| &src_bits[src_start[k] as usize..src_start[k + 1] as usize];
    let mut comp_width = vec![0u32; nc];
    for c in 0..nc {
        comp_width[c] = match comp_best[c] {
            NONE => comp_pinw[c],
            b => bits(b as usize).len() as u32,
        };
    }
    // E003 (distinct names on one component) and E011 (names of unequal width).
    let mut usable = vec![true; nsrc];
    for k in 0..nsrc {
        let c = src_comp[k] as usize;
        let b = comp_best[c] as usize;
        if b == k || bits(k) == bits(b) {
            continue;
        }
        if bits(k).len() != bits(b).len() {
            usable[k] = false;
            let detail = format!(
                "`{}` ({} bits) and `{}` ({} bits) name one net",
                names.get(src_text[k]),
                bits(k).len(),
                names.get(src_text[b]),
                bits(b).len()
            );
            out.push(finding(Code::E011, src_obj[k], src_text[k], detail));
        } else {
            let detail = format!(
                "`{}` is shorted to `{}`",
                names.get(src_text[k]),
                names.get(src_text[b])
            );
            out.push(finding(Code::E003, src_obj[k], src_text[k], detail));
        }
    }

    // ---- bit slots: component bits, then name-only references
    let mut slot_start = vec![0u32; nc + 1];
    for c in 0..nc {
        slot_start[c + 1] = slot_start[c] + comp_width[c];
    }
    let mut nslots = slot_start[nc];
    let mut pairs: Vec<(Name, u32, u8)> = Vec::new(); // (bit name, slot, class)
    for k in 0..nsrc {
        if !usable[k] {
            continue;
        }
        let c = src_comp[k] as usize;
        let class = src_class[k];
        for (j, &b) in bits(k).iter().enumerate() {
            let class = if glob(b, names) { GLOBAL } else { class };
            pairs.push((b, slot_start[c] + j as u32, class));
        }
    }
    let mut name_slots = |text_bits: &[Name],
                          class: u8,
                          pairs: &mut Vec<(Name, u32, u8)>,
                          names: &Names|
     -> (u32, u32) {
        let first = nslots;
        for &b in text_bits {
            let class = if glob(b, names) { GLOBAL } else { class };
            pairs.push((b, nslots, class));
            nslots += 1;
        }
        (first, nslots)
    };
    // Own interface pins (ports and inherited pins bind by name).
    let mut port_slot_start = vec![0u32];
    let mut port_slots: Vec<u32> = Vec::new();
    for p in 0..own.pin_name.len() {
        let (a, b) = name_slots(own.bits(p), PORT, &mut pairs, names);
        port_slots.extend(a..b);
        port_slot_start.push(port_slots.len() as u32);
    }
    // Instance `inherit <pin> <net>` overrides.
    let mut inh_slot_start = vec![0u32];
    let mut inh_slots: Vec<u32> = Vec::new();
    let mut tmp: Vec<Name> = Vec::new();
    for i in 0..ni {
        for (k, vals) in doc.pool().attrs(inst.attrs[i]) {
            if s.get(k) != "inherit" || vals.len() != 2 {
                continue;
            }
            let (pin, net) = (s.get(vals[0]), s.get(vals[1]));
            tmp.clear();
            if netexpr::expand(net, names, &mut tmp, &mut buf).is_err() {
                out.push(finding(
                    Code::E011,
                    inst.id[i],
                    Name::NONE,
                    format!("`{net}` is wider than 65535 bits"),
                ));
                continue;
            }
            let pin_name = names.intern(pin);
            // A declared inherited pin checks its width here; a pin propagated from the
            // instance's subtree is checked where elaboration resolves it (§5).
            if let Some(f) = ifaces.get(inst_iface[i] as usize) {
                let declared = (0..f.pin_name.len())
                    .find(|&p| f.pin_name[p] == pin_name && f.pin_inherit[p] != Name::NONE);
                if let Some(p) = declared.filter(|&p| f.bits(p).len() != tmp.len()) {
                    let detail = format!(
                        "inherit `{pin}` ({} bits) set to `{net}` ({} bits)",
                        f.bits(p).len(),
                        tmp.len()
                    );
                    out.push(finding(Code::E011, inst.id[i], pin_name, detail));
                    continue;
                }
            }
            let (a, b) = name_slots(&tmp, LABEL, &mut pairs, names);
            cn.inh_inst.push(i as u32);
            cn.inh_pin.push(pin_name);
            inh_slots.extend(a..b);
            inh_slot_start.push(inh_slots.len() as u32);
        }
    }

    // ---- instance pin bits → slots (bus slicing, §4)
    let mut conn_slots: Vec<u32> = Vec::new();
    let mut pin_domain_slots: Vec<(u32, Domain)> = Vec::new();
    let mut k_node = pins0;
    for i in 0..ni {
        cn.conn_start.push(conn_slots.len() as u32);
        let Some(f) = ifaces.get(inst_iface[i] as usize) else {
            continue;
        };
        let elems = (cn.elem_start[i + 1] - cn.elem_start[i]) as usize;
        let base = conn_slots.len();
        conn_slots.resize(base + elems * f.bits_total as usize, NONE);
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] != Name::NONE {
                continue;
            }
            let c = comp[k_node] as usize;
            k_node += 1;
            let w = f.bits(p).len();
            let nwidth = comp_width[c] as usize;
            let sliced = if nwidth == w {
                false
            } else if nwidth == w * elems {
                true
            } else {
                let detail = format!(
                    "pin `{}` ({w} bits × {elems}) on a {nwidth}-bit net",
                    names.get(f.pin_name[p])
                );
                out.push(finding(Code::E011, inst.id[i], f.pin_name[p], detail));
                continue;
            };
            for e in 0..elems {
                for j in 0..w {
                    let bit = if sliced { e * w + j } else { j };
                    let slot = slot_start[c] + bit as u32;
                    conn_slots[base + e * f.bits_total as usize + f.pin_off[p] as usize + j] = slot;
                    pin_domain_slots.push((slot, f.pin_domain[p]));
                }
            }
        }
    }
    cn.conn_start.push(conn_slots.len() as u32);
    assert_eq!(k_node, labels0, "every pin node consumed in order");

    // ---- merge slots: equal names, then `ignore short`
    let mut sp: Vec<u32> = (0..nslots).collect();
    pairs.sort_unstable_by_key(|p| (p.0, p.1));
    for w in pairs.windows(2) {
        if w[0].0 == w[1].0 {
            union(&mut sp, w[0].1, w[1].1);
        }
    }
    for i in 0..ni {
        if cn.ignore[i] != Ignore::Short {
            continue;
        }
        let r = cn.conn_start[i] as usize..cn.conn_start[i + 1] as usize;
        let mut first = NONE;
        for &slot in &conn_slots[r] {
            if slot == NONE {
                continue;
            }
            if first == NONE {
                first = slot;
            } else {
                union(&mut sp, first, slot);
            }
        }
    }
    let mut net_of = vec![NONE; nslots as usize];
    let mut nbits = 0u32;
    let mut root_net = vec![NONE; nslots as usize];
    for slot in 0..nslots as usize {
        let r = find(&mut sp, slot as u32) as usize;
        if root_net[r] == NONE {
            root_net[r] = nbits;
            nbits += 1;
        }
        net_of[slot] = root_net[r];
    }
    drop(root_net);

    // ---- bit names: best named pair, else net_<smallest id>[k]
    let nb = nbits as usize;
    let mut best: Vec<(u8, Name)> = vec![(u8::MAX, Name::NONE); nb];
    for &(name, slot, class) in &pairs {
        let b = net_of[slot as usize] as usize;
        let (bc, bn) = best[b];
        if class < bc || (class == bc && names.cmp(name, bn) == Ordering::Less) {
            best[b] = (class, name);
        }
    }
    // Unnamed bits: smallest (component id, bit index) among the slots.
    let mut unnamed: Vec<(ObjId, u32, u32)> = vec![(ObjId::NONE, 0, 1); nb];
    for c in 0..nc {
        for j in 0..comp_width[c] {
            let b = net_of[(slot_start[c] + j) as usize] as usize;
            if best[b].1 != Name::NONE {
                continue;
            }
            let (id, k, _) = unnamed[b];
            let id_c = comp_minid[c];
            let take = id == ObjId::NONE
                || match id_cmp(id_c, id, s) {
                    Ordering::Less => true,
                    Ordering::Equal => j < k,
                    Ordering::Greater => false,
                };
            if take {
                unnamed[b] = (id_c, j, comp_width[c]);
            }
        }
    }
    cn.bit_name.reserve(nb);
    for b in 0..nb {
        let name = match best[b] {
            (_, n) if n != Name::NONE => n,
            _ => {
                let (id, k, width) = unnamed[b];
                assert!(
                    id != ObjId::NONE,
                    "every component holds an object with an id"
                );
                buf.clear();
                buf.push_str("net_");
                id.write(s, &mut buf);
                if width > 1 {
                    buf.push('[');
                    buf.push_str(&k.to_string());
                    buf.push(']');
                }
                names.intern(&buf)
            }
        };
        cn.bit_name.push(name);
        cn.bit_global.push(glob(name, names));
    }
    // All pairs with one name were merged above, so one net per name.
    cn.by_name = pairs.iter().map(|p| (p.0, net_of[p.1 as usize])).collect();
    cn.by_name.sort_unstable();
    cn.by_name.dedup_by_key(|p| p.0);
    debug_assert!(cn.by_name.windows(2).all(|w| w[0].0 < w[1].0));

    // ---- connections, ports, overrides as bit nets
    let to_net = |slot: u32| {
        if slot == NONE {
            NO_NET
        } else {
            net_of[slot as usize]
        }
    };
    cn.conns = conn_slots.iter().map(|&sl| to_net(sl)).collect();
    cn.port_bit_start = port_slot_start;
    cn.port_bits = port_slots.iter().map(|&sl| to_net(sl)).collect();
    cn.inh_bit_start = inh_slot_start;
    cn.inh_bits = inh_slots.iter().map(|&sl| to_net(sl)).collect();
    for i in 0..ni {
        if cn.ignore[i] == Ignore::Open {
            let r = cn.conn_start[i] as usize..cn.conn_start[i + 1] as usize;
            cn.conns[r].fill(NO_NET);
        }
    }

    // ---- component rows
    cn.net_bit_start.push(0);
    for c in 0..nc {
        cn.net_name.push(match comp_best[c] {
            NONE => {
                let b = net_of[slot_start[c] as usize];
                if comp_width[c] == 1 {
                    cn.bit_name[b as usize]
                } else {
                    buf.clear();
                    buf.push_str("net_");
                    comp_minid[c].write(s, &mut buf);
                    names.intern(&buf)
                }
            }
            k => src_text[k as usize],
        });
        cn.net_width
            .push(comp_width[c].min(u32::from(u16::MAX)) as u16);
        cn.net_pins.push(comp_pins[c]);
        let r = slot_start[c]..slot_start[c + 1];
        cn.net_bits.extend(r.map(|sl| net_of[sl as usize]));
        cn.net_bit_start.push(cn.net_bits.len() as u32);
        let global = cn.bits_of(c).iter().any(|&b| cn.bit_global[b as usize]);
        cn.net_global.push(global);
    }
    cn.wire_net = (0..nw).map(|w| comp[2 * w]).collect();
    cn.label_net = (0..labels.id.len()).map(|l| comp[labels0 + l]).collect();
    cn.port_net = (0..ports.id.len()).map(|p| comp[ports0 + p]).collect();
    cn.pin_net = comp[pins0..labels0].to_vec();
    cn.pin_start = Vec::with_capacity(ni + 1);
    cn.pin_start.push(0);
    let mut k = 0usize;
    for i in 0..ni {
        while k < pin_inst.len() && pin_inst[k] as usize == i {
            k += 1;
        }
        cn.pin_start.push(k as u32);
    }
    assert_eq!(k, cn.pin_net.len());

    // ---- connection counts and domains per bit
    cn.bit_conns = vec![0; nb];
    for &b in cn.conns.iter().chain(&cn.port_bits).chain(&cn.inh_bits) {
        if b != NO_NET {
            cn.bit_conns[b as usize] += 1;
        }
    }
    let mut dom = vec![0u8; nb];
    for &(slot, d) in &pin_domain_slots {
        dom[net_of[slot as usize] as usize] |= 1 << d as u8;
    }

    // ---- findings
    for k in 0..(labels0 - pins0) {
        if comp_nodes[comp[pins0 + k] as usize] == 1 {
            let i = pin_inst[k] as usize;
            let f = &ifaces[inst_iface[i] as usize];
            let pin = f.pin_name[pin_pin[k] as usize];
            out.push(finding(
                Code::W004,
                inst.id[i],
                pin,
                format!("pin `{}` is unconnected", names.get(pin)),
            ));
        }
    }
    for b in 0..nb {
        if cn.bit_conns[b] == 1 && !cn.bit_global[b] {
            let name = cn.bit_name[b];
            out.push(finding(
                Code::W016,
                ObjId::NONE,
                name,
                format!("net `{}` has a single connection", names.get(name)),
            ));
        }
        let real = dom[b] & (1 << Domain::Real as u8) != 0;
        if real && dom[b] & !(1 << Domain::Real as u8) != 0 {
            let name = cn.bit_name[b];
            out.push(finding(
                Code::W010,
                ObjId::NONE,
                name,
                format!(
                    "net `{}` joins real and analog/digital pins",
                    names.get(name)
                ),
            ));
        }
    }
    // E009: duplicate element names.
    let mut elems: Vec<(Name, ObjId)> = Vec::with_capacity(cn.elem_name.len());
    for i in 0..ni {
        elems.extend(cn.elems(i).iter().map(|&e| (e, inst.id[i])));
    }
    elems.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(id_cmp(a.1, b.1, s)));
    for w in elems.windows(2) {
        if w[0].0 == w[1].0 {
            let detail = format!("instance name `{}` is used twice", names.get(w[1].0));
            out.push(finding(Code::E009, w[1].1, w[1].0, detail));
        }
    }
    // E012: ports ↔ own interface pins; E002 once when the schematic impl breaks it.
    if !own.impl_name.is_empty() || !ports.id.is_empty() {
        let before = out.len();
        for p in 0..ports.id.len() {
            let text = names.find(s.get(ports.text[p]));
            let known = text.is_some_and(|t| own.pin_name.contains(&t));
            if !known {
                let detail = format!("port `{}` names no interface pin", s.get(ports.text[p]));
                out.push(finding(
                    Code::E012,
                    ports.id[p],
                    text.unwrap_or(Name::NONE),
                    detail,
                ));
            }
        }
        for p in 0..own.pin_name.len() {
            let has = (0..ports.id.len())
                .any(|q| names.find(s.get(ports.text[q])) == Some(own.pin_name[p]));
            if !has && own.pin_inherit[p] == Name::NONE {
                let detail = format!("interface pin `{}` has no port", names.get(own.pin_name[p]));
                out.push(finding(Code::E012, ObjId::NONE, own.pin_name[p], detail));
            }
        }
        if let Some(si) = own.schematic_impl(names).filter(|_| out.len() > before) {
            let detail = format!(
                "schematic impl `{}` ports differ from the symbol pins",
                names.get(own.impl_name[si])
            );
            out.push(finding(Code::E002, ObjId::NONE, own.impl_name[si], detail));
        }
    }
    assert_eq!(cn.net_name.len(), nc);
    assert_eq!(cn.bit_name.len(), nb);
    assert_eq!(cn.conn_start.len(), ni + 1);
    cn
}
