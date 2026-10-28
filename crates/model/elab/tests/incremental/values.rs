//! Value edits: random op sequences, overrides, renames, value edits and D20 cached
//! write-backs, each against a fresh build.

use schemify_doc::{Edit, Mode, Op, Outcome, RecordBuf, Target, Transaction};
use schemify_elab::{CellState, NetChange, no_funcs};
use schemify_format::RecordKind;

use super::{DEV, cell, commit, edit, node_vals, revalued, revert, snapshot};
use crate::common::{Mix, c, id, project};

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
    if rng.chance(15) {
        // A rename: unique, a duplicate (E009), a bus (more elements), back to its own.
        let i = rng.below(n as u64);
        let name = match rng.below(4) {
            0 => format!("MX{}", rng.below(3)),
            1 => "M3".to_owned(),
            2 => "MB[1:0]".to_owned(),
            _ => format!("M{i}"),
        };
        return Op::SetAttr {
            target: Target::new(RecordKind::Inst, &id(i + 1)),
            key: "name".into(),
            values: vec![name],
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

/// An instance rename that keeps its element count patches: element names and the
/// duplicate-name rows (`E009`) of the names it left and took change, every net keeps its
/// handle, and the state equals a fresh build. Negative space: a rename to a bus (more
/// elements) rebuilds the connectivity.
#[test]
fn a_rename_patches_element_names_and_duplicates() {
    let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &cell(12))]);
    let h = hs[1];
    let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
    let e009 = |st: &CellState| st.violations.with(schemify_elab::Code::E009).count();
    use NetChange::{Patched, Rebuilt};
    for (name, want, dups) in [
        ("M3", Patched, 1),
        ("MX", Patched, 0),
        ("MB[1:0]", Rebuilt, 0),
    ] {
        let op = Op::SetAttr {
            target: Target::new(RecordKind::Inst, &id(6)),
            key: "name".into(),
            values: vec![name.into()],
        };
        let nets = st.nets.net_name.clone();
        assert!(commit(&mut p, h, vec![op.clone()]));
        let done = st.recompute(&p, h, std::slice::from_ref(&op), &mut no_funcs);
        assert_eq!(done.nets, want, "{name}");
        assert!(!done.graph, "{name}: {done:?}");
        if want == Patched {
            assert_eq!(st.nets.net_name, nets, "{name}: every net kept");
        }
        assert_eq!(e009(&st), dups, "{name}");
        let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
        assert_eq!(snapshot(&st), snapshot(&fresh), "{name}");
    }
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
    assert!(!done.graph && done.nets == NetChange::Unchanged);
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
        assert!(!done.graph && done.nets == NetChange::Unchanged, "{done:?}");
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
        assert!(!done.graph && done.nets == NetChange::Unchanged, "{done:?}");
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
