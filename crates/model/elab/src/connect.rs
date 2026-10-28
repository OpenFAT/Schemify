//! Connectivity of one cell (netlist spec §3–§5): geometry → connected components
//! ("nets", what render highlights and the plugin `Nets` table lists) → bit nets
//! (what netlists connect), with names, bus slicing, iterated instances, inherited
//! connection overrides and `ignore short`. This file holds the spine and its first
//! phases (geometry, components, name sources); `bits` turns components into bit nets
//! and `netcheck` writes the findings.
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
//! SIMD triage: the hot loops are the two sorts (radix) and union-find (a pointer chase,
//! a chain): neither has the lane shape. The only lane-independent loop, the
//! collinearity scan for diagonal wires, runs over the few nodes in a diagonal wire's
//! x-range, so it stays scalar.
//!
//! Contents:
//! - `pub fn connect(doc, own, ifaces, inst_iface, globals, names, out) -> CellNets`
//! - `pub(crate) fn connect_seeded(…, seed: Option<&mut Seed>) -> CellNets`
//! - `pub(crate) struct Cx`, `pub(crate) struct Found` (`push`), `pub(crate) struct Comps`,
//!   `pub(crate) struct Sources` (`bits`)

use crate::bits;
use crate::iface::Iface;
use crate::names::{Name, Names, radix_sort};
use crate::netcheck;
use crate::netexpr;
use crate::nets::{
    CellNets, GLOBAL, Ignore, LABEL, NONE_PIN, OWN_STATIC, PORT, Seed, find, glob, id_cmp, key,
    on_segment, place, union,
};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::{Document, ObjId};
use std::cmp::Ordering;

/// The inputs every phase of one [`connect`] run reads.
pub(crate) struct Cx<'a> {
    pub(crate) doc: &'a Document,
    /// The cell's own interface (ports and inherited pins bind to its pins).
    pub(crate) own: &'a Iface,
    pub(crate) ifaces: &'a [Iface],
    /// Interface of each instance row, `u32::MAX` when unresolved.
    pub(crate) inst_iface: &'a [u32],
    /// Names made global by `global true` labels, sorted.
    pub(crate) globals: &'a [Name],
}

/// Findings with their owners (what `live` drops when the owner changes).
pub(crate) struct Found<'a> {
    pub(crate) out: &'a mut Vec<Violation>,
    /// Owner of each finding appended to `out`: a component, or an `OWN_*` constant.
    pub(crate) owner: Vec<u32>,
}

/// Per-component facts the naming and width rules read.
pub(crate) struct Comps {
    /// Nodes in each component.
    pub(crate) nodes: Vec<u32>,
    /// Instance pins and ports touching it (the plugin `Nets.pin_count`).
    pub(crate) pins: Vec<u32>,
    /// Naming key: an owned object id (pin `NONE_PIN`), else its smallest pin node.
    pub(crate) key: Vec<(ObjId, u32)>,
    /// Best name source ([`Sources`] row), `u32::MAX` when unnamed.
    pub(crate) best: Vec<u32>,
    /// Bits it carries: its best name's, else its widest pin's.
    pub(crate) width: Vec<u32>,
}

/// Name sources: labels, then ports, each with its bit names.
#[derive(Default)]
pub(crate) struct Sources {
    pub(crate) comp: Vec<u32>,
    /// [`GLOBAL`], [`PORT`] or [`LABEL`]: the §3 precedence class.
    pub(crate) class: Vec<u8>,
    pub(crate) text: Vec<Name>,
    pub(crate) obj: Vec<ObjId>,
    /// CSR offsets (`sources + 1`) into `bits`.
    pub(crate) start: Vec<u32>,
    pub(crate) bits: Vec<Name>,
    /// Whether the source's bits name its component's slots (`false`: an `E011` width
    /// mismatch with the best name).
    pub(crate) usable: Vec<bool>,
}

/// Builds the connectivity of `doc`. `own` is the cell's own interface (ports and
/// inherited pins bind to its pins; a bench passes an interface without pins);
/// `inst_iface[i]` indexes `ifaces` for instance row `i`, or is `u32::MAX` when the
/// instance's cell did not resolve (it then has no pins). `globals` (sorted) are the
/// names made global by `global true` labels anywhere in the hierarchy (see
/// [`crate::global_labels`]); `0` and `…!` are global by spelling. Findings (`E003`,
/// `E009`, `E011`, `E012`, `E002`, `W004`, `W010`, `W016`) are appended to `out`.
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
    let ni = doc.instances().id.len();
    assert_eq!(inst_iface.len(), ni, "one interface slot per instance");
    let cx = Cx {
        doc,
        own,
        ifaces,
        inst_iface,
        globals,
    };
    let mut out = Found {
        out,
        owner: Vec::new(),
    };
    let mut cn = CellNets::default();
    // Element names, then a generated name per unnamed component: at most one per two
    // nodes in practice (a component holds a wire's two ends, or two pins), so the table
    // grows once; a bigger fresh table only costs page faults.
    names.reserve(ni + node_count(&cx) / 2);
    // Instance elements run on a second thread in a large cell: they are the only names
    // interned before the components exist.
    let (elems, (mut geo, nc)) = if crate::parallel_ok(ni) {
        std::thread::scope(|sc| {
            let worker = sc.spawn(|| netcheck::element_names(doc, names, own.cell));
            let geo = geometry(&cx);
            (worker.join().expect("element names do not panic"), geo)
        })
    } else {
        (netcheck::element_names(doc, names, own.cell), geometry(&cx))
    };
    let (elem_start, elem_name, wide) = elems;
    (cn.elem_start, cn.elem_name) = (elem_start, elem_name);
    for v in wide {
        out.push(OWN_STATIC, v);
    }
    cn.ignore = ignore_modes(doc);
    let mut comps = components(&cx, &geo, nc);
    let mut src = sources(&cx, &geo, names, &mut out);
    let merged = name_components(&cx, &mut comps, &mut src, names, &mut out);
    let mut slots = bits::slots(&cx, &geo, &comps, &src, names, &mut cn, &mut out);
    let (net_of, nb) = bits::merge(&mut slots, &cn);
    bits::name_bits(&cx, &comps, &slots, &net_of, nb, names, &mut cn);
    bits::outputs(&cx, &geo, &comps, &src, &slots, &net_of, names, &mut cn);
    let (real, other) = bits::domains(&slots, &net_of, nb);
    netcheck::findings(&cx, &geo, &comps, &cn, &real, &other, names, &mut out);
    if let Some(sd) = seed {
        geo.named = comps.best.iter().map(|&b| b != NONE).collect();
        geo.owner = std::mem::take(&mut out.owner);
        (geo.real, geo.other) = (real, other);
        geo.merged = merged || cn.ignore.contains(&Ignore::Short);
        *sd = geo;
    }
    assert_eq!(cn.net_name.len(), nc);
    assert_eq!(cn.bit_name.len(), nb);
    assert_eq!(cn.conn_start.len(), ni + 1);
    cn
}

impl Found<'_> {
    /// Appends finding `v`, owned by `owner`.
    pub(crate) fn push(&mut self, owner: u32, v: Violation) {
        self.out.push(v);
        self.owner.push(owner);
        debug_assert!(self.owner.len() <= self.out.len());
    }
}

impl Sources {
    /// The bit names of source `k`.
    pub(crate) fn bits(&self, k: usize) -> &[Name] {
        &self.bits[self.start[k] as usize..self.start[k + 1] as usize]
    }
}

// ---- private ----

/// Geometric nodes of the cell: two per wire, one per ordinary instance pin, label, port
/// and noconn.
fn node_count(cx: &Cx<'_>) -> usize {
    let doc = cx.doc;
    let inst = doc.instances();
    let pins: usize = (0..inst.id.len())
        .filter_map(|i| cx.ifaces.get(cx.inst_iface[i] as usize))
        .map(|f| f.pin_inherit.iter().filter(|p| **p == Name::NONE).count())
        .sum();
    2 * doc.wires().id.len()
        + pins
        + doc.labels().id.len()
        + doc.ports().id.len()
        + doc.noconns().id.len()
}

/// Nodes (wire ends, instance pins, labels, ports, noconns), the two node sorts, and the
/// components numbered densely in node order: `(seed, component count)`.
fn geometry(cx: &Cx<'_>) -> (Seed, usize) {
    let doc = cx.doc;
    let (inst, wires) = (doc.instances(), doc.wires());
    let (labels, ports, noconns) = (doc.labels(), doc.ports(), doc.noconns());
    let nw = wires.id.len();
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
    for i in 0..inst.id.len() {
        let Some(f) = cx.ifaces.get(cx.inst_iface[i] as usize) else {
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
    assert!(n < NONE as usize);
    assert_eq!(py.len(), n);

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
        for &(_, node) in sorted[start..].iter().take_while(|e| e.0 <= hi) {
            union(&mut parent, node, wn);
        }
    }

    // ---- components, dense in node order: `union` keeps the smaller root, so a set's
    // root is its first node, met before its members, and numbers the set.
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
    assert!(ncomp as usize <= n);
    let seed = Seed {
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
        ..Seed::default()
    };
    (seed, ncomp as usize)
}

/// `(key(a, b), i)` for every node `i`, sorted (the radix sort keeps ties in index order).
fn sorted_nodes(a: &[i32], b: &[i32]) -> Vec<(u64, u32)> {
    assert_eq!(a.len(), b.len());
    let v: Vec<(u64, u32)> = (0..a.len()).map(|i| (key(a[i], b[i]), i as u32)).collect();
    radix_sort(v)
}

/// The `ignore` attribute of every instance (absent or unknown: [`Ignore::No`]).
fn ignore_modes(doc: &Document) -> Vec<Ignore> {
    let (s, inst) = (doc.strings(), doc.instances());
    // The key looked up once (a key the document never interned is on no row).
    let ignore_key = s.find("ignore");
    let modes: Vec<Ignore> = (0..inst.id.len())
        .map(|i| {
            let mode = ignore_key.and_then(|key| {
                let mut attrs = doc.pool().attrs(inst.attrs[i]);
                attrs.find(|(k, _)| *k == key).and_then(|(_, v)| v.first())
            });
            Ignore::parse(mode.map(|v| s.get(*v)))
        })
        .collect();
    assert_eq!(modes.len(), inst.id.len());
    modes
}

/// Node and pin counts, widest pin and naming key of each of the `nc` components.
///
/// Generated names must never be shared by two components (SPICE connects equal node
/// names): wires, labels, ports and noconns belong to exactly one component, so their
/// smallest id names it; a component made only of instance pins falls back to its smallest
/// (instance id, pin index) pin node, unique to that component.
fn components(cx: &Cx<'_>, geo: &Seed, nc: usize) -> Comps {
    let doc = cx.doc;
    let s = doc.strings();
    let (inst, wires, labels) = (doc.instances(), doc.wires(), doc.labels());
    let (ports, noconns) = (doc.ports(), doc.noconns());
    let mut nodes = vec![0u32; nc];
    let mut pins = vec![0u32; nc];
    let mut pinw = vec![1u32; nc];
    let mut minid = vec![ObjId::NONE; nc];
    let mut minpin = vec![(ObjId::NONE, 0u32); nc];
    let mut better = |c: usize, id: ObjId| {
        if minid[c] == ObjId::NONE || id_cmp(id, minid[c], s) == Ordering::Less {
            minid[c] = id;
        }
    };
    for i in 0..geo.comp.len() {
        let c = geo.comp[i] as usize;
        nodes[c] += 1;
        if i < geo.pins0 {
            better(c, wires.id[i / 2]);
        } else if i < geo.labels0 {
            let k = i - geo.pins0;
            let f = &cx.ifaces[cx.inst_iface[geo.pin_inst[k] as usize] as usize];
            let p = geo.pin_pin[k] as usize;
            pins[c] += 1;
            pinw[c] = pinw[c].max(f.pin_bit_start[p + 1] - f.pin_bit_start[p]);
            let id = inst.id[geo.pin_inst[k] as usize];
            let (best_id, best_p) = minpin[c];
            let take = best_id == ObjId::NONE
                || match id_cmp(id, best_id, s) {
                    Ordering::Less => true,
                    Ordering::Equal => (p as u32) < best_p,
                    Ordering::Greater => false,
                };
            if take {
                minpin[c] = (id, p as u32);
            }
        } else if i < geo.ports0 {
            better(c, labels.id[i - geo.labels0]);
        } else if i < geo.noconns0 {
            pins[c] += 1;
            better(c, ports.id[i - geo.ports0]);
        } else {
            better(c, noconns.id[i - geo.noconns0]);
        }
    }
    let key: Vec<(ObjId, u32)> = (0..nc)
        .map(|c| match minid[c] {
            ObjId::NONE => minpin[c],
            id => (id, NONE_PIN),
        })
        .collect();
    assert!(
        key.iter().all(|k| k.0 != ObjId::NONE),
        "every component holds an id"
    );
    Comps {
        nodes,
        pins,
        key,
        best: vec![NONE; nc],
        width: pinw,
    }
}

/// The name sources: every label, then every port, with its bits and class (a source
/// whose bits are all global is [`GLOBAL`]). An over-wide text is `E011` and no source.
fn sources(cx: &Cx<'_>, geo: &Seed, names: &mut Names, out: &mut Found<'_>) -> Sources {
    let doc = cx.doc;
    let (s, labels, ports) = (doc.strings(), doc.labels(), doc.ports());
    let mut src = Sources {
        start: vec![0],
        ..Sources::default()
    };
    let mut buf = String::new();
    let all = (0..labels.id.len())
        .map(|l| {
            (
                geo.comp[geo.labels0 + l],
                LABEL,
                labels.text[l],
                labels.id[l],
            )
        })
        .chain(
            (0..ports.id.len())
                .map(|p| (geo.comp[geo.ports0 + p], PORT, ports.text[p], ports.id[p])),
        );
    for (c, class, sym, obj) in all {
        let text = s.get(sym);
        let before = src.bits.len();
        if netexpr::expand(text, names, &mut src.bits, &mut buf).is_err() {
            let detail = format!("`{text}` is wider than 65535 bits");
            let v = Violation::new(Code::E011, cx.own.cell, obj, Name::NONE, detail);
            out.push(c, v);
            continue;
        }
        let global = src.bits[before..]
            .iter()
            .all(|b| glob(cx.globals, names, *b));
        src.comp.push(c);
        src.class.push(if global { GLOBAL } else { class });
        src.text.push(names.intern(text));
        src.obj.push(obj);
        src.start.push(src.bits.len() as u32);
    }
    src.usable = vec![true; src.comp.len()];
    assert_eq!(src.start.len(), src.comp.len() + 1);
    src
}

/// Picks each component's best name (smallest (class, text)), sets its width, and checks
/// the other names on it: `E011` for a name of another width (unusable), `E003` for a
/// distinct name of the same width (a short). Returns whether any short merges names.
fn name_components(
    cx: &Cx<'_>,
    comps: &mut Comps,
    src: &mut Sources,
    names: &Names,
    out: &mut Found<'_>,
) -> bool {
    for k in 0..src.comp.len() {
        let c = src.comp[k] as usize;
        let b = comps.best[c];
        if b == NONE
            || (src.class[k], names.get(src.text[k]))
                < (src.class[b as usize], names.get(src.text[b as usize]))
        {
            comps.best[c] = k as u32;
        }
    }
    for c in 0..comps.best.len() {
        if comps.best[c] != NONE {
            comps.width[c] = src.bits(comps.best[c] as usize).len() as u32;
        }
    }
    let mut merged = false;
    for k in 0..src.comp.len() {
        let c = src.comp[k] as usize;
        let b = comps.best[c] as usize;
        let (bk, bb) = (src.bits(k), src.bits(b));
        if b == k || bk == bb {
            continue;
        }
        let (wk, wb) = (bk.len(), bb.len());
        let (tk, tb) = (names.get(src.text[k]), names.get(src.text[b]));
        let (code, detail) = if wk != wb {
            src.usable[k] = false;
            let detail = format!("`{tk}` ({wk} bits) and `{tb}` ({wb} bits) name one net");
            (Code::E011, detail)
        } else {
            merged = true;
            (Code::E003, format!("`{tk}` is shorted to `{tb}`"))
        };
        let v = Violation::new(code, cx.own.cell, src.obj[k], src.text[k], detail);
        out.push(c as u32, v);
    }
    assert_eq!(comps.width.len(), comps.best.len());
    merged
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
}
