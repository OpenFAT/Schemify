//! D19 oracle (docs/dev/testing.md `expr`/`elab`): after any sequence of edits, undos and redos
//! applied with `doc::apply_edits` (`recompute` takes the ops `Project::applied` lends),
//! the incrementally recomputed state equals a state built from scratch — values,
//! unverified flags, nets and violations.

mod common;

use common::*;
use schemify_doc::{
    DocHandle, Edit, EditRequest, Mode, Op, Outcome, Project, RecordBuf, Target, Transaction,
    apply_edits,
};
use schemify_elab::{CellState, no_funcs};
use schemify_format::RecordKind;

const DEV: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 20 0\n\nparam w 1\n  min 0.5\n  max 8\nparam l w*2\n  grid 0.5\nparam ad 0\n  computed w*l\n\nimpl spice spice\n";

/// A cell with vars, an own param, and `n` instances with overrides that read them.
fn cell(n: usize) -> String {
    let mut s =
        String::from("schemify 1 shcomp\n\npin P inout 0 0\n\nparam p 3\n\nimpl sch schematic\n\n");
    for i in 0..n {
        let ovr = match i % 4 {
            0 => "  w a+k2\n".to_owned(),
            1 => "  w b*2\n  l c\n".to_owned(),
            2 => "  l p+a\n".to_owned(),
            _ => "  w 2\n".to_owned(),
        };
        s.push_str(&format!(
            "inst {} ./dev\n  at {} 0 R0\n  name M{i}\n{ovr}",
            id(i as u64 + 1),
            c(i as i64 * 40_000)
        ));
    }
    s.push_str(&format!("\nwire {} 0 0 20 0\n\nport {} 0 0 R0 P\n\nvar a 1\nvar b a+1\nvar c b*a+k1\nvar k1 5\nvar k2 7\n", id(9_001), id(9_002)));
    s
}

fn commit(p: &mut Project, h: DocHandle, ops: Vec<Op>) -> bool {
    let tx = Transaction {
        label: "t".into(),
        mode: Mode::Commit,
        ops,
    };
    matches!(edit(p, h, Edit::Tx(tx)).outcome, Outcome::Committed { .. })
}

/// One request's outcome and the ops it applied.
struct Applied {
    outcome: Outcome,
    ops: Vec<Op>,
}

/// Applies one request; its applied ops (`Project::applied`: a commit's resolved ops, an
/// undo's inverse ops, a redo's ops) are what the editor hands `recompute`.
fn edit(p: &mut Project, h: DocHandle, edit: Edit) -> Applied {
    let mut out = apply_edits(
        p,
        &[EditRequest {
            doc: h,
            edit,
            unix_ms: 1,
        }],
    );
    let outcome = out.pop().expect("one outcome per request");
    let ops = p.applied(h, &outcome).map_or_else(Vec::new, <[Op]>::to_vec);
    Applied { outcome, ops }
}

/// The ops of a random undo or redo, or `None` when there was nothing to revert.
fn revert(p: &mut Project, h: DocHandle, rng: &mut Mix) -> Option<Vec<Op>> {
    let e = if rng.chance(50) {
        Edit::Undo
    } else {
        Edit::Redo
    };
    let done = edit(p, h, e);
    matches!(
        done.outcome,
        Outcome::Undone { .. } | Outcome::Redone { .. }
    )
    .then_some(done.ops)
}

/// Everything comparable between two states, as text.
fn snapshot(st: &CellState) -> (Vec<String>, Vec<bool>, Vec<String>, Vec<String>) {
    let vals = st.graph.vals.iter().map(|v| format!("{v:?}")).collect();
    let nets = st
        .nets
        .conns
        .iter()
        .map(|&b| {
            if b == u32::MAX {
                "-".into()
            } else {
                st.names.get(st.nets.bit_name[b as usize]).to_owned()
            }
        })
        .collect();
    let n = |x: schemify_elab::Name| {
        if x == schemify_elab::Name::NONE {
            String::new()
        } else {
            st.names.get(x).to_owned()
        }
    };
    let viol = st
        .violations
        .rows
        .iter()
        .map(|v| {
            format!(
                "{} {} {} {} {} {}",
                v.code.as_str(),
                n(v.cell),
                v.object.0,
                n(v.path),
                n(v.param),
                v.detail
            )
        })
        .collect();
    (vals, st.graph.unverified.clone(), nets, viol)
}

/// Every node's value and `unverified` flag, as text.
fn node_vals(st: &CellState) -> Vec<String> {
    let g = &st.graph;
    (0..g.len())
        .map(|n| format!("{:?} {}", g.vals[n], g.unverified[n]))
        .collect()
}

/// The instance rows owning a node whose text differs from `before` (same graph).
fn revalued(st: &CellState, before: &[String]) -> Vec<u32> {
    let now = node_vals(st);
    assert_eq!(now.len(), before.len(), "same nodes");
    let mut rows: Vec<u32> = (0..now.len())
        .filter(|&n| now[n] != before[n])
        .map(|n| st.graph.owner[n])
        .filter(|&o| o != u32::MAX)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

fn random_op(rng: &mut Mix, n: usize) -> Op {
    let exprs = [
        "1", "2", "a", "b*2", "c+p", "0.25", "a+b", "p/2", "9", "b", "nope", "c",
    ];
    let pick = |rng: &mut Mix| exprs[rng.below(exprs.len() as u64) as usize].to_owned();
    let inst = |rng: &mut Mix| Target::new(RecordKind::Inst, &id(rng.below(n as u64) + 1));
    // Half the edits are what users mostly do: a new constant for a var or an override
    // that already is a constant (dependencies unchanged, the incremental path).
    if rng.chance(50) {
        let value = format!("{}", rng.range(1, 9));
        if rng.chance(50) {
            let var = ["k1", "k2"][rng.below(2) as usize];
            return Op::SetField {
                target: Target::new(RecordKind::Var, var),
                index: 1,
                value,
            };
        }
        let i = rng.below((n / 4) as u64) * 4 + 3; // M3, M7, …: `w 2`
        return Op::SetAttr {
            target: Target::new(RecordKind::Inst, &id(i + 1)),
            key: "w".into(),
            values: vec![value],
        };
    }
    match rng.below(10) {
        0..=2 => {
            let var = ["a", "b", "c"][rng.below(3) as usize];
            Op::SetField {
                target: Target::new(RecordKind::Var, var),
                index: 1,
                value: pick(rng),
            }
        }
        3..=5 => Op::SetAttr {
            target: inst(rng),
            key: ["w", "l"][rng.below(2) as usize].to_owned(),
            values: vec![pick(rng)],
        },
        6 => Op::RemoveAttr {
            target: inst(rng),
            key: ["w", "l"][rng.below(2) as usize].to_owned(),
            first: None,
        },
        7 => Op::SetAttr {
            target: inst(rng),
            key: "at".to_owned(),
            values: vec![
                c(rng.range(-3, 3) * 20_000),
                "0".to_owned(),
                "R0".to_owned(),
            ],
        },
        8 => Op::Insert {
            rec: RecordBuf::new(RecordKind::Wire)
                .field("-")
                .field(&c(rng.range(-3, 3) * 20_000))
                .field("0")
                .field(&c(rng.range(4, 8) * 20_000))
                .field("0"),
            at: None,
        },
        _ => Op::SetField {
            target: Target::new(RecordKind::Param, "p"),
            index: 1,
            value: pick(rng),
        },
    }
}

#[test]
fn incremental_recompute_equals_full_rebuild_after_random_edits() {
    let (mut incremental, mut reverted) = (0, 0);
    for seed in 0..12u64 {
        let n = 24;
        let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &cell(n))]);
        let h = hs[1];
        let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
        let mut rng = Mix(seed);
        for step in 0..60 {
            let ops = if rng.chance(25) {
                let Some(ops) = revert(&mut p, h, &mut rng) else {
                    continue;
                };
                reverted += 1;
                ops
            } else {
                let ops: Vec<Op> = (0..1 + rng.below(3))
                    .map(|_| random_op(&mut rng, n))
                    .collect();
                let tx = Transaction {
                    label: "t".into(),
                    mode: Mode::Commit,
                    ops,
                };
                let done = edit(&mut p, h, Edit::Tx(tx));
                if !matches!(done.outcome, Outcome::Committed { .. }) {
                    continue;
                }
                done.ops
            };
            let (before, pass) = (st.violations.rows.clone(), st.pass);
            let vals0 = node_vals(&st);
            let done = st.recompute(&p, h, &ops, &mut no_funcs);
            if done.evaluated > 0 {
                incremental += 1;
            }
            if !done.graph {
                // The value delta names exactly the instances whose values changed.
                assert_eq!(
                    st.revalued,
                    revalued(&st, &vals0),
                    "seed {seed} step {step} ops {ops:?}"
                );
            } else {
                assert!(st.revalued.is_empty(), "a rebuild reports no delta");
            }
            let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
            assert_eq!(
                snapshot(&st),
                snapshot(&fresh),
                "seed {seed} step {step} ops {ops:?}"
            );
            // The pass's delta turns the previous rows into the current ones (a rebuilt
            // state starts a new chain: `prev_pass == 0`, consumers take every row).
            if st.prev_pass == 0 {
                continue;
            }
            assert_eq!(st.prev_pass, pass, "passes chain");
            let mut rows = before;
            for g in &st.changes.0 {
                let at = rows
                    .iter()
                    .position(|r| r == g)
                    .expect("a removed row existed");
                rows.remove(at);
            }
            rows.extend(st.changes.1.iter().cloned());
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
    }
    assert!(
        incremental > 50,
        "the incremental path ran {incremental} times"
    );
    assert!(reverted > 40, "{reverted} undos and redos");
}

/// An override added or removed (the properties dialog's first edit of a param) keeps the
/// graph: the node takes its new program and dependencies, only its dependents are
/// evaluated, and the state equals a fresh build. Negative space: a var that would read a
/// later node (here a cycle) rebuilds.
#[test]
fn adding_or_removing_an_override_keeps_the_graph() {
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &cell(40))]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let m3 = Target::new(RecordKind::Inst, &id(4));
    let edits = [
        // M3 has `w 2`: a new `l` override reading vars, then `w` dropped (back to the
        // cell's default `1`), then `w` again with a var.
        Op::SetAttr {
            target: m3.clone(),
            key: "l".into(),
            values: vec!["a+k1".into()],
        },
        Op::RemoveAttr {
            target: m3.clone(),
            key: "w".into(),
            first: None,
        },
        Op::SetAttr {
            target: m3,
            key: "w".into(),
            values: vec!["b".into()],
        },
    ];
    for op in edits {
        assert!(commit(&mut p, h, vec![op.clone()]));
        let done = st.recompute(&p, h, std::slice::from_ref(&op), &mut no_funcs);
        assert!(!done.graph, "{op:?} keeps the graph");
        assert!(done.evaluated > 0 && done.evaluated < 8, "{op:?}: {done:?}");
        assert_eq!(st.revalued, [3], "{op:?}: only M3's values changed");
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        assert_eq!(snapshot(&st), snapshot(&fresh), "{op:?}");
    }
    let op = Op::SetField {
        target: Target::new(RecordKind::Var, "a"),
        index: 1,
        value: "c".into(),
    };
    assert!(commit(&mut p, h, vec![op.clone()]));
    let done = st.recompute(&p, h, &[op], &mut no_funcs);
    assert!(done.graph, "a cycle rebuilds");
    let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
    assert_eq!(snapshot(&st), snapshot(&fresh));
}

#[test]
fn a_value_edit_reevaluates_only_its_dependents() {
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &cell(40))]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let total = st.graph.len();
    let op = Op::SetField {
        target: Target::new(RecordKind::Var, "c"),
        index: 1,
        value: "b*a+k1*3".into(),
    };
    assert!(commit(&mut p, h, vec![op.clone()]));
    let done = st.recompute(&p, h, &[op], &mut no_funcs);
    assert!(!done.graph && !done.nets);
    // `c` feeds `l` of M1, M5, …: c itself + 10 overrides + their computed `ad`.
    assert_eq!(done.evaluated, 1 + 10 + 10, "of {total} nodes");
}

/// D20 write-back: the app copies plugin results into `cached` attributes. Such an edit is
/// value-only: no graph rebuild, the stand-in reaches the value when the plugin is missing,
/// and the result equals a fresh build.
#[test]
fn cached_attribute_edits_refresh_without_a_rebuild() {
    use schemify_elab::Val;
    let src = format!(
        "{}var g \"gmid.w(a)\"\n  cached 2 00aa\n",
        cell(8).replace("  w a+k2\n", "  w g\n")
    );
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &src)]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let g = |st: &CellState| {
        let v = st
            .graph
            .var_name
            .iter()
            .position(|n| st.names.get(*n) == "g")
            .unwrap();
        let node = st.graph.params as usize + v;
        (st.graph.vals[node].clone(), st.graph.unverified[node])
    };
    assert_eq!(
        g(&st),
        (Val::Int(2), true),
        "missing plugin: cached, unverified"
    );
    let m0_w = st.graph.inst_param(0, 0).unwrap() as usize;
    assert!(
        st.graph.unverified[m0_w],
        "a dependent of a stand-in is unverified"
    );
    let set = Op::SetAttr {
        target: Target::new(RecordKind::Var, "g"),
        key: "cached".into(),
        values: vec!["3.5".into(), "00bb".into()],
    };
    let remove = Op::RemoveAttr {
        target: Target::new(RecordKind::Var, "g"),
        key: "cached".into(),
        first: None,
    };
    for (op, want) in [(set, Some(Val::Num(3.5))), (remove, None)] {
        assert!(commit(&mut p, h, vec![op.clone()]));
        let done = st.recompute(&p, h, std::slice::from_ref(&op), &mut no_funcs);
        assert!(!done.graph && !done.nets, "{done:?}");
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        assert_eq!(snapshot(&st), snapshot(&fresh), "{op:?}");
        match want {
            Some(v) => {
                assert_eq!(g(&st), (v.clone(), true));
                assert_eq!(st.graph.vals[m0_w], v, "the override reads the stand-in");
            }
            None => assert!(
                matches!(g(&st).0, Val::Err(e) if e.code == schemify_expr::Code::X006),
                "no plugin, no cache: X006"
            ),
        }
    }
    // A loaded plugin wins over any cache, and a cache write never re-runs its value.
    let mut funcs = |name: &str,
                     args: &[schemify_expr::Value<'_>]|
     -> Option<Result<schemify_expr::Value<'static>, schemify_expr::Error>> {
        (name == "gmid.w").then(|| match args[0] {
            schemify_expr::Value::Int(i) => Ok(schemify_expr::Value::Num(i as f64 * 10.0)),
            _ => Ok(schemify_expr::Value::Num(-1.0)),
        })
    };
    let mut st = CellState::build(&p, h, &mut funcs).unwrap();
    assert_eq!(g(&st), (Val::Num(10.0), false));
    let op = Op::SetAttr {
        target: Target::new(RecordKind::Var, "g"),
        key: "cached".into(),
        values: vec!["10".into(), "00cc".into()],
    };
    assert!(commit(&mut p, h, vec![op.clone()]));
    let done = st.recompute(&p, h, &[op], &mut funcs);
    assert_eq!(
        (done.graph, done.evaluated),
        (false, 0),
        "verified values stay"
    );
    assert_eq!(g(&st), (Val::Num(10.0), false));
}

/// The same for an own param's default (D20 write-back of params): a `cached` edit on a param
/// record is value-only, its stand-in reaches the instances that read the param, and the
/// result equals a fresh build.
#[test]
fn cached_param_edits_refresh_without_a_rebuild() {
    use schemify_elab::Val;
    let src = cell(8)
        .replace(
            "param p 3\n",
            "param p 3\nparam q \"gmid.w(p)\"\n  cached 4 00aa\n",
        )
        .replace("  l p+a\n", "  l q\n");
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &src)]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let q = |st: &CellState| (st.graph.vals[1].clone(), st.graph.unverified[1]);
    assert_eq!(
        q(&st),
        (Val::Int(4), true),
        "missing plugin: cached, unverified"
    );
    let m2_l = st.graph.inst_param(2, 1).unwrap() as usize;
    assert_eq!(
        st.graph.vals[m2_l],
        Val::Int(4),
        "the override reads the stand-in"
    );
    let target = Target::new(RecordKind::Param, "q");
    let set = Op::SetAttr {
        target: target.clone(),
        key: "cached".into(),
        values: vec!["2.5".into(), "00bb".into()],
    };
    let remove = Op::RemoveAttr {
        target,
        key: "cached".into(),
        first: None,
    };
    for (op, want) in [(set, Some(Val::Num(2.5))), (remove, None)] {
        assert!(commit(&mut p, h, vec![op.clone()]));
        let done = st.recompute(&p, h, std::slice::from_ref(&op), &mut no_funcs);
        assert!(!done.graph && !done.nets, "{done:?}");
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        assert_eq!(snapshot(&st), snapshot(&fresh), "{op:?}");
        match want {
            Some(v) => assert_eq!(q(&st), (v, true)),
            None => assert!(
                matches!(q(&st).0, Val::Err(e) if e.code == schemify_expr::Code::X006),
                "no plugin, no cache: X006"
            ),
        }
    }
}

// ---- geometry edits: connectivity patched per touched net (D39) -------------------------

/// Names labels and ports take in the geometry tests: scalars, a bus and its bits, globals,
/// an interface pin, a repeat, and a port text that names no pin (`E012`).
const NET_NAMES: [&str; 10] = [
    "a", "b", "d[1:0]", "d[1]", "d[0]", "vdd!", "0", "P", "2*a", "Q",
];

fn grid(rng: &mut Mix) -> i64 {
    rng.range(-6, 6) * 10_000
}

/// A cell for geometry edits: interface pins `P` and `B[1:0]`, `common::DEV` instances in
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
            id(100 + i),
            c(x),
            c(y)
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
            id(200 + k),
            c(x),
            c(y),
            c(x2),
            c(y2)
        ));
    }
    for k in 0..3 {
        let name = NET_NAMES[rng.below(7) as usize];
        s.push_str(&format!(
            "label {} {} {} R0 {name}\n",
            id(300 + k),
            c(grid(rng)),
            c(grid(rng))
        ));
    }
    s.push_str(&format!(
        "port {} {} {} R0 P\nport {} {} {} R0 B[1:0]\nnoconn {} {} {}\n",
        id(400),
        c(grid(rng)),
        c(grid(rng)),
        id(401),
        c(grid(rng)),
        c(grid(rng)),
        id(402),
        c(grid(rng)),
        c(grid(rng))
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
                        .field(&c(x))
                        .field(&c(y))
                        .field(&c(x + dx))
                        .field(&c(y + dy)),
                    at: None,
                })
            }
            3 | 4 => pick(rng, doc, RecordKind::Wire)
                .map(|k| Op::Delete(Target::new(RecordKind::Wire, &k))),
            5 => pick(rng, doc, RecordKind::Wire)
                .map(|k| set(RecordKind::Wire, k, 1 + rng.below(4) as u32, c(grid(rng)))),
            6 | 7 => Some(Op::Insert {
                rec: RecordBuf::new(RecordKind::Label)
                    .field("-")
                    .field(&c(grid(rng)))
                    .field(&c(grid(rng)))
                    .field("R0")
                    .field(name),
                at: None,
            }),
            8 => {
                pick(rng, doc, RecordKind::Label).map(|k| set(RecordKind::Label, k, 4, name.into()))
            }
            9 => pick(rng, doc, RecordKind::Label)
                .map(|k| set(RecordKind::Label, k, 1 + rng.below(2) as u32, c(grid(rng)))),
            10 => pick(rng, doc, RecordKind::Label)
                .map(|k| Op::Delete(Target::new(RecordKind::Label, &k))),
            11 => match pick(rng, doc, RecordKind::Noconn) {
                Some(k) if rng.chance(50) => Some(Op::Delete(Target::new(RecordKind::Noconn, &k))),
                _ => Some(Op::Insert {
                    rec: RecordBuf::new(RecordKind::Noconn)
                        .field("-")
                        .field(&c(grid(rng)))
                        .field(&c(grid(rng))),
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
                    Some(k) => Some(set(RecordKind::Port, k, 1, c(grid(rng)))),
                    None => None,
                }
            }
            13 | 14 => pick(rng, doc, RecordKind::Inst).map(|k| {
                let o = ["R0", "R90", "R180", "MR270"][rng.below(4) as usize];
                Op::SetAttr {
                    target: Target::new(RecordKind::Inst, &k),
                    key: "at".into(),
                    values: vec![c(grid(rng)), c(grid(rng)), o.into()],
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
                            .field(&c(grid(rng)))
                            .field(&c(grid(rng)))
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
    let mut out: Vec<String> = canon(&real_partition(doc, n))
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
    live.extend(
        n.by_name
            .iter()
            .map(|&(k, b)| format!("by_name {} {}", name(k), name(n.bit_name[b as usize]))),
    );
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
    let (mut p, hs) = project(&[("dev.shprim", common::DEV), ("cell.shcomp", &cell)]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let mut patched = 0;
    for step in 0..steps {
        let ops = if rng.chance(20) {
            let Some(ops) = revert(&mut p, h, &mut rng) else {
                continue;
            };
            ops
        } else {
            let tx = Transaction {
                label: "g".into(),
                mode: Mode::Commit,
                ops: geo_ops(&mut rng, p.doc(h).unwrap()),
            };
            let done = edit(&mut p, h, Edit::Tx(tx));
            // A rejected transaction still permutes rows (its replayed inverses
            // re-insert deletions at the end): the next step must cope.
            if !matches!(done.outcome, Outcome::Committed { .. }) {
                continue;
            }
            done.ops
        };
        let (before, pass) = (st.violations.rows.clone(), st.pass);
        let done = st.recompute(&p, h, &ops, &mut no_funcs);
        patched += usize::from(done.patched);
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        let doc = p.doc(h).unwrap();
        assert_eq!(
            nets_view(&st, doc),
            nets_view(&fresh, doc),
            "seed {seed} step {step} ops {ops:?}"
        );
        assert_eq!(snapshot(&st), snapshot(&fresh), "seed {seed} step {step}");
        if st.prev_pass == 0 {
            continue;
        }
        assert_eq!(st.prev_pass, pass, "passes chain");
        let mut rows = before;
        for g in &st.changes.0 {
            let at = rows
                .iter()
                .position(|r| r == g)
                .expect("a removed row existed");
            rows.remove(at);
        }
        rows.extend(st.changes.1.iter().cloned());
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
                id((side / 1000 + k) as u64 + 10),
                c(x),
                c(y),
                c(x),
                c(y + 10_000)
            ));
            if k % 4 == 0 {
                s.push_str(&format!(
                    "label {} {} {} R0 n{k}\n",
                    id((side / 1000 + k) as u64 + 5000),
                    c(x),
                    c(y)
                ));
            }
        }
    }
    // The port sits apart: on a labelled wire it would short two names (E003, a rebuild).
    s.push_str(&format!("port {} {} 0 R0 P\n", id(9_999), c(-100_000)));
    let (mut p, hs) = project(&[("dev.shprim", common::DEV), ("cell.shcomp", &s)]);
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
    let right = |k: i64| id((1_000_000 / 1000 + k) as u64 + 10);
    let edits = [
        // Join two right-hand nets, rename a right-hand label to a left-hand name (merge by
        // name), delete a right-hand wire, insert one.
        vec![Op::Insert {
            rec: RecordBuf::new(RecordKind::Wire)
                .field("-")
                .field(&c(1_000_000))
                .field(&c(10_000))
                .field(&c(1_040_000))
                .field(&c(10_000)),
            at: None,
        }],
        vec![Op::SetField {
            target: Target::new(RecordKind::Label, &id(1_000 + 4 + 5000)),
            index: 4,
            value: "n0".into(),
        }],
        vec![Op::Delete(Target::new(RecordKind::Wire, &right(7)))],
    ];
    for ops in edits {
        assert!(commit(&mut p, h, ops.clone()), "{ops:?}");
        let done = st.recompute(&p, h, &ops, &mut no_funcs);
        assert!(done.patched, "{ops:?}");
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
