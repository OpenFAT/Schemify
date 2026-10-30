//! Geometry edits: connectivity patched per touched net (D39), against a fresh build.

use crate::common;
use schemify_doc::{Edit, Mode, Op, Outcome, RecordBuf, Target, Transaction};
use schemify_elab::{CellState, NetChange, no_funcs};
use schemify_format::RecordKind;

use crate::common::{DEV, Mix};

/// Names labels and ports take in the geometry tests: scalars, a bus and its bits, globals,
/// an interface pin, a repeat, and a port text that names no pin (`E012`).
const NET_NAMES: [&str; 10] = [
    "a", "b", "d[1:0]", "d[1]", "d[0]", "vdd!", "0", "P", "2*a", "Q",
];

fn grid(rng: &mut Mix) -> i64 {
    rng.range(-6, 6) * 10_000
}

/// A cell for geometry edits: interface pins `P` and `B[1:0]`, `DEV` instances in
/// every orientation, sparse wires, labels, ports and noconns on a coarse grid.
fn geo_cell(rng: &mut Mix) -> String {
    let orients = ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"];
    let mut s = String::from(
        "schemify 1 shcomp\n\npin P inout 0 0\npin B[1:0] inout 0 20\n\nimpl sch schematic\n\n",
    );
    for i in 0..5 {
        let o = orients[rng.below(8) as usize];
        let (x, y) = (grid(rng), grid(rng));
        s.push_str(&format!(
            "inst {} ./dev\n  at {} {} {o}\n  name X{i}\n",
            common::id(100 + i),
            common::c(x),
            common::c(y)
        ));
    }
    for k in 0..12 {
        let (x, y) = (grid(rng), grid(rng));
        let len = rng.range(1, 4) * 10_000;
        let (x2, y2) = if rng.chance(50) {
            (x + len, y)
        } else {
            (x, y + len)
        };
        s.push_str(&format!(
            "wire {} {} {} {} {}\n",
            common::id(200 + k),
            common::c(x),
            common::c(y),
            common::c(x2),
            common::c(y2)
        ));
    }
    for k in 0..3 {
        let name = NET_NAMES[rng.below(7) as usize];
        s.push_str(&format!(
            "label {} {} {} R0 {name}\n",
            common::id(300 + k),
            common::c(grid(rng)),
            common::c(grid(rng))
        ));
    }
    s.push_str(&format!(
        "port {} {} {} R0 P\nport {} {} {} R0 B[1:0]\nnoconn {} {} {}\n",
        common::id(400),
        common::c(grid(rng)),
        common::c(grid(rng)),
        common::id(401),
        common::c(grid(rng)),
        common::c(grid(rng)),
        common::id(402),
        common::c(grid(rng)),
        common::c(grid(rng))
    ));
    s
}

/// The id text of a random row of table `kind`, if it has rows.
fn pick(rng: &mut Mix, doc: &schemify_doc::Document, kind: RecordKind) -> Option<String> {
    let ids = match kind {
        RecordKind::Wire => &doc.wires().id,
        RecordKind::Label => &doc.labels().id,
        RecordKind::Port => &doc.ports().id,
        RecordKind::Noconn => &doc.noconns().id,
        _ => &doc.instances().id,
    };
    (!ids.is_empty()).then(|| ids[rng.below(ids.len() as u64) as usize].text(doc.strings()))
}

/// One random geometry edit (1–2 ops): wires joining, splitting and moving nets, labels
/// merging or splitting nets by name, bus labels, globals, ports, noconns, instance moves.
fn geo_ops(rng: &mut Mix, doc: &schemify_doc::Document) -> Vec<Op> {
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(2) {
        let name = NET_NAMES[rng.below(NET_NAMES.len() as u64) as usize];
        let set = |kind, key: String, index, value: String| Op::SetField {
            target: Target::new(kind, &key),
            index,
            value,
        };
        let op = match rng.below(16) {
            0..=2 => {
                let (x, y) = (grid(rng), grid(rng));
                let (dx, dy) = match rng.below(5) {
                    0 => (rng.range(-3, 3) * 10_000, rng.range(-3, 3) * 10_000),
                    1 | 2 => (rng.range(1, 4) * 10_000, 0),
                    _ => (0, rng.range(1, 4) * 10_000),
                };
                Some(Op::Insert {
                    rec: RecordBuf::new(RecordKind::Wire)
                        .field("-")
                        .field(&common::c(x))
                        .field(&common::c(y))
                        .field(&common::c(x + dx))
                        .field(&common::c(y + dy)),
                    at: None,
                })
            }
            3 | 4 => pick(rng, doc, RecordKind::Wire)
                .map(|k| Op::Delete(Target::new(RecordKind::Wire, &k))),
            5 => pick(rng, doc, RecordKind::Wire).map(|k| {
                set(
                    RecordKind::Wire,
                    k,
                    1 + rng.below(4) as u32,
                    common::c(grid(rng)),
                )
            }),
            6 | 7 => Some(Op::Insert {
                rec: RecordBuf::new(RecordKind::Label)
                    .field("-")
                    .field(&common::c(grid(rng)))
                    .field(&common::c(grid(rng)))
                    .field("R0")
                    .field(name),
                at: None,
            }),
            8 => {
                pick(rng, doc, RecordKind::Label).map(|k| set(RecordKind::Label, k, 4, name.into()))
            }
            9 => pick(rng, doc, RecordKind::Label).map(|k| {
                set(
                    RecordKind::Label,
                    k,
                    1 + rng.below(2) as u32,
                    common::c(grid(rng)),
                )
            }),
            10 => pick(rng, doc, RecordKind::Label)
                .map(|k| Op::Delete(Target::new(RecordKind::Label, &k))),
            11 => match pick(rng, doc, RecordKind::Noconn) {
                Some(k) if rng.chance(50) => Some(Op::Delete(Target::new(RecordKind::Noconn, &k))),
                _ => Some(Op::Insert {
                    rec: RecordBuf::new(RecordKind::Noconn)
                        .field("-")
                        .field(&common::c(grid(rng)))
                        .field(&common::c(grid(rng))),
                    at: None,
                }),
            },
            12 => {
                let text = ["P", "B[1:0]", "Q"][rng.below(3) as usize];
                match pick(rng, doc, RecordKind::Port) {
                    Some(k) if rng.chance(30) => {
                        Some(Op::Delete(Target::new(RecordKind::Port, &k)))
                    }
                    Some(k) if rng.chance(50) => Some(set(RecordKind::Port, k, 4, text.into())),
                    Some(k) => Some(set(RecordKind::Port, k, 1, common::c(grid(rng)))),
                    None => None,
                }
            }
            13 if rng.chance(50) => pick(rng, doc, RecordKind::Inst).map(|k| Op::SetAttr {
                target: Target::new(RecordKind::Inst, &k),
                key: "name".into(),
                values: vec![["X0", "X9", "Y[1:0]", "X1"][rng.below(4) as usize].into()],
            }),
            13 | 14 => pick(rng, doc, RecordKind::Inst).map(|k| {
                let o = ["R0", "R90", "R180", "MR270"][rng.below(4) as usize];
                Op::SetAttr {
                    target: Target::new(RecordKind::Inst, &k),
                    key: "at".into(),
                    values: vec![common::c(grid(rng)), common::c(grid(rng)), o.into()],
                }
            }),
            _ => pick(rng, doc, RecordKind::Label).map(|k| {
                let target = Target::new(RecordKind::Label, &k);
                if rng.chance(50) {
                    Op::SetAttr {
                        target,
                        key: "global".into(),
                        values: vec!["true".into()],
                    }
                } else {
                    Op::Put {
                        target,
                        rec: RecordBuf::new(RecordKind::Label)
                            .field(&k)
                            .field(&common::c(grid(rng)))
                            .field(&common::c(grid(rng)))
                            .field("R0")
                            .field(name),
                    }
                }
            }),
        };
        ops.extend(op);
    }
    ops
}

/// Everything connectivity derives, as text keyed by object id (handles canonicalized):
/// the partition, each object's net (name, width, global, pin count, bit names), every
/// connection's bit and its connection count, the live nets and bits, and the name index.
fn nets_view(st: &CellState, doc: &schemify_doc::Document) -> Vec<String> {
    let (n, s) = (&st.nets, doc.strings());
    let name = |x: schemify_elab::Name| st.names.get(x).to_owned();
    let bits = |c: u32| -> Vec<String> {
        n.bits_of(c as usize)
            .iter()
            .map(|&b| name(n.bit_name[b as usize]))
            .collect()
    };
    let net = |c: u32| {
        format!(
            "{} w{} g{} p{} {:?}",
            name(n.net_name[c as usize]),
            n.net_width[c as usize],
            n.net_global[c as usize],
            n.net_pins[c as usize],
            bits(c)
        )
    };
    let mut out: Vec<String> = common::canon(&common::real_partition(doc, n))
        .into_iter()
        .map(|(k, b)| format!("block {k} {b}"))
        .collect();
    for (r, &c) in n.wire_net.iter().enumerate() {
        out.push(format!("w:{} {}", doc.wires().id[r].text(s), net(c)));
    }
    for (r, &c) in n.label_net.iter().enumerate() {
        out.push(format!("l:{} {}", doc.labels().id[r].text(s), net(c)));
    }
    for (r, &c) in n.port_net.iter().enumerate() {
        out.push(format!("p:{} {}", doc.ports().id[r].text(s), net(c)));
    }
    for i in 0..doc.instances().id.len() {
        for (k, &c) in n.pins(i).iter().enumerate() {
            out.push(format!(
                "i:{}:{k} {}",
                doc.instances().id[i].text(s),
                net(c)
            ));
        }
    }
    for (k, &b) in n.conns.iter().enumerate() {
        out.push(match b {
            u32::MAX => format!("conn {k} -"),
            b => format!(
                "conn {k} {} {}",
                name(n.bit_name[b as usize]),
                n.bit_conns[b as usize]
            ),
        });
    }
    for &b in n.port_bits.iter().chain(&n.inh_bits) {
        out.push(format!("port bit {}", name(n.bit_name[b as usize])));
    }
    let mut live: Vec<String> = (0..n.net_name.len() as u32)
        .filter(|&c| n.net_width[c as usize] > 0)
        .map(|c| format!("net {}", net(c)))
        .collect();
    live.extend(
        (0..n.bit_name.len())
            .filter(|&b| n.bit_name[b] != schemify_elab::Name::NONE)
            .map(|b| {
                format!(
                    "bit {} g{} c{}",
                    name(n.bit_name[b]),
                    n.bit_global[b],
                    n.bit_conns[b]
                )
            }),
    );
    live.extend(n.by_name.iter().map(|r| {
        format!(
            "by_name {} {}",
            name(r.name),
            name(n.bit_name[r.bit as usize])
        )
    }));
    live.sort();
    out.extend(live);
    out
}

/// Random geometry edit sequences, with undo and redo, through `doc::apply_edits`: after
/// every step the patched state equals a fresh build (connectivity, names, values,
/// violations) and the pass's delta turns the previous rows into the current ones.
/// Returns how many steps were patched (not rebuilt).
fn geometry_run(seed: u64, steps: usize) -> usize {
    let mut rng = Mix(seed);
    let cell = geo_cell(&mut rng);
    let (mut p, hs) = common::project(&[("dev.shprim", DEV), ("cell.shcomp", &cell)]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).expect("a live handle builds");
    let mut patched = 0;
    for step in 0..steps {
        let ops = if rng.chance(20) {
            let Some(ops) = super::revert(&mut p, h, &mut rng) else {
                continue;
            };
            ops
        } else {
            let tx = Transaction {
                label: "g".into(),
                mode: Mode::Commit,
                ops: geo_ops(&mut rng, p.doc(h).expect("a live handle has a document")),
            };
            let done = super::edit(&mut p, h, Edit::Tx(tx));
            // A rejected transaction still permutes rows (its replayed inverses
            // re-insert deletions at the end): the next step must cope.
            if !matches!(done.outcome, Outcome::Committed { .. }) {
                continue;
            }
            done.ops
        };
        let (before, pass) = (st.violations.rows.clone(), st.pass);
        let done = st.recompute(&p, h, &ops, &mut no_funcs);
        patched += usize::from(done.nets == NetChange::Patched);
        let fresh = CellState::build(&p, h, &mut no_funcs).expect("a live handle builds");
        let doc = p.doc(h).expect("a live handle has a document");
        assert_eq!(
            nets_view(&st, doc),
            nets_view(&fresh, doc),
            "seed {seed} step {step} ops {ops:?}"
        );
        assert_eq!(
            super::snapshot(&st),
            super::snapshot(&fresh),
            "seed {seed} step {step}"
        );
        if st.prev_pass == 0 {
            continue;
        }
        assert_eq!(st.prev_pass, pass, "passes chain");
        let mut rows = before;
        for g in &st.changes.gone {
            let at = rows
                .iter()
                .position(|r| r == g)
                .expect("a removed row existed");
            rows.remove(at);
        }
        rows.extend(st.changes.added.iter().cloned());
        let text = |v: &[schemify_elab::Violation]| {
            let mut t: Vec<String> = v.iter().map(|r| format!("{r:?}")).collect();
            t.sort();
            t
        };
        assert_eq!(
            text(&rows),
            text(&st.violations.rows),
            "seed {seed} step {step} delta"
        );
    }
    patched
}

#[test]
fn patched_connectivity_equals_a_full_build_after_random_geometry_edits() {
    let patched: usize = (0..64).map(|seed| geometry_run(seed, 60)).sum();
    // Most steps patch; the rest rebuild (E003 shorts, `global true` labels).
    assert!(patched > 1500, "patched {patched} of 3840 steps");
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 32, ..Default::default() })]

    /// The same oracle over arbitrary seeds (shrinks to the seed).
    #[test]
    fn geometry_edits_patch_like_a_full_build(seed in proptest::prelude::any::<u64>()) {
        geometry_run(seed, 30);
    }
}

/// Negative space: an edit far from a net leaves its handle, name and bits alone, and a
/// patch touches only the nets on the edited objects.
#[test]
fn an_edit_far_away_keeps_every_other_net() {
    // Two clusters 1000 units apart: a chain of wires with labels on the left, the same on
    // the right; the edits happen on the right only.
    let mut s = String::from("schemify 1 shcomp\n\npin P inout 0 0\n\nimpl sch schematic\n\n");
    for side in [0i64, 1_000_000] {
        for k in 0..20i64 {
            let (x, y) = (side + k * 20_000, 0);
            s.push_str(&format!(
                "wire {} {} {} {} {}\n",
                common::id((side / 1000 + k) as u64 + 10),
                common::c(x),
                common::c(y),
                common::c(x),
                common::c(y + 10_000)
            ));
            if k % 4 == 0 {
                s.push_str(&format!(
                    "label {} {} {} R0 n{k}\n",
                    common::id((side / 1000 + k) as u64 + 5000),
                    common::c(x),
                    common::c(y)
                ));
            }
        }
    }
    // The port sits apart: on a labelled wire it would short two names (E003, a rebuild).
    s.push_str(&format!(
        "port {} {} 0 R0 P\n",
        common::id(9_999),
        common::c(-100_000)
    ));
    let (mut p, hs) = common::project(&[("dev.shprim", DEV), ("cell.shcomp", &s)]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let left = |st: &CellState, doc: &schemify_doc::Document| -> Vec<(u32, String, Vec<u32>)> {
        (0..doc.wires().id.len())
            .filter(|&w| doc.wires().x1[w] < 500_000)
            .map(|w| {
                let c = st.nets.wire_net[w];
                let name = st.names.get(st.nets.net_name[c as usize]).to_owned();
                (c, name, st.nets.bits_of(c as usize).to_vec())
            })
            .collect()
    };
    let before = left(&st, p.doc(h).unwrap());
    let right = |k: i64| common::id((1_000_000 / 1000 + k) as u64 + 10);
    let edits = [
        // Join two right-hand nets, rename a right-hand label to a left-hand name (merge by
        // name), delete a right-hand wire, insert one.
        vec![Op::Insert {
            rec: RecordBuf::new(RecordKind::Wire)
                .field("-")
                .field(&common::c(1_000_000))
                .field(&common::c(10_000))
                .field(&common::c(1_040_000))
                .field(&common::c(10_000)),
            at: None,
        }],
        vec![Op::SetField {
            target: Target::new(RecordKind::Label, &common::id(1_000 + 4 + 5000)),
            index: 4,
            value: "n0".into(),
        }],
        vec![Op::Delete(Target::new(RecordKind::Wire, &right(7)))],
    ];
    for ops in edits {
        assert!(super::commit(&mut p, h, ops.clone()), "{ops:?}");
        let done = st.recompute(&p, h, &ops, &mut no_funcs);
        assert_eq!(done.nets, NetChange::Patched, "{ops:?}");
        let doc = p.doc(h).unwrap();
        assert_eq!(
            left(&st, doc),
            before,
            "the left cluster is untouched: {ops:?}"
        );
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        assert_eq!(nets_view(&st, doc), nets_view(&fresh, doc));
    }
}
