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
///
/// [`connect`] numbers components and bits densely. The editor's [`crate::CellState`]
/// patches them per edit instead: component and bit handles are then stable (an edit keeps
/// every net it does not touch, handle and name) and freed rows are holes until reused. A
/// hole component has width 0, no bits and the empty name; a hole bit has
/// [`Name::NONE`] as its name and is on no component.
#[derive(Clone, Debug, Default)]
pub struct CellNets {
    /// Component name: its emitted name expression (`d[3:0]`) or `net_<id>`.
    pub net_name: Vec<Name>,
    /// Bits the component carries (0 for a hole).
    pub net_width: Vec<u16>,
    /// Whether any of its bits is global.
    pub net_global: Vec<bool>,
    /// Instance pins and ports touching it (the plugin `Nets.pin_count`).
    pub net_pins: Vec<u32>,
    /// Offset of each component's bits in [`CellNets::net_bits`]; it has
    /// [`CellNets::net_width`] of them.
    pub net_bit_start: Vec<u32>,
    /// Bit net of each component bit (patched components append theirs: unreferenced
    /// entries are garbage until the next full build).
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

    /// Returns the bit nets of component `c` (empty for a hole).
    pub fn bits_of(&self, c: usize) -> &[u32] {
        let start = self.net_bit_start[c] as usize;
        &self.net_bits[start..start + usize::from(self.net_width[c])]
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
pub(crate) const GLOBAL: u8 = 0;
pub(crate) const PORT: u8 = 1;
pub(crate) const LABEL: u8 = 2;

/// Whether bit name `n` is global: by spelling (`0`, `…!`) or made so by a `global true`
/// label (`globals`, sorted).
pub(crate) fn glob(globals: &[Name], names: &Names, n: Name) -> bool {
    is_global(names.get(n)) || globals.binary_search(&n).is_ok()
}

/// Bus slicing (§4): whether a `w`-bit pin of an `elems`-element instance takes its whole
/// `net`-bit net (`Ok(false)`) or one `w`-bit slice per element (`Ok(true)`).
///
/// # Errors
///
/// The `E011` detail when the net is neither width.
pub(crate) fn slicing(pin: &str, w: usize, elems: usize, net: u32) -> Result<bool, String> {
    let net = net as usize;
    if net == w {
        Ok(false)
    } else if net == w * elems {
        Ok(true)
    } else {
        Err(format!(
            "pin `{pin}` ({w} bits × {elems}) on a {net}-bit net"
        ))
    }
}

/// Whether `(x, y)` lies on the segment `(x1, y1)`–`(x2, y2)` (ends included). Exact over
/// the whole i32 plane: each cross-product term reaches 2^64, so it is computed in i128.
pub(crate) fn on_segment(x: i32, y: i32, x1: i32, y1: i32, x2: i32, y2: i32) -> bool {
    let (dx, dy) = (
        i128::from(x2) - i128::from(x1),
        i128::from(y2) - i128::from(y1),
    );
    let cross = (i128::from(x) - i128::from(x1)) * dy - (i128::from(y) - i128::from(y1)) * dx;
    cross == 0 && (x1.min(x2)..=x1.max(x2)).contains(&x) && (y1.min(y2)..=y1.max(y2)).contains(&y)
}

/// Appends `v` in decimal (what `to_string` writes, without its allocation: generated net
/// names are built once per unnamed bit).
pub(crate) fn push_decimal(mut v: u32, out: &mut String) {
    let mut digits = [0u8; 10];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend(digits[..n].iter().rev().map(|&d| char::from(d)));
}

fn key(a: i32, b: i32) -> u64 {
    (u64::from(a as u32 ^ 0x8000_0000) << 32) | u64::from(b as u32 ^ 0x8000_0000)
}

/// `(key(a, b), i)` for every node `i`, sorted: what `sort_unstable` gives, in about a third
/// of its time at 10^6 nodes (the two node sorts were a quarter of a flat 1M-instance
/// elaboration).
fn sorted_nodes(a: &[i32], b: &[i32]) -> Vec<(u64, u32)> {
    assert_eq!(a.len(), b.len());
    let v: Vec<(u64, u32)> = (0..a.len()).map(|i| (key(a[i], b[i]), i as u32)).collect();
    radix_sort(v)
}

/// Sorts `(key, index)` pairs whose indices are ascending (so ties keep index order, as a
/// `(key, index)` sort would): LSD radix on the key for large inputs.
pub(crate) fn radix_sort(mut v: Vec<(u64, u32)>) -> Vec<(u64, u32)> {
    if v.len() < 1 << 12 {
        v.sort_unstable();
        return v;
    }
    // LSD radix on the key, 11-bit digits. Stable, and the indices start ascending, so
    // ties stay in index order as in a `(key, index)` sort. A digit every key shares
    // (the high bits of a narrow coordinate range) is skipped.
    const BITS: u32 = 11;
    const DIGITS: usize = 64usize.div_ceil(BITS as usize);
    let digit = |k: u64, d: usize| ((k >> (d as u32 * BITS)) & ((1 << BITS) - 1)) as usize;
    let mut counts = vec![[0u32; 1 << BITS]; DIGITS];
    for &(k, _) in &v {
        for (d, c) in counts.iter_mut().enumerate() {
            c[digit(k, d)] += 1;
        }
    }
    let mut tmp = vec![(0u64, 0u32); v.len()];
    for (d, c) in counts.iter_mut().enumerate() {
        if c.iter().any(|&n| n as usize == v.len()) {
            continue;
        }
        let mut sum = 0;
        for n in c.iter_mut() {
            (*n, sum) = (sum, sum + *n);
        }
        for &e in &v {
            let slot = &mut c[digit(e.0, d)];
            tmp[*slot as usize] = e;
            *slot += 1;
        }
        std::mem::swap(&mut v, &mut tmp);
    }
    debug_assert!(v.windows(2).all(|w| w[0] < w[1]));
    v
}

pub(crate) fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[parent[x as usize] as usize];
        parent[x as usize] = p;
        x = p;
    }
    x
}

/// Joins the sets of `a` and `b`, keeping the smaller root: a set's root is its smallest
/// element (`connect` numbers sets by meeting roots first).
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
    let global_key = s.find("global");
    for l in 0..labels.id.len() {
        let flag = global_key.and_then(|key| {
            let mut attrs = doc.pool().attrs(labels.attrs[l]);
            attrs.find(|(k, _)| *k == key).map(|(_, v)| v)
        });
        if flag
            .and_then(|v| v.first())
            .is_some_and(|v| s.get(*v) == "true")
        {
            // An over-wide expression is reported by `connect` as E011.
            let _ = netexpr::expand(s.get(labels.text[l]), names, out, &mut buf);
        }
    }
}

/// Owner of a finding that no component or bit owns: instance names (`E011`, `E009`) and
/// `inherit` overrides (`E011`); they change only with what forces a full build.
pub(crate) const OWN_STATIC: u32 = u32::MAX;
/// Owner of the port ↔ interface pin findings (`E012`, `E002`).
pub(crate) const OWN_PORTS: u32 = u32::MAX - 1;
/// Owner of the bit findings (`W016`, `W010`), recomputed from each bit's counts.
pub(crate) const OWN_BITS: u32 = u32::MAX - 2;

/// What the full pass knows that the editor's incremental connectivity (`live`) starts
/// from: the nodes in [`connect`]'s order (wire ends, instance pins, labels, ports,
/// noconns) with their components, the two node sorts, and who owns each finding.
#[derive(Debug, Default)]
pub(crate) struct Seed {
    pub(crate) px: Vec<i32>,
    pub(crate) py: Vec<i32>,
    /// Component of each node.
    pub(crate) comp: Vec<u32>,
    /// First node of the instance pins, labels, ports and noconns.
    pub(crate) pins0: usize,
    pub(crate) labels0: usize,
    pub(crate) ports0: usize,
    pub(crate) noconns0: usize,
    /// Instance row and interface pin index of each pin node.
    pub(crate) pin_inst: Vec<u32>,
    pub(crate) pin_pin: Vec<u16>,
    /// `(key(x, y), node)` and `(key(y, x), node)`, sorted (`yx` empty without wires).
    pub(crate) xy: Vec<(u64, u32)>,
    pub(crate) yx: Vec<(u64, u32)>,
    /// Whether each component is named by a label or port.
    pub(crate) named: Vec<bool>,
    /// Owner of each finding appended to `out`: a component, or `OWN_*`.
    pub(crate) owner: Vec<u32>,
    /// Instance pin bits of each bit net in the `real` domain and in the others (`W010`).
    pub(crate) real: Vec<u32>,
    pub(crate) other: Vec<u32>,
    /// Whether names merge components beyond one name per bit (`E003` shorts, `ignore
    /// short`): then nothing is patched per edit.
    pub(crate) merged: bool,
}

/// Findings with their owners (what `live` drops when the owner changes).
struct Found<'a> {
    out: &'a mut Vec<Violation>,
    owner: Vec<u32>,
}

impl Found<'_> {
    fn push(&mut self, owner: u32, v: Violation) {
        self.out.push(v);
        self.owner.push(owner);
    }
}

/// A component key's pin index when the key is an owned object id.
pub(crate) const NONE_PIN: u32 = u32::MAX;

/// Appends `net_<id>` (an owned object id) or `net_<inst id>_<pin>` (a pin node): the
/// generated name of an unnamed component keyed `k` (§3).
pub(crate) fn write_key(k: (ObjId, u32), s: &Strings, buf: &mut String) {
    buf.push_str("net_");
    k.0.write(s, buf);
    if k.1 != NONE_PIN {
        buf.push('_');
        push_decimal(k.1, buf);
    }
}

/// The geometric half of `connect`: nodes, their two sorts and their components.
struct Geometry {
    px: Vec<i32>,
    py: Vec<i32>,
    pin_inst: Vec<u32>,
    pin_pin: Vec<u16>,
    pins0: usize,
    labels0: usize,
    ports0: usize,
    noconns0: usize,
    xy: Vec<(u64, u32)>,
    yx: Vec<(u64, u32)>,
    comp: Vec<u32>,
    nc: usize,
}

/// Element names of every instance of `doc` (`X1[3]` … for `X1[3:0]`, else the name):
/// `(CSR starts, names, E011 findings of cell `cell`)`. Plain names (nearly all) are
/// interned in batches (`Names::intern_batch`: 10^6 new names in a flat design); a bus
/// name flushes the batch first, so every name gets the number one-by-one interning gives.
fn element_names(
    doc: &Document,
    names: &mut Names,
    cell: Name,
) -> (Vec<u32>, Vec<Name>, Vec<Violation>) {
    let (s, inst) = (doc.strings(), doc.instances());
    let ni = inst.id.len();
    let (mut start, mut elems, mut found) = (Vec::with_capacity(ni + 1), Vec::new(), Vec::new());
    elems.reserve(ni);
    start.push(0);
    let (mut plain, mut plain_at, mut got): (Vec<&str>, Vec<usize>, Vec<Name>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut flush =
        |names: &mut Names, plain: &mut Vec<&str>, at: &mut Vec<usize>, elems: &mut Vec<Name>| {
            got.clear();
            names.intern_batch(plain, &mut got);
            for (k, &e) in at.iter().enumerate() {
                elems[e] = got[k];
            }
            plain.clear();
            at.clear();
        };
    let mut buf = String::new();
    for i in 0..ni {
        let text = s.get(inst.name[i]);
        if netexpr::is_plain(text) {
            plain_at.push(elems.len());
            elems.push(Name::NONE);
            plain.push(text);
        } else {
            flush(names, &mut plain, &mut plain_at, &mut elems);
            if netexpr::expand(text, names, &mut elems, &mut buf).is_err() {
                found.push(Violation {
                    code: Code::E011,
                    cell,
                    object: inst.id[i],
                    path: Name::NONE,
                    param: Name::NONE,
                    detail: format!("instance name `{text}` is wider than 65535"),
                });
                elems.push(names.intern(text));
            }
        }
        start.push(elems.len() as u32);
    }
    flush(names, &mut plain, &mut plain_at, &mut elems);
    assert!(
        elems.iter().all(|&e| e != Name::NONE),
        "every element named"
    );
    assert_eq!(start.len(), ni + 1);
    (start, elems, found)
}

/// Decodes [`key`]: `(a, b)`.
pub(crate) fn unkey(k: u64) -> (i32, i32) {
    (
        ((k >> 32) as u32 ^ 0x8000_0000) as i32,
        (k as u32 ^ 0x8000_0000) as i32,
    )
}

/// `W016` and `W010` of bit `b` named `name` (the bit-owned findings).
#[allow(clippy::too_many_arguments)]
pub(crate) fn bit_findings(
    cell: Name,
    name: Name,
    global: bool,
    conns: u32,
    real: u32,
    other: u32,
    names: &Names,
    out: &mut Vec<Violation>,
) {
    let row = |code, detail| Violation {
        code,
        cell,
        object: ObjId::NONE,
        path: Name::NONE,
        param: name,
        detail,
    };
    if conns == 1 && !global {
        let detail = format!("net `{}` has a single connection", names.get(name));
        out.push(row(Code::W016, detail));
    }
    if real > 0 && other > 0 {
        let detail = format!(
            "net `{}` joins real and analog/digital pins",
            names.get(name)
        );
        out.push(row(Code::W010, detail));
    }
}

/// `E012` (ports ↔ own interface pins) and `E002` once when the schematic impl breaks it.
pub(crate) fn port_findings(doc: &Document, own: &Iface, names: &Names, out: &mut Vec<Violation>) {
    let (s, ports) = (doc.strings(), doc.ports());
    if own.impl_name.is_empty() && ports.id.is_empty() {
        return;
    }
    let row = |code, object, param, detail| Violation {
        code,
        cell: own.cell,
        object,
        path: Name::NONE,
        param,
        detail,
    };
    let before = out.len();
    for p in 0..ports.id.len() {
        let text = names.find(s.get(ports.text[p]));
        let known = text.is_some_and(|t| own.pin_name.contains(&t));
        if !known {
            let detail = format!("port `{}` names no interface pin", s.get(ports.text[p]));
            out.push(row(
                Code::E012,
                ports.id[p],
                text.unwrap_or(Name::NONE),
                detail,
            ));
        }
    }
    for p in 0..own.pin_name.len() {
        let has =
            (0..ports.id.len()).any(|q| names.find(s.get(ports.text[q])) == Some(own.pin_name[p]));
        if !has && own.pin_inherit[p] == Name::NONE && own.pin_alias[p] == NONE {
            let detail = format!("interface pin `{}` has no port", names.get(own.pin_name[p]));
            out.push(row(Code::E012, ObjId::NONE, own.pin_name[p], detail));
        }
    }
    if let Some(si) = own.schematic_impl(names).filter(|_| out.len() > before) {
        let detail = format!(
            "schematic impl `{}` ports differ from the symbol pins",
            names.get(own.impl_name[si])
        );
        out.push(row(Code::E002, ObjId::NONE, own.impl_name[si], detail));
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
    connect_seeded(doc, own, ifaces, inst_iface, globals, names, out, None)
}

/// [`connect`], also filling `seed` (the editor's incremental start) when given.
#[allow(clippy::too_many_arguments)]
pub(crate) fn connect_seeded(
    doc: &Document,
    own: &Iface,
    ifaces: &[Iface],
    inst_iface: &[u32],
    globals: &[Name],
    names: &mut Names,
    out: &mut Vec<Violation>,
    seed: Option<&mut Seed>,
) -> CellNets {
    assert!(
        globals.windows(2).all(|w| w[0] <= w[1]),
        "globals are sorted"
    );
    let mut out = Found {
        out,
        owner: Vec::new(),
    };
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
    let mut merged = false;
    let mut buf = String::new();
    // Attribute keys looked up once (a key the document never interned is on no row).
    let (ignore_key, inherit_key) = (s.find("ignore"), s.find("inherit"));

    // ---- instance elements (on a second thread in a large cell: they are the only names
    // interned before the components exist) and ignore modes; geometry → union-find
    let nw = wires.id.len();
    let pins: usize = (0..ni)
        .filter_map(|i| ifaces.get(inst_iface[i] as usize))
        .map(|f| f.pin_inherit.iter().filter(|p| **p == Name::NONE).count())
        .sum();
    let nodes = 2 * nw + pins + labels.id.len() + ports.id.len() + noconns.id.len();
    // Element names, then a generated name per unnamed component: at most one per two
    // nodes in practice (a component holds a wire's two ends, or two pins), so the table
    // grows once; a bigger fresh table only costs page faults.
    names.reserve(ni + nodes / 2);
    let geometry = || {
        // ---- nodes: wire ends, instance pins, labels, ports, noconns
        let mut px: Vec<i32> = Vec::with_capacity(2 * nw);
        let mut py: Vec<i32> = Vec::with_capacity(2 * nw);
        for w in 0..nw {
            px.extend([wires.x1[w], wires.x2[w]]);
            py.extend([wires.y1[w], wires.y2[w]]);
        }
        let pins0 = px.len();
        let mut pin_inst: Vec<u32> = Vec::new();
        let mut pin_pin: Vec<u16> = Vec::new();
        // (alias pin node, target pin node): one node, joined after the union-find starts.
        let mut aliased: Vec<(u32, u32)> = Vec::new();
        for i in 0..ni {
            let Some(f) = ifaces.get(inst_iface[i] as usize) else {
                continue;
            };
            let (m, t) = (inst.orient[i].mirrored(), inst.orient[i].quarter_turns());
            let first = px.len();
            for p in 0..f.pin_name.len() {
                if f.pin_inherit[p] != Name::NONE {
                    continue; // inherited pins are not geometric (§3)
                }
                let q = f.pin_alias[p];
                if q != NONE {
                    // Targets precede their aliases and are ordinary: find its node.
                    let at = (first..px.len()).find(|&k| pin_pin[k - pins0] == q as u16);
                    aliased.extend(at.map(|k| (px.len() as u32, k as u32)));
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
        for &(a, b) in &aliased {
            union(&mut parent, a, b);
        }
        let xy = sorted_nodes(&px, &py);
        for pair in xy.windows(2) {
            if pair[0].0 == pair[1].0 {
                union(&mut parent, pair[0].1, pair[1].1);
            }
        }
        // Only horizontal wires search the (y, x) order: a wireless cell (a flat netlist of
        // abutting pins) skips the second sort.
        let horizontal = (0..nw).any(|w| wires.y1[w] == wires.y2[w] && wires.x1[w] != wires.x2[w]);
        let yx = if horizontal {
            sorted_nodes(&py, &px)
        } else {
            Vec::new()
        };
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
                for &(_, node) in &xy[start..end] {
                    let (x, y) = (px[node as usize], py[node as usize]);
                    if on_segment(x, y, x1, y1, x2, y2) {
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
        // `union` keeps the smaller root, so a set's root is its first node: it is met
        // before its members and numbers the set.
        let mut comp = vec![NONE; n];
        let mut ncomp = 0u32;
        for i in 0..n {
            let r = find(&mut parent, i as u32) as usize;
            comp[i] = if r == i {
                ncomp += 1;
                ncomp - 1
            } else {
                comp[r]
            };
        }
        Geometry {
            px,
            py,
            pin_inst,
            pin_pin,
            pins0,
            labels0,
            ports0,
            noconns0,
            xy,
            yx,
            comp,
            nc: ncomp as usize,
        }
    };
    let (elems, geo) = if crate::parallel_ok(ni) {
        std::thread::scope(|sc| {
            let worker = sc.spawn(|| element_names(doc, names, own.cell));
            let geo = geometry();
            (worker.join().expect("element names do not panic"), geo)
        })
    } else {
        (element_names(doc, names, own.cell), geometry())
    };
    (cn.elem_start, cn.elem_name) = (elems.0, elems.1);
    for v in elems.2 {
        out.push(OWN_STATIC, v);
    }
    let Geometry {
        px,
        py,
        pin_inst,
        pin_pin,
        pins0,
        labels0,
        ports0,
        noconns0,
        xy,
        yx,
        comp,
        nc,
    } = geo;
    let n = px.len();
    for i in 0..ni {
        let mode = ignore_key.and_then(|key| {
            let mut attrs = doc.pool().attrs(inst.attrs[i]);
            attrs.find(|(k, _)| *k == key).and_then(|(_, v)| v.first())
        });
        cn.ignore.push(match mode.map(|v| s.get(*v)) {
            Some("true") => Ignore::True,
            Some("open") => Ignore::Open,
            Some("short") => Ignore::Short,
            _ => Ignore::No,
        });
    }

    let mut comp_nodes = vec![0u32; nc];
    let mut comp_pins = vec![0u32; nc];
    // Generated names must never be shared by two components (SPICE connects equal
    // node names): wires, labels, ports and noconns belong to exactly one component, so
    // their smallest id names it; a component made only of instance pins falls back to
    // its smallest (instance id, pin index) pin node, unique to that component.
    let mut comp_minid = vec![ObjId::NONE; nc];
    let mut comp_minpin = vec![(ObjId::NONE, 0u32); nc];
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
            let id = inst.id[pin_inst[k] as usize];
            let (best_id, best_p) = comp_minpin[c];
            let take = best_id == ObjId::NONE
                || match id_cmp(id, best_id, s) {
                    Ordering::Less => true,
                    Ordering::Equal => (p as u32) < best_p,
                    Ordering::Greater => false,
                };
            if take {
                comp_minpin[c] = (id, p as u32);
            }
        } else if i < ports0 {
            better(c, labels.id[i - labels0]);
        } else if i < noconns0 {
            comp_pins[c] += 1;
            better(c, ports.id[i - ports0]);
        } else {
            better(c, noconns.id[i - noconns0]);
        }
    }

    // Naming key per component: an owned object id (pin = NONE_PIN), else a pin node.
    let comp_key: Vec<(ObjId, u32)> = (0..nc)
        .map(|c| match comp_minid[c] {
            ObjId::NONE => comp_minpin[c],
            id => (id, NONE_PIN),
        })
        .collect();
    assert!(
        comp_key.iter().all(|k| k.0 != ObjId::NONE),
        "every component holds an id"
    );
    // Owned ids sort before pin nodes; then by id text, then pin index.
    let key_cmp = |a: (ObjId, u32), b: (ObjId, u32)| {
        (a.1 != NONE_PIN)
            .cmp(&(b.1 != NONE_PIN))
            .then_with(|| id_cmp(a.0, b.0, s))
            .then(a.1.cmp(&b.1))
    };
    let write_key = |k: (ObjId, u32), buf: &mut String| write_key(k, s, buf);

    // ---- names per component: (comp, class, bits range)
    let mut src_comp: Vec<u32> = Vec::new();
    let mut src_class: Vec<u8> = Vec::new();
    let mut src_text: Vec<Name> = Vec::new();
    let mut src_obj: Vec<ObjId> = Vec::new();
    let mut src_start: Vec<u32> = vec![0];
    let mut src_bits: Vec<Name> = Vec::new();
    let mut add_src =
        |c: u32, class: u8, text: &str, obj: ObjId, names: &mut Names, out: &mut Found<'_>| {
            let before = src_bits.len();
            if netexpr::expand(text, names, &mut src_bits, &mut buf).is_err() {
                out.push(
                    c,
                    finding(
                        Code::E011,
                        obj,
                        Name::NONE,
                        format!("`{text}` is wider than 65535 bits"),
                    ),
                );
                return;
            }
            let global = src_bits[before..].iter().all(|b| glob(globals, names, *b));
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
            &mut out,
        );
    }
    for p in 0..ports.id.len() {
        add_src(
            comp[ports0 + p],
            PORT,
            s.get(ports.text[p]),
            ports.id[p],
            names,
            &mut out,
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
            out.push(
                c as u32,
                finding(Code::E011, src_obj[k], src_text[k], detail),
            );
        } else {
            let detail = format!(
                "`{}` is shorted to `{}`",
                names.get(src_text[k]),
                names.get(src_text[b])
            );
            out.push(
                c as u32,
                finding(Code::E003, src_obj[k], src_text[k], detail),
            );
            merged = true;
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
            let class = if glob(globals, names, b) {
                GLOBAL
            } else {
                class
            };
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
            let class = if glob(globals, names, b) {
                GLOBAL
            } else {
                class
            };
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
    for i in (0..ni).filter(|_| inherit_key.is_some()) {
        for (k, vals) in doc.pool().attrs(inst.attrs[i]) {
            if Some(k) != inherit_key || vals.len() != 2 {
                continue;
            }
            let (pin, net) = (s.get(vals[0]), s.get(vals[1]));
            tmp.clear();
            if netexpr::expand(net, names, &mut tmp, &mut buf).is_err() {
                out.push(
                    OWN_STATIC,
                    finding(
                        Code::E011,
                        inst.id[i],
                        Name::NONE,
                        format!("`{net}` is wider than 65535 bits"),
                    ),
                );
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
                    out.push(
                        OWN_STATIC,
                        finding(Code::E011, inst.id[i], pin_name, detail),
                    );
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
            let sliced = match slicing(names.get(f.pin_name[p]), w, elems, comp_width[c]) {
                Ok(sliced) => sliced,
                Err(detail) => {
                    out.push(
                        c as u32,
                        finding(Code::E011, inst.id[i], f.pin_name[p], detail),
                    );
                    continue;
                }
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
    for slot in 0..nslots as usize {
        let r = find(&mut sp, slot as u32) as usize; // the set's first slot (see `comp`)
        net_of[slot] = if r == slot {
            nbits += 1;
            nbits - 1
        } else {
            net_of[r]
        };
    }

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
    // Unnamed bits: smallest (component key, bit index) among the slots.
    let mut unnamed: Vec<((ObjId, u32), u32, u32)> = vec![((ObjId::NONE, 0), 0, 1); nb];
    for c in 0..nc {
        for j in 0..comp_width[c] {
            let b = net_of[(slot_start[c] + j) as usize] as usize;
            if best[b].1 != Name::NONE {
                continue;
            }
            let (key, k, _) = unnamed[b];
            let take = key.0 == ObjId::NONE
                || match key_cmp(comp_key[c], key) {
                    Ordering::Less => true,
                    Ordering::Equal => j < k,
                    Ordering::Greater => false,
                };
            if take {
                unnamed[b] = (comp_key[c], j, comp_width[c]);
            }
        }
    }
    // Generated names are written into one buffer and interned as a batch, in bit order
    // (the order one-by-one interning used).
    cn.bit_name = best.iter().map(|b| b.1).collect();
    let unnamed_bits = best.iter().filter(|b| b.1 == Name::NONE).count();
    let mut gen_text = String::with_capacity(unnamed_bits * 16);
    let (mut gen_end, mut gen_bit) = (
        Vec::with_capacity(unnamed_bits),
        Vec::with_capacity(unnamed_bits),
    );
    for b in 0..nb {
        if best[b].1 != Name::NONE {
            continue;
        }
        let (key, k, width) = unnamed[b];
        assert!(
            key.0 != ObjId::NONE,
            "every component holds an object with an id"
        );
        write_key(key, &mut gen_text);
        if width > 1 {
            gen_text.push('[');
            push_decimal(k, &mut gen_text);
            gen_text.push(']');
        }
        gen_end.push(gen_text.len());
        gen_bit.push(b);
    }
    let texts: Vec<&str> = (0..gen_end.len())
        .map(|g| &gen_text[if g == 0 { 0 } else { gen_end[g - 1] }..gen_end[g]])
        .collect();
    let mut gen_name = Vec::with_capacity(texts.len());
    names.intern_batch(&texts, &mut gen_name);
    cn.bit_global = vec![false; nb];
    for (b, best) in best.iter().enumerate().filter(|(_, b)| b.1 != Name::NONE) {
        cn.bit_global[b] = glob(globals, names, best.1);
    }
    // A generated bit's text is at hand: no second read of the arena per bit.
    for (g, &b) in gen_bit.iter().enumerate() {
        cn.bit_name[b] = gen_name[g];
        cn.bit_global[b] = is_global(texts[g]) || globals.binary_search(&gen_name[g]).is_ok();
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
    for c in 0..nc {
        cn.net_name.push(match comp_best[c] {
            NONE => {
                let b = net_of[slot_start[c] as usize];
                if comp_width[c] == 1 {
                    cn.bit_name[b as usize]
                } else {
                    buf.clear();
                    write_key(comp_key[c], &mut buf);
                    names.intern(&buf)
                }
            }
            k => src_text[k as usize],
        });
        assert!(
            comp_width[c] <= u32::from(u16::MAX),
            "widths fit u16 (E011 above)"
        );
        cn.net_width.push(comp_width[c] as u16);
        cn.net_pins.push(comp_pins[c]);
        cn.net_bit_start.push(cn.net_bits.len() as u32);
        let r = slot_start[c]..slot_start[c + 1];
        cn.net_bits.extend(r.map(|sl| net_of[sl as usize]));
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
    let (mut real, mut other) = (vec![0u32; nb], vec![0u32; nb]);
    for &(slot, d) in &pin_domain_slots {
        let b = net_of[slot as usize] as usize;
        if d == Domain::Real {
            real[b] += 1;
        } else {
            other[b] += 1;
        }
    }

    // ---- findings
    // A cell's floating pins share few names (G, S, B, …): format each detail once.
    let mut unconnected: Vec<(Name, String)> = Vec::new();
    for k in 0..(labels0 - pins0) {
        let c = comp[pins0 + k];
        if comp_nodes[c as usize] == 1 {
            let i = pin_inst[k] as usize;
            let f = &ifaces[inst_iface[i] as usize];
            let pin = f.pin_name[pin_pin[k] as usize];
            let detail = match unconnected.iter().find(|(p, _)| *p == pin) {
                Some((_, d)) => d.clone(),
                None => {
                    let d = format!("pin `{}` is unconnected", names.get(pin));
                    if unconnected.len() < 64 {
                        unconnected.push((pin, d.clone()));
                    }
                    d
                }
            };
            out.push(c, finding(Code::W004, inst.id[i], pin, detail));
        }
    }
    let mut rows = Vec::new();
    for b in 0..nb {
        bit_findings(
            own.cell,
            cn.bit_name[b],
            cn.bit_global[b],
            cn.bit_conns[b],
            real[b],
            other[b],
            names,
            &mut rows,
        );
        for v in rows.drain(..) {
            out.push(OWN_BITS, v);
        }
    }
    // E009: duplicate element names (a bitset over the arena finds whether any; only then
    // are they sorted to name each).
    let mut seen = vec![0u64; (names.len() as usize).div_ceil(64)];
    let twice = cn.elem_name.iter().any(|&e| {
        let (w, bit) = (e.0 as usize / 64, 1u64 << (e.0 % 64));
        let again = seen[w] & bit != 0;
        seen[w] |= bit;
        again
    });
    drop(seen);
    let mut elems: Vec<(Name, ObjId)> = Vec::new();
    for i in (0..ni).filter(|_| twice) {
        elems.extend(cn.elems(i).iter().map(|&e| (e, inst.id[i])));
    }
    elems.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(id_cmp(a.1, b.1, s)));
    for w in elems.windows(2) {
        if w[0].0 == w[1].0 {
            let detail = format!("instance name `{}` is used twice", names.get(w[1].0));
            out.push(OWN_STATIC, finding(Code::E009, w[1].1, w[1].0, detail));
        }
    }
    port_findings(doc, own, names, &mut rows);
    for v in rows.drain(..) {
        out.push(OWN_PORTS, v);
    }
    if let Some(sd) = seed {
        merged |= cn.ignore.contains(&Ignore::Short);
        *sd = Seed {
            px,
            py,
            comp,
            pins0,
            labels0,
            ports0,
            noconns0,
            pin_inst,
            pin_pin,
            xy,
            yx,
            named: comp_best.iter().map(|&b| b != NONE).collect(),
            owner: std::mem::take(&mut out.owner),
            real,
            other,
            merged,
        };
    }
    assert_eq!(cn.net_name.len(), nc);
    assert_eq!(cn.bit_name.len(), nb);
    assert_eq!(cn.conn_start.len(), ni + 1);
    cn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_nodes_equals_a_pair_sort() {
        // Oracle: `sort_unstable` on (key, index). Wide, narrow (skipped digits) and
        // duplicate-heavy coordinates, on both sides of the radix threshold.
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for (n, range) in [
            (0, 1u64),
            (100, 7),
            (5000, 1 << 32),
            (9000, 40),
            (20_000, 1 << 20),
        ] {
            let mut coord = || (next() % range) as i64 as i32;
            let (a, b): (Vec<i32>, Vec<i32>) = (0..n).map(|_| (coord(), coord())).unzip();
            let mut want: Vec<(u64, u32)> = (0..n).map(|i| (key(a[i], b[i]), i as u32)).collect();
            want.sort_unstable();
            assert_eq!(sorted_nodes(&a, &b), want, "n {n} range {range}");
        }
    }

    #[test]
    fn slicing_matches_the_bus_rule_over_a_small_domain() {
        // Oracle (§4): whole when the net is the pin's width, sliced when it is
        // width × elements, else E011; whole wins when both hold (one element).
        for w in 0..5usize {
            for elems in 1..5usize {
                for net in 0..20u32 {
                    let want = if net as usize == w {
                        Some(false)
                    } else if net as usize == w * elems {
                        Some(true)
                    } else {
                        None
                    };
                    let got = slicing("p", w, elems, net);
                    assert_eq!(got.as_ref().ok().copied(), want, "{w} {elems} {net}");
                    if let Err(d) = got {
                        let text = format!("pin `p` ({w} bits × {elems}) on a {net}-bit net");
                        assert_eq!(d, text);
                    }
                }
            }
        }
    }

    #[test]
    fn on_segment_equals_a_point_walk() {
        // Oracle: a segment's lattice points are its ends and the steps of
        // (dx, dy) / gcd between them; every other point of a small box is off it.
        fn gcd(a: i32, b: i32) -> i32 {
            if b == 0 { a.abs() } else { gcd(b, a % b) }
        }
        let segments = [
            (0, 0, 3, 3),
            (-2, 1, 2, -1),
            (1, -3, 1, 2),
            (-3, 0, 3, 0),
            (2, 2, 2, 2),
            (0, 0, 3, -2),
        ];
        for (x1, y1, x2, y2) in segments {
            let g = gcd(x2 - x1, y2 - y1).max(1);
            let (sx, sy) = ((x2 - x1) / g, (y2 - y1) / g);
            let on: Vec<(i32, i32)> = (0..=g).map(|k| (x1 + k * sx, y1 + k * sy)).collect();
            for x in -3..=3 {
                for y in -3..=3 {
                    let want = on.contains(&(x, y));
                    let got = on_segment(x, y, x1, y1, x2, y2);
                    assert_eq!(got, want, "({x}, {y}) on {x1},{y1}–{x2},{y2}");
                }
            }
        }
        // No overflow at the coordinate extremes.
        assert!(on_segment(
            i32::MAX,
            i32::MAX,
            i32::MIN,
            i32::MIN,
            i32::MAX,
            i32::MAX
        ));
        assert!(!on_segment(0, 1, i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    }
}
