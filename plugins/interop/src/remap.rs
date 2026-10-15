//! Re-mastered instances: labels and stubs carry connectivity when a master changes to one
//! with different pin geometry (spec `interop.md` §6.4).
//!
//! [`apply`] is pure over one cell's geometry and nets and returns [`Edits`], a request
//! table; the side that owns the output applies it: `virtuoso::write` while emitting
//! records (export), [`apply_to_doc`] on a Schemify document (import, tests).

use schemify_format::{Document, FileKind, Orient, RecordKind};

use crate::design::{Geom, place};
use crate::nets::{NO_NODE, Nets, on_segment};

/// One grid step in milli-units (10 xschem units, 1/16 inch): pull-back and stub length.
pub const STEP: i32 = 10_000;

/// Re-mastered instances per cell, a bound on the edit tables.
pub const REMASTERED_MAX: usize = 1 << 16;

/// One instance changing master.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remaster {
    /// Index of the instance in [`Geom`]'s instance order.
    pub inst: usize,
    /// `(old master pin index, new master pin name)`: connections to carry. Old pins not
    /// listed lose their connection (reported by the caller).
    pub pins: Vec<(usize, String)>,
    /// The new master's pins (name, symbol-local point, Schemify coordinates) when its
    /// geometry is known; `None` asks the loader to connect (`conn` records).
    pub new_pins: Option<Vec<(String, [i32; 2])>>,
    /// Where the new master's origin goes when it is not the instance's
    /// ([`crate::align::fit`]: its pins then land on the old ones); `None`: in place.
    pub at: Option<[i32; 2]>,
}

/// A label to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLabel {
    /// Anchor, milli-units.
    pub at: [i32; 2],
    /// Net name (Schemify syntax).
    pub net: String,
    /// The net is global by a `global true` label.
    pub global: bool,
}

/// What the owner of the output adds or changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edits {
    /// `(wire index, end 0|1, new point)`.
    pub wire_end: Vec<(usize, usize, [i32; 2])>,
    /// Wires to delete (shorter than a step after the pull-back).
    pub wire_dead: Vec<usize>,
    /// Labels to add (on pulled-back ends and on points other objects still share).
    pub labels: Vec<NewLabel>,
    /// Stubs `(from pin, to, label)`: a wire from the new master's pin, labelled at `to`.
    pub stubs: Vec<([i32; 2], NewLabel)>,
    /// `(instance index, new terminal, net)` for masters of unknown geometry.
    pub conns: Vec<(usize, String, NewLabel)>,
    /// `(instance index, new origin)`: re-mastered instances placed by [`Remaster::at`].
    pub moved: Vec<(usize, [i32; 2])>,
}

/// Direction of `p` seen from `o` along its dominant axis (right when they coincide).
pub fn away(o: [i32; 2], p: [i32; 2]) -> [i32; 2] {
    let (dx, dy) = (
        i64::from(p[0]) - i64::from(o[0]),
        i64::from(p[1]) - i64::from(o[1]),
    );
    if dx == 0 && dy == 0 {
        [1, 0]
    } else if dx.abs() >= dy.abs() {
        [dx.signum() as i32, 0]
    } else {
        [0, dy.signum() as i32]
    }
}

/// The far end of a stub from `q`: one step in `first`, else the first other direction
/// whose segment touches no existing wire or node (a stub must not connect anything but
/// its label, §6.4 step 3); `first` again when every direction is taken.
fn stub_end(g: &Geom, n: &Nets, q: [i32; 2], first: [i32; 2]) -> [i32; 2] {
    let dirs = [
        first,
        [-first[1], first[0]],
        [first[1], -first[0]],
        [-first[0], -first[1]],
    ];
    for d in dirs {
        let to = [q[0] + d[0] * STEP, q[1] + d[1] * STEP];
        let seg = [
            q[0].min(to[0]),
            q[1].min(to[1]),
            q[0].max(to[0]),
            q[1].max(to[1]),
        ];
        let hits_wire = g.wire.iter().any(|w| {
            let (a, b) = ([w[0], w[1]], [w[2], w[3]]);
            on_segment(to, *w) || (a != q && on_segment(a, seg)) || (b != q && on_segment(b, seg))
        });
        let hits_node = n.node_at.iter().any(|p| *p != q && on_segment(*p, seg));
        if !hits_wire && !hits_node {
            return to;
        }
    }
    [q[0] + first[0] * STEP, q[1] + first[1] * STEP]
}

fn step_toward(p: [i32; 2], q: [i32; 2], len: f64) -> [i32; 2] {
    let f = f64::from(STEP) / len;
    [
        p[0] + ((f64::from(q[0]) - f64::from(p[0])) * f).round() as i32,
        p[1] + ((f64::from(q[1]) - f64::from(p[1])) * f).round() as i32,
    ]
}

/// Computes the edits that keep every net of the re-mastered instances `todo` (§6.4 steps
/// 1–2): wire ends on an old pin are pulled back one step and labelled with the pin's net;
/// a point that other objects still share keeps a label; the new master's pins get a stub
/// and label each (known geometry) or a `conn` request.
///
/// # Panics
///
/// When `todo` names an instance outside `g`, holds more than [`REMASTERED_MAX`] rows,
/// or when `n` was not extracted from `g`.
// ponytail: O(re-mastered pins × (wires + nodes)); a spatial index when a cell re-masters
// thousands of instances.
pub fn apply(g: &Geom, n: &Nets, todo: &[Remaster]) -> Edits {
    assert!(
        todo.len() <= REMASTERED_MAX,
        "too many re-mastered instances"
    );
    assert_eq!(
        n.inst_pin.len(),
        g.inst_id.len(),
        "nets of another geometry"
    );
    let mut e = Edits::default();
    let moving: Vec<usize> = todo.iter().map(|t| t.inst).collect();
    let mut ends: Vec<[Option<[i32; 2]>; 2]> = vec![[None, None]; g.wire.len()];
    for t in todo {
        assert!(t.inst < g.inst_id.len(), "instance {} out of range", t.inst);
        let k = t.inst;
        // The new master's origin: aligned by the caller, else the instance's.
        let base = t.at.unwrap_or(g.inst_at[k]);
        if let Some(at) = t.at {
            e.moved.push((k, at));
        }
        for (p, node) in n.inst_pin[k].iter().enumerate() {
            if *node == NO_NODE {
                continue;
            }
            let at = n.node_at[*node as usize];
            let net = n.node_net[*node as usize] as usize;
            // A new pin exactly where the old one was keeps its connection by geometry.
            let same_place = t
                .pins
                .iter()
                .find(|(op, _)| *op == p)
                .is_some_and(|(_, nm)| {
                    t.new_pins.as_ref().is_some_and(|pins| {
                        pins.iter().any(|(x, local)| {
                            x == nm && place(*local, g.inst_orient[k], base) == Some(at)
                        })
                    })
                });
            if same_place {
                continue;
            }
            let lab = NewLabel {
                at,
                net: n.net_name[net].clone(),
                global: n.net_global[net]
                    && n.net_name[net] != "0"
                    && !n.net_name[net].ends_with('!'),
            };
            let mut shared = false;
            for (w, seg) in g.wire.iter().enumerate() {
                let (a, b) = ([seg[0], seg[1]], [seg[2], seg[3]]);
                if a == at || b == at {
                    let (end, far) = if a == at { (0, b) } else { (1, a) };
                    let len = (f64::from(far[0] - at[0])).hypot(f64::from(far[1] - at[1]));
                    if len <= f64::from(STEP) {
                        e.wire_dead.push(w);
                        e.labels.push(NewLabel {
                            at: far,
                            ..lab.clone()
                        });
                    } else {
                        let np = step_toward(at, far, len);
                        ends[w][end] = Some(np);
                        e.labels.push(NewLabel {
                            at: np,
                            ..lab.clone()
                        });
                    }
                } else if on_segment(at, *seg) {
                    shared = true;
                }
            }
            // Other objects at the pin point that stay (labels, ports, noconns, pins of
            // instances keeping their master).
            for (v, vat) in n.node_at.iter().enumerate() {
                if *vat != at
                    || v as u32 == *node
                    || n.wire_ends.iter().any(|we| we.contains(&(v as u32)))
                {
                    continue;
                }
                let of_moving = n
                    .inst_pin
                    .iter()
                    .enumerate()
                    .any(|(i, pins)| moving.contains(&i) && pins.contains(&(v as u32)));
                if !of_moving {
                    shared = true;
                }
            }
            if shared {
                e.labels.push(lab.clone());
            }
            let Some((_, new_name)) = t.pins.iter().find(|(op, _)| *op == p) else {
                continue;
            };
            match &t.new_pins {
                Some(pins) => {
                    if let Some((_, local)) = pins.iter().find(|(nm, _)| nm == new_name) {
                        let q = place(*local, g.inst_orient[k], base).unwrap_or(base);
                        let to = stub_end(g, n, q, away(base, q));
                        e.stubs.push((
                            q,
                            NewLabel {
                                at: to,
                                ..lab.clone()
                            },
                        ));
                    }
                }
                None => e.conns.push((k, new_name.clone(), lab.clone())),
            }
        }
    }
    for (w, pair) in ends.iter().enumerate() {
        if e.wire_dead.contains(&w) {
            continue;
        }
        let seg = g.wire[w];
        let a = pair[0].unwrap_or([seg[0], seg[1]]);
        let b = pair[1].unwrap_or([seg[2], seg[3]]);
        let before = (i64::from(seg[2] - seg[0])) * i64::from(b[0] - a[0])
            + i64::from(seg[3] - seg[1]) * i64::from(b[1] - a[1]);
        if a == b || before <= 0 {
            e.wire_dead.push(w);
            continue;
        }
        for (end, p) in pair.iter().enumerate() {
            if let Some(p) = p {
                e.wire_end.push((w, end, *p));
            }
        }
    }
    e.wire_dead.sort_unstable();
    e.wire_dead.dedup();
    let mut seen = Vec::new();
    e.labels.retain(|l| {
        let fresh = !seen.contains(l);
        seen.push(l.clone());
        fresh
    });
    assert!(e.wire_end.iter().all(|(w, _, _)| !e.wire_dead.contains(w)));
    e
}

/// A replacement master for one instance: `(instance index, new cellref, new override
/// attributes)`; `None` attributes keep the instance's own.
pub type NewMaster = (usize, String, Option<Vec<(String, Vec<String>)>>);

/// Rebuilds `doc` (a `.shcomp`/`.shbench` whose geometry is `g`) with `e` applied and
/// instance masters replaced by `masters`: wires moved or deleted, labels and stub wires
/// added with ids from `new_id`. `conns` are not representable in a Schemify file and are
/// ignored here.
///
/// # Panics
///
/// When `g` is not `doc`'s geometry (record indices out of range).
pub fn apply_to_doc(
    doc: &Document<'_>,
    g: &Geom,
    e: &Edits,
    masters: &[NewMaster],
    mut new_id: impl FnMut() -> String,
) -> Document<'static> {
    assert!(matches!(doc.kind(), FileKind::Shcomp | FileKind::Shbench));
    let mut out = Document::new(doc.kind());
    for a in doc.attrs(0) {
        out.push_attr(doc.attr_key(a));
        for v in doc.attr_values(a) {
            out.push_value(v);
        }
    }
    let wire_of = |r: u32| g.wire_rec.iter().position(|x| *x == r);
    let inst_of = |r: u32| g.inst_rec.iter().position(|x| *x == r);
    for r in 1..doc.record_count() {
        if let Some(w) = wire_of(r) {
            if e.wire_dead.contains(&w) {
                continue;
            }
            let mut seg = g.wire[w];
            for (_, end, p) in e.wire_end.iter().filter(|x| x.0 == w) {
                seg[end * 2] = p[0];
                seg[end * 2 + 1] = p[1];
            }
            push_wire(&mut out, doc.field(r, 0).unwrap_or(""), seg, g.wire_bus[w]);
            continue;
        }
        if let Some((k, cref, attrs)) = inst_of(r).and_then(|k| masters.iter().find(|m| m.0 == k)) {
            out.begin_record(RecordKind::Inst);
            out.push_field(doc.field(r, 0).unwrap_or(""));
            out.push_field(cref);
            let moved = e.moved.iter().find(|m| m.0 == *k).map(|m| m.1);
            for a in doc.attrs(r) {
                let key = doc.attr_key(a);
                if attrs.is_some() && !matches!(key, "at" | "name" | "ignore" | "inherit") {
                    continue;
                }
                out.push_attr(key);
                match moved.filter(|_| key == "at") {
                    // Aligned (§6.4): the new origin, the orientation kept.
                    Some(at) => {
                        out.push_value_coord(at[0]);
                        out.push_value_coord(at[1]);
                        for v in doc.attr_values(a).skip(2) {
                            out.push_value(v);
                        }
                    }
                    None => {
                        for v in doc.attr_values(a) {
                            out.push_value(v);
                        }
                    }
                }
            }
            for (k, vs) in attrs.iter().flatten() {
                out.push_attr(k);
                for v in vs {
                    out.push_value(v);
                }
            }
            continue;
        }
        out.copy_record(doc, r);
    }
    let label = |out: &mut Document<'static>, l: &NewLabel, id: String| {
        out.begin_record(RecordKind::Label);
        out.push_field(&id);
        out.push_field_coord(l.at[0]);
        out.push_field_coord(l.at[1]);
        out.push_field(Orient::R0.as_str());
        out.push_field(&l.net);
        if l.global {
            out.push_attr("global");
            out.push_value("true");
        }
    };
    for l in &e.labels {
        label(&mut out, l, new_id());
    }
    for (from, l) in &e.stubs {
        let mut seg = [from[0], from[1], l.at[0], l.at[1]];
        if (seg[2], seg[3]) < (seg[0], seg[1]) {
            seg = [seg[2], seg[3], seg[0], seg[1]];
        }
        push_wire(&mut out, &new_id(), seg, false);
        label(&mut out, l, new_id());
    }
    assert!(out.record_count() >= 1);
    out
}

fn push_wire(out: &mut Document<'static>, id: &str, mut seg: [i32; 4], bus: bool) {
    if (seg[2], seg[3]) < (seg[0], seg[1]) {
        seg = [seg[2], seg[3], seg[0], seg[1]];
    }
    out.begin_record(RecordKind::Wire);
    out.push_field(id);
    for v in seg {
        out.push_field_coord(v);
    }
    if bus {
        out.push_attr("bus");
        out.push_value("true");
    }
}
