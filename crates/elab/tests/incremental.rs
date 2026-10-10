//! D19 oracle (TESTING.md `expr`/`elab`): after any sequence of edits committed with
//! `doc::apply_edits`, the incrementally recomputed state equals a state built from
//! scratch — values, unverified flags, nets and violations.

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
    let out = apply_edits(
        p,
        &[EditRequest {
            doc: h,
            edit: Edit::Tx(tx),
            unix_ms: 1,
        }],
    );
    matches!(out[0], Outcome::Committed { .. })
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
    let mut incremental = 0;
    for seed in 0..12u64 {
        let n = 24;
        let (mut p, hs) = project(&[("dev.shprim", DEV), ("cell.shcomp", &cell(n))]);
        let h = hs[1];
        let mut st = CellState::build(&p, h, &mut no_funcs).unwrap();
        let mut rng = Mix(seed);
        for step in 0..60 {
            let ops: Vec<Op> = (0..1 + rng.below(3))
                .map(|_| random_op(&mut rng, n))
                .collect();
            if !commit(&mut p, h, ops.clone()) {
                continue;
            }
            let done = st.recompute(&p, h, &ops, &mut no_funcs);
            if done.evaluated > 0 {
                incremental += 1;
            }
            let fresh = CellState::build(&p, h, &mut no_funcs).unwrap();
            assert_eq!(
                snapshot(&st),
                snapshot(&fresh),
                "seed {seed} step {step} ops {ops:?}"
            );
        }
    }
    assert!(
        incremental > 50,
        "the incremental path ran {incremental} times"
    );
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
    let mut funcs = |name: &str, args: &[schemify_expr::Value<'_>]| {
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
