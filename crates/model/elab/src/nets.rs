//! The connectivity tables of one cell ([`CellNets`]) and the geometry and naming rules
//! `connect` (a whole cell) and `live` (one edit) share: placement, point-on-segment,
//! node keys, union-find, id order, generated names, bus slicing, globals. Cites netlist
//! spec §3 (nets, names, globals), §4 (bus slicing), §7.1 (`ignore`), format spec §3
//! (placement), D33 (i32 milli-unit coordinates).
//!
//! Contents:
//! - `pub const NO_NET`, `pub enum Ignore` (`pub(crate) parse`)
//! - `pub struct CellNets`: `bit`, `elems`, `port`, `pins`, `bits_of`
//! - `pub(crate) struct Seed`, `pub(crate) struct NetKey`
//! - `pub(crate) const GLOBAL`, `PORT`, `LABEL`, `OWN_STATIC`, `OWN_PORTS`, `OWN_BITS`,
//!   `NONE_PIN`
//! - `pub fn place(x, y, mirrored, turns, at_x, at_y) -> (i32, i32)`
//! - `pub fn global_labels(doc: &Document, names: &mut Names, out: &mut Vec<Name>)`
//! - `pub(crate) fn label_is_global(doc: &Document, l: usize) -> bool`
//! - `pub(crate) fn glob(globals: &[Name], names: &Names, n: Name) -> bool`
//! - `pub(crate) fn slicing(pin, w, elems, net) -> Result<bool, String>`
//! - `pub(crate) fn on_segment(x, y, x1, y1, x2, y2) -> bool`
//! - `pub(crate) fn key(a: i32, b: i32) -> u64`, `pub(crate) fn unkey(k: u64) -> (i32, i32)`
//! - `pub(crate) fn find(parent: &mut [u32], x: u32) -> u32`
//! - `pub(crate) fn union(parent: &mut [u32], a: u32, b: u32)`
//! - `pub(crate) fn id_cmp(a: ObjId, b: ObjId, s: &Strings) -> Ordering`
//! - `pub(crate) fn write_key(k: NetKey, s: &Strings, buf: &mut String)`
//! - `pub(crate) fn csr<T>(start: &[u32], items: &[T], row: usize) -> &[T]`

use crate::names::{self, Name, Names};
use crate::netexpr;
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
/// what a netlist connects. All names live in the arena passed to [`crate::connect`].
///
/// [`crate::connect`] numbers components and bits densely. The editor's [`crate::CellState`]
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
    pub(crate) net_bit_start: Vec<u32>,
    /// Bit net of each component bit (patched components append theirs: unreferenced
    /// entries are garbage until the next full build).
    pub(crate) net_bits: Vec<u32>,
    /// Component of each wire row.
    pub wire_net: Vec<u32>,
    /// Component of each label row.
    pub label_net: Vec<u32>,
    /// Component of each port row.
    pub port_net: Vec<u32>,
    /// CSR offsets (`instances + 1`) into [`CellNets::pin_net`].
    pub(crate) pin_start: Vec<u32>,
    /// Component of each ordinary pin of each instance (interface pin order, inherited
    /// pins skipped); unresolved instances have none.
    pub(crate) pin_net: Vec<u32>,

    /// Emitted bit name (`d[3]`, `vdd!`, `net_0abcde`).
    pub bit_name: Vec<Name>,
    /// Whether the bit is a global net (§3).
    pub bit_global: Vec<bool>,
    /// Connections: instance pin bits, port bits and inherit overrides on the bit.
    pub bit_conns: Vec<u32>,
    /// Every name that refers to a bit (emitted or merged by name), sorted by `Name`.
    pub by_name: Vec<(Name, u32)>,

    /// CSR offsets (`instances + 1`) into [`CellNets::elem_name`].
    pub(crate) elem_start: Vec<u32>,
    /// Element names of each instance (`X1[3]` … for `X1[3:0]`, else the name).
    pub(crate) elem_name: Vec<Name>,
    /// Offset of each instance's connections in [`CellNets::conns`]; element `k`, pin
    /// bit `b` (ordinary pins, `Iface::pin_off`) is at `start + k * bits_total + b`.
    pub conn_start: Vec<u32>,
    /// Bit net per connection, or [`NO_NET`].
    pub conns: Vec<u32>,
    /// `ignore` mode per instance.
    pub ignore: Vec<Ignore>,

    /// `inherit <pin> <net>` overrides: owning instance row.
    pub(crate) inh_inst: Vec<u32>,
    /// Overridden pin name.
    pub(crate) inh_pin: Vec<Name>,
    /// CSR offsets (`overrides + 1`) into [`CellNets::inh_bits`].
    pub(crate) inh_bit_start: Vec<u32>,
    /// Bit nets of the override net, in written order.
    pub inh_bits: Vec<u32>,

    /// CSR offsets (`own pins + 1`) into [`CellNets::port_bits`]: the bit nets each pin
    /// of the cell's own interface binds to (ordinary and inherited pins).
    pub(crate) port_bit_start: Vec<u32>,
    /// Bit nets of the own pins, in pin bit order.
    pub port_bits: Vec<u32>,
}

/// What the full pass knows that the editor's incremental connectivity (`live`) starts
/// from: the nodes in [`crate::connect`]'s order (wire ends, instance pins, labels, ports,
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

/// The naming key of a component (§3): an owned object id (`pin` is [`NONE_PIN`]), or an
/// instance id and the pin index of its smallest pin node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NetKey {
    pub(crate) id: ObjId,
    pub(crate) pin: u32,
}

/// Name classes in precedence order (§3): globals win, then ports, then labels.
pub(crate) const GLOBAL: u8 = 0;

pub(crate) const PORT: u8 = 1;

pub(crate) const LABEL: u8 = 2;

/// Owner of a finding that no component or bit owns: instance names (`E011`, `E009`) and
/// `inherit` overrides (`E011`); they change only with what forces a full build.
pub(crate) const OWN_STATIC: u32 = u32::MAX;

/// Owner of the port ↔ interface pin findings (`E012`, `E002`).
pub(crate) const OWN_PORTS: u32 = u32::MAX - 1;

/// Owner of the bit findings (`W016`, `W010`), recomputed from each bit's counts.
pub(crate) const OWN_BITS: u32 = u32::MAX - 2;

/// A component key's pin index when the key is an owned object id.
pub(crate) const NONE_PIN: u32 = u32::MAX;

impl Ignore {
    /// Reads an `ignore` attribute value (absent or unknown: [`Ignore::No`]).
    pub(crate) fn parse(v: Option<&str>) -> Ignore {
        match v {
            Some("true") => Ignore::True,
            Some("open") => Ignore::Open,
            Some("short") => Ignore::Short,
            _ => Ignore::No,
        }
    }
}

impl CellNets {
    /// Returns the bit net named `name`, if any net carries it. O(log names).
    ///
    /// # Panics
    ///
    /// Panics only on a broken internal invariant: a name index row past the bit table.
    #[must_use]
    pub fn bit(&self, name: Name) -> Option<u32> {
        let i = self.by_name.binary_search_by_key(&name, |p| p.0).ok()?;
        let bit = self.by_name[i].1;
        assert!(
            (bit as usize) < self.bit_name.len(),
            "by_name points past the bit table"
        );
        Some(bit)
    }

    /// Returns the element names of instance row `i`.
    ///
    /// # Panics
    ///
    /// If `i` is not an instance row.
    #[must_use]
    pub fn elems(&self, i: usize) -> &[Name] {
        csr(&self.elem_start, &self.elem_name, i)
    }

    /// Returns the bit nets of own pin `p`.
    ///
    /// # Panics
    ///
    /// If `p` is not a pin of the cell's own interface.
    #[must_use]
    pub fn port(&self, p: usize) -> &[u32] {
        csr(&self.port_bit_start, &self.port_bits, p)
    }

    /// Returns the components of instance row `i`'s ordinary pins.
    ///
    /// # Panics
    ///
    /// If `i` is not an instance row.
    #[must_use]
    pub fn pins(&self, i: usize) -> &[u32] {
        csr(&self.pin_start, &self.pin_net, i)
    }

    /// Returns the bit nets of component `c` (empty for a hole).
    ///
    /// # Panics
    ///
    /// If `c` is not a component row.
    #[must_use]
    pub fn bits_of(&self, c: usize) -> &[u32] {
        assert!(c < self.net_width.len(), "component {c} out of range");
        assert_eq!(self.net_bit_start.len(), self.net_width.len());
        let start = self.net_bit_start[c] as usize;
        let end = start + usize::from(self.net_width[c]);
        assert!(
            end <= self.net_bits.len(),
            "component {c}'s bits are in the column"
        );
        &self.net_bits[start..end]
    }
}

/// Places a symbol-local point (format spec §3): mirror x, rotate clockwise by quarter
/// turns (y down), translate. Saturates at the i32 range.
///
/// # Panics
///
/// If `turns` is not below 4.
#[must_use]
pub fn place(x: i32, y: i32, mirrored: bool, turns: u8, at_x: i32, at_y: i32) -> (i32, i32) {
    assert!(turns < 4, "a rotation is 0 to 3 quarter turns, got {turns}");
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
    // A mirror or quarter turn keeps the L1 distance from the origin.
    debug_assert_eq!(rx.abs() + ry.abs(), x.abs() + y.abs());
    let sat = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    (sat(rx + i64::from(at_x)), sat(ry + i64::from(at_y)))
}

/// Appends the bit names of `doc`'s labels marked `global true` (netlist spec §3):
/// they are global in the whole elaborated hierarchy.
pub fn global_labels(doc: &Document, names: &mut Names, out: &mut Vec<Name>) {
    let (s, labels) = (doc.strings(), doc.labels());
    let before = out.len();
    let mut buf = String::new();
    let global_rows = (0..labels.id.len()).filter(|&l| label_is_global(doc, l));
    for l in global_rows {
        // An over-wide expression is reported by `connect` as E011.
        let _ = netexpr::expand(s.get(labels.text[l]), names, out, &mut buf);
    }
    debug_assert!(out[before..].iter().all(|&n| n != Name::NONE));
}

/// Whether label row `l` carries `global true`.
pub(crate) fn label_is_global(doc: &Document, l: usize) -> bool {
    assert!(l < doc.labels().id.len(), "label row {l} out of range");
    let s = doc.strings();
    doc.pool()
        .find_attr(s, doc.labels().attrs[l], "global")
        .and_then(|v| v.first())
        .is_some_and(|v| s.get(*v) == "true")
}

/// Whether bit name `n` is global: by spelling (`0`, `…!`) or made so by a `global true`
/// label (`globals`, sorted).
pub(crate) fn glob(globals: &[Name], names: &Names, n: Name) -> bool {
    debug_assert!(globals.is_sorted(), "globals is binary-searched");
    netexpr::is_global(names.get(n)) || globals.binary_search(&n).is_ok()
}

/// Bus slicing (§4): whether a `w`-bit pin of an `elems`-element instance takes its whole
/// `net`-bit net (`Ok(false)`) or one `w`-bit slice per element (`Ok(true)`).
///
/// # Errors
///
/// The `E011` detail when the net is neither width.
pub(crate) fn slicing(pin: &str, w: usize, elems: usize, net: u32) -> Result<bool, String> {
    assert!(elems >= 1, "an instance has at least one element");
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

/// The sort key of point `(a, b)`: both coordinates offset to unsigned, `a` high.
pub(crate) fn key(a: i32, b: i32) -> u64 {
    let k = (u64::from(a as u32 ^ 0x8000_0000) << 32) | u64::from(b as u32 ^ 0x8000_0000);
    debug_assert_eq!(unkey(k), (a, b), "key and unkey are inverses");
    k
}

/// Decodes [`key`]: `(a, b)`.
pub(crate) fn unkey(k: u64) -> (i32, i32) {
    (
        ((k >> 32) as u32 ^ 0x8000_0000) as i32,
        (k as u32 ^ 0x8000_0000) as i32,
    )
}

/// The root of `x`'s set, halving the path on the way.
pub(crate) fn find(parent: &mut [u32], mut x: u32) -> u32 {
    assert!((x as usize) < parent.len(), "element {x} out of the forest");
    // Each step moves at least one level up a tree of at most `parent.len()` levels.
    let mut steps = 0;
    while parent[x as usize] != x {
        debug_assert!(steps < parent.len(), "union-find cycle");
        steps += 1;
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
    debug_assert_eq!(
        parent[ra.min(rb) as usize],
        ra.min(rb),
        "the kept root is a root"
    );
}

/// Compares ids by their text (bytewise), the order `net_<id>` names use.
pub(crate) fn id_cmp(a: ObjId, b: ObjId, s: &Strings) -> Ordering {
    if !a.is_alias() && !b.is_alias() {
        let order = a.0.cmp(&b.0);
        debug_assert_eq!(
            order,
            a.text(s).cmp(&b.text(s)),
            "6-char ids order like their text"
        );
        return order;
    }
    a.text(s).cmp(&b.text(s))
}

/// Appends `net_<id>` (an owned object id) or `net_<inst id>_<pin>` (a pin node): the
/// generated name of an unnamed component keyed `k` (§3).
pub(crate) fn write_key(k: NetKey, s: &Strings, buf: &mut String) {
    assert!(k.id != ObjId::NONE, "a naming key holds an id");
    buf.push_str("net_");
    k.id.write(s, buf);
    if k.pin != NONE_PIN {
        buf.push('_');
        names::push_decimal(k.pin, buf);
    }
}

/// Row `row` of a CSR table: `items[start[row]..start[row + 1]]`.
pub(crate) fn csr<'a, T>(start: &[u32], items: &'a [T], row: usize) -> &'a [T] {
    assert!(
        row + 1 < start.len(),
        "row {row} of a {}-row table",
        start.len().saturating_sub(1)
    );
    let (from, to) = (start[row] as usize, start[row + 1] as usize);
    assert!(from <= to, "CSR offsets are monotone");
    assert!(to <= items.len(), "CSR offsets stay inside their column");
    &items[from..to]
}

#[cfg(test)]
mod tests {
    use super::*;

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
