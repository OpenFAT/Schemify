//! One cell's connectivity in, its connectivity findings out (netlist spec §3–§5, §8):
//! instance element names with their width findings (`E011`), lone pins (`W004`), bit
//! findings (`W016` single connection, `W010` real beside analog/digital), duplicate
//! element names (`E009`) and the port ↔ interface pin findings (`E012`, `E002`).
//! `connect` runs all of them; `live` reruns the per-bit and port ones per edit.
//!
//! Contents:
//! - `pub(crate) fn element_names(doc, names, cell) -> (Vec<u32>, Vec<Name>, Vec<Violation>)`
//! - `pub(crate) fn findings(cx, geo, comps, cn, real, other, names, out)`
//! - `pub(crate) fn bit_findings(cell, name, global, (conns, real, other), names, out)`
//! - `pub(crate) fn port_findings(doc, own, names, out)`

use crate::connect::{Comps, Cx, Found};
use crate::iface::Iface;
use crate::names::{Name, Names};
use crate::netexpr;
use crate::nets::{CellNets, OWN_BITS, OWN_PORTS, OWN_STATIC, Seed, id_cmp};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::{Document, ObjId};

/// Element names of every instance of `doc` (`X1[3]` … for `X1[3:0]`, else the name):
/// `(CSR starts, names, E011 findings of cell `cell`)`. Plain names (nearly all) are
/// interned in batches (`Names::intern_batch`: 10^6 new names in a flat design); a bus
/// name flushes the batch first, so every name gets the number one-by-one interning gives.
pub(crate) fn element_names(
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
                found.push(Violation::new(
                    Code::E011,
                    cell,
                    inst.id[i],
                    Name::NONE,
                    format!("instance name `{text}` is wider than 65535"),
                ));
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

/// The findings of one `connect` run after its outputs exist: `W004` per lone pin node,
/// the bit findings (`W016`, `W010`), `E009` duplicate element names, and the port
/// findings (`E012`, `E002`), each with its owner.
#[allow(clippy::too_many_arguments)]
pub(crate) fn findings(
    cx: &Cx<'_>,
    geo: &Seed,
    comps: &Comps,
    cn: &CellNets,
    real: &[u32],
    other: &[u32],
    names: &Names,
    out: &mut Found<'_>,
) {
    let (s, inst) = (cx.doc.strings(), cx.doc.instances());
    let before = out.owner.len();
    // A cell's floating pins share few names (G, S, B, …): format each detail once.
    let mut unconnected: Vec<(Name, String)> = Vec::new();
    for k in 0..(geo.labels0 - geo.pins0) {
        let c = geo.comp[geo.pins0 + k];
        if comps.nodes[c as usize] != 1 {
            continue;
        }
        let i = geo.pin_inst[k] as usize;
        let f = &cx.ifaces[cx.inst_iface[i] as usize];
        let pin = f.pin_name[geo.pin_pin[k] as usize];
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
        let v = Violation::new(Code::W004, cx.own.cell, inst.id[i], pin, detail);
        out.push(c, v);
    }
    let mut rows = Vec::new();
    for b in 0..cn.bit_name.len() {
        let (name, global) = (cn.bit_name[b], cn.bit_global[b]);
        let counts = (cn.bit_conns[b], real[b], other[b]);
        bit_findings(cx.own.cell, name, global, counts, names, &mut rows);
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
    for i in (0..inst.id.len()).filter(|_| twice) {
        elems.extend(cn.elems(i).iter().map(|&e| (e, inst.id[i])));
    }
    elems.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(id_cmp(a.1, b.1, s)));
    for w in elems.windows(2).filter(|w| w[0].0 == w[1].0) {
        let detail = format!("instance name `{}` is used twice", names.get(w[1].0));
        let v = Violation::new(Code::E009, cx.own.cell, w[1].1, w[1].0, detail);
        out.push(OWN_STATIC, v);
    }
    port_findings(cx.doc, cx.own, names, &mut rows);
    for v in rows.drain(..) {
        out.push(OWN_PORTS, v);
    }
    assert!(out.owner.len() >= before);
}

/// `W016` and `W010` of a bit named `name` with `conns` connections and `real` / `other`
/// instance pin bits by domain (the bit-owned findings).
pub(crate) fn bit_findings(
    cell: Name,
    name: Name,
    global: bool,
    (conns, real, other): (u32, u32, u32),
    names: &Names,
    out: &mut Vec<Violation>,
) {
    let row = |code, detail| Violation::new(code, cell, ObjId::NONE, name, detail);
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
    let row = |code, object, param, detail| Violation::new(code, own.cell, object, param, detail);
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
